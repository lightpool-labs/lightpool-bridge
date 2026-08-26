// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use log::{info, warn};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

use crate::route_config::{BridgeRoute, ForeignLeg};
use crate::router::{BridgeRouter, RouteState};

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

pub fn evm_ws_url(http_rpc: &str) -> String {
    let trimmed = http_rpc.trim();
    if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        return trimmed.to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("https://") {
        let ws_host = rest.replacen(":8545", ":8546", 1);
        return format!("wss://{ws_host}");
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        let ws_host = rest.replacen(":8545", ":8546", 1);
        return format!("ws://{ws_host}");
    }
    format!("ws://{trimmed}")
}

pub(crate) async fn run_deposit_subscriber(
    router: Arc<BridgeRouter>,
    state: Arc<RouteState>,
    route_def: BridgeRoute,
    cancel: CancellationToken,
) {
    let route_id = state.route_id.clone();
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match subscribe_loop(&router, &state, &route_def).await {
            Ok(()) => info!("Route {} EVM log subscriber stopped", route_id),
            Err(err) => warn!(
                "Route {} EVM WebSocket error: {err}; reconnecting in 3s",
                route_id
            ),
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
        }
    }
}

async fn subscribe_loop(
    router: &BridgeRouter,
    state: &RouteState,
    route_def: &BridgeRoute,
) -> anyhow::Result<()> {
    let ForeignLeg::Evm {
        rpc_url,
        bridge_address,
        confirmations,
        start_block,
        ..
    } = &route_def.foreign
    else {
        return Ok(());
    };

    let ws_url = evm_ws_url(rpc_url);
    info!(
        "Route {} subscribing to EVM deposit logs via WebSocket {}",
        state.route_id, ws_url
    );

    let (ws, _) = connect_async(&ws_url).await?;
    let (mut write, mut read) = ws.split();
    let mut next_id = 1u64;
    let mut pending: HashMap<u64, String> = HashMap::new();

    let latest = ws_call(
        &mut write,
        &mut read,
        &mut next_id,
        &mut pending,
        "eth_blockNumber",
        json!([]),
    )
    .await?;
    let latest = parse_hex_u64(latest.as_str().unwrap_or("0x0"))?;
    let safe = latest.saturating_sub(*confirmations);

    let mut catchup_from = state.last_scanned_block.load(Ordering::Relaxed);
    if catchup_from == 0 {
        catchup_from = *start_block;
    } else {
        catchup_from = catchup_from.saturating_add(1);
    }

    if catchup_from <= safe {
        let logs = ws_call(
            &mut write,
            &mut read,
            &mut next_id,
            &mut pending,
            "eth_getLogs",
            json!([{
                "fromBlock": format!("0x{catchup_from:x}"),
                "toBlock": format!("0x{safe:x}"),
                "address": bridge_address,
                "topics": [router.deposit_topic()],
            }]),
        )
        .await?;
        if let Some(items) = logs.as_array() {
            for log in items {
                router
                    .on_evm_deposit_log(state, route_def, log.clone())
                    .await?;
                if let Some(block) = log_block_number(log) {
                    state
                        .last_scanned_block
                        .fetch_max(block, Ordering::Relaxed);
                }
            }
        }
    }

    let logs_sub = ws_call(
        &mut write,
        &mut read,
        &mut next_id,
        &mut pending,
        "eth_subscribe",
        json!(["logs", {
            "address": bridge_address,
            "topics": [router.deposit_topic()],
        }]),
    )
    .await?;
    let logs_sub = logs_sub
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("eth_subscribe logs missing subscription id"))?
        .to_string();

    let heads_sub = ws_call(
        &mut write,
        &mut read,
        &mut next_id,
        &mut pending,
        "eth_subscribe",
        json!(["newHeads"]),
    )
    .await?;
    let heads_sub = heads_sub
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("eth_subscribe newHeads missing subscription id"))?
        .to_string();

    let mut latest_head = latest;
    let mut queued: VecDeque<(Value, u64)> = VecDeque::new();

    while let Some(msg) = read.next().await {
        let msg = msg?;
        match msg {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)?;
                if value.get("method").and_then(|v| v.as_str()) != Some("eth_subscription") {
                    continue;
                }
                let Some(params) = value.get("params") else {
                    continue;
                };
                let sub_id = params
                    .get("subscription")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let Some(result) = params.get("result").cloned() else {
                    continue;
                };

                if sub_id == heads_sub {
                    if let Some(number) = result.get("number").and_then(|v| v.as_str()) {
                        latest_head = parse_hex_u64(number)?;
                    }
                } else if sub_id == logs_sub {
                    let block = log_block_number(&result).unwrap_or(latest_head);
                    queued.push_back((result, block));
                }

                while let Some((log, block)) = queued.front().cloned() {
                    if latest_head < block.saturating_add(*confirmations) {
                        break;
                    }
                    queued.pop_front();
                    router.on_evm_deposit_log(state, route_def, log).await?;
                    state.last_scanned_block.fetch_max(block, Ordering::Relaxed);
                }
            }
            Message::Ping(payload) => {
                write.send(Message::Pong(payload)).await?;
            }
            Message::Close(_) => anyhow::bail!("EVM WebSocket closed"),
            _ => {}
        }
    }

    Ok(())
}

async fn ws_call(
    write: &mut futures_util::stream::SplitSink<WsStream, Message>,
    read: &mut futures_util::stream::SplitStream<WsStream>,
    next_id: &mut u64,
    pending: &mut HashMap<u64, String>,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    let id = *next_id;
    *next_id += 1;
    pending.insert(id, method.to_string());
    let req = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    write
        .send(Message::Text(req.to_string()))
        .await?;
    wait_for_result(read, pending).await
}

async fn wait_for_result(
    read: &mut futures_util::stream::SplitStream<WsStream>,
    pending: &mut HashMap<u64, String>,
) -> anyhow::Result<Value> {
    while let Some(msg) = read.next().await {
        let msg = msg?;
        let text = match msg {
            Message::Text(text) => text,
            Message::Close(_) => anyhow::bail!("EVM WebSocket closed while waiting for RPC result"),
            _ => continue,
        };
        let value: Value = serde_json::from_str(&text)?;
        if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
            pending.remove(&id);
            if let Some(err) = value.get("error") {
                return Err(anyhow::anyhow!("evm ws rpc error: {err}"));
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("evm ws rpc missing result"));
        }
    }
    anyhow::bail!("EVM WebSocket closed before RPC result")
}

fn log_block_number(log: &Value) -> Option<u64> {
    let hex = log.get("blockNumber")?.as_str()?;
    parse_hex_u64(hex).ok()
}

fn parse_hex_u64(hex: &str) -> anyhow::Result<u64> {
    Ok(u64::from_str_radix(hex.trim_start_matches("0x"), 16)?)
}

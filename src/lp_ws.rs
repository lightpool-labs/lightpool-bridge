// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use lightpool_types::contract::ContractAddress;
use lightpool_types::effects::{EventData, EventType, TransactionEvent};
use lightpool_types::module_types::bridge::{
    BridgeWithdrawRecord, BridgeWithdrawStatus, OutboundWithdrawRecord, OutboundWithdrawStatus,
};
use lightpool_types::ReceiptBlock;
use log::{info, warn};
use serde::Deserialize;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::lp::LightpoolClient;
use crate::route_config::{BridgeRoute, ForeignLeg};
use crate::router::{BridgeRouter, RouteState};
use crate::util::parse_contract_address;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Deserialize)]
struct OutboundWithdrawEventPayload {
    bridge: ContractAddress,
    lane_index: u32,
    nonce: u64,
    sender: lightpool_types::Address,
    foreign_recipient: [u8; 20],
    amount: u64,
    token: ContractAddress,
}

#[derive(Debug, Clone, Deserialize)]
struct InboundWithdrawEventPayload {
    lane_index: u32,
    nonce: u64,
    sender: lightpool_types::Address,
    foreign_recipient: [u8; 20],
    amount: u64,
    token: ContractAddress,
}

pub fn lp_ws_url(http_rpc: &str) -> anyhow::Result<String> {
    let mut url = Url::parse(http_rpc.trim())?;
    url.set_scheme("ws")
        .map_err(|_| anyhow::anyhow!("invalid RPC URL scheme: {http_rpc}"))?;
    if let Some(port) = url.port() {
        url.set_port(Some(port + 100))
            .map_err(|_| anyhow::anyhow!("failed to derive WebSocket port from {http_rpc}"))?;
    }
    Ok(url.to_string())
}

pub(crate) async fn run_foreign_withdraw_subscriber(
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
        match foreign_subscribe_loop(&router, &state, &route_def).await {
            Ok(()) => info!("Route {} LP foreign WebSocket subscriber stopped", route_id),
            Err(err) => warn!(
                "Route {} LP foreign WebSocket error: {err}; reconnecting in 3s",
                route_id
            ),
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
        }
    }
}

pub(crate) async fn run_local_withdraw_subscriber(
    router: Arc<BridgeRouter>,
    state: Arc<RouteState>,
    route_def: BridgeRoute,
    local_rpc_url: String,
    cancel: CancellationToken,
) {
    let route_id = state.route_id.clone();
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match local_subscribe_loop(&router, &state, &route_def, &local_rpc_url).await {
            Ok(()) => info!("Route {} local LP WebSocket subscriber stopped", route_id),
            Err(err) => warn!(
                "Route {} local LP WebSocket error: {err}; reconnecting in 3s",
                route_id
            ),
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
        }
    }
}

async fn foreign_subscribe_loop(
    router: &BridgeRouter,
    state: &RouteState,
    route_def: &BridgeRoute,
) -> anyhow::Result<()> {
    let ForeignLeg::Lightpool {
        rpc_url,
        outbound_bridge_contract,
        ..
    } = &route_def.foreign
    else {
        return Ok(());
    };

    let ws_url = lp_ws_url(rpc_url)?;
    info!(
        "Route {} subscribing to foreign outbound withdraws via WebSocket {}",
        state.route_id, ws_url
    );

    let outbound = parse_contract_address(outbound_bridge_contract)?;
    let mut ws = connect_ws(&ws_url).await?;
    subscribe_receipt_blocks(&mut ws).await?;

    while let Some(msg) = ws.next().await {
        let msg = msg?;
        match msg {
            Message::Text(text) => {
                if let Some(block) = parse_receipt_block_notification(&text) {
                    for withdraw in extract_outbound_withdraws(&block, outbound) {
                        if let Err(err) = router
                            .on_lp_foreign_withdraw(state, route_def, withdraw)
                            .await
                        {
                            warn!(
                                "Route {} foreign withdraw handling failed: {err}",
                                state.route_id
                            );
                        }
                    }
                }
            }
            Message::Ping(payload) => {
                ws.send(Message::Pong(payload)).await?;
            }
            Message::Close(_) => anyhow::bail!("LightPool WebSocket closed"),
            _ => {}
        }
    }

    Ok(())
}

async fn local_subscribe_loop(
    router: &Arc<BridgeRouter>,
    state: &Arc<RouteState>,
    route_def: &BridgeRoute,
    local_rpc_url: &str,
) -> anyhow::Result<()> {
    let ws_url = lp_ws_url(local_rpc_url)?;
    let inbound = parse_contract_address(&route_def.local_inbound.bridge_contract)?;
    info!(
        "Route {} subscribing to local inbound withdraws via WebSocket {}",
        state.route_id, ws_url
    );

    catch_up_local_withdraws(router, state, route_def, local_rpc_url).await;

    let mut ws = connect_ws(&ws_url).await?;
    subscribe_receipt_blocks(&mut ws).await?;

    while let Some(msg) = ws.next().await {
        let msg = msg?;
        match msg {
            Message::Text(text) => {
                if let Some(block) = parse_receipt_block_notification(&text) {
                    for withdraw in extract_inbound_withdraws(&block, inbound) {
                        if let Err(err) = router
                            .on_lp_local_withdraw(state, route_def, withdraw)
                            .await
                        {
                            warn!(
                                "Route {} local withdraw handling failed: {err}",
                                state.route_id
                            );
                        }
                    }
                }
            }
            Message::Ping(payload) => {
                ws.send(Message::Pong(payload)).await?;
            }
            Message::Close(_) => anyhow::bail!("LightPool WebSocket closed"),
            _ => {}
        }
    }

    Ok(())
}

async fn catch_up_local_withdraws(
    router: &Arc<BridgeRouter>,
    state: &Arc<RouteState>,
    route_def: &BridgeRoute,
    local_rpc_url: &str,
) {
    if !matches!(route_def.foreign, ForeignLeg::Lightpool { .. }) {
        return;
    }
    let Ok(inbound) = parse_contract_address(&route_def.local_inbound.bridge_contract) else {
        return;
    };
    let client = LightpoolClient::new(local_rpc_url);
    let Ok(cfg) = client.fetch_bridge_config(inbound).await else {
        return;
    };
    for lane in &cfg.lanes {
        for nonce in 1..lane.next_withdraw_nonce {
            let Ok(record) = client
                .fetch_withdraw_record(inbound, lane.lane_index, nonce)
                .await
            else {
                continue;
            };
            if record.status != BridgeWithdrawStatus::Pending {
                continue;
            }
            if let Err(err) = router.on_lp_local_withdraw(state, route_def, record).await {
                warn!(
                    "Route {} catch-up withdraw lane={} nonce={}: {err}",
                    state.route_id, lane.lane_index, nonce
                );
            }
        }
    }
}

async fn connect_ws(ws_url: &str) -> anyhow::Result<WsStream> {
    let url = Url::parse(ws_url)?;
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", "jsonrpc".parse().unwrap());
    let (ws, _) = connect_async(request)
        .await
        .map_err(|err| anyhow::anyhow!("WebSocket connect to {ws_url}: {err}"))?;
    Ok(ws)
}

async fn subscribe_receipt_blocks(ws: &mut WsStream) -> anyhow::Result<()> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "subscribe",
        "params": "ReceiptBlocks"
    });
    ws.send(Message::Text(request.to_string())).await?;
    Ok(())
}

fn parse_receipt_block_notification(text: &str) -> Option<ReceiptBlock> {
    let value: Value = serde_json::from_str(text).ok()?;
    if let Some(data) = value
        .get("params")
        .and_then(|params| params.get("result"))
        .and_then(|result| result.get("data"))
    {
        if let Ok(message) = serde_json::from_value::<WsMessagePayload>(data.clone()) {
            return message.receipt_block();
        }
    }
    None
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum WsMessagePayload {
    Wrapped { ReceiptBlock: ReceiptBlock },
    Enum(MessageEnum),
}

#[derive(Debug, Deserialize)]
enum MessageEnum {
    ReceiptBlock(ReceiptBlock),
}

impl WsMessagePayload {
    fn receipt_block(self) -> Option<ReceiptBlock> {
        match self {
            Self::Wrapped { ReceiptBlock } => Some(ReceiptBlock),
            Self::Enum(MessageEnum::ReceiptBlock(block)) => Some(block),
        }
    }
}

fn extract_outbound_withdraws(
    block: &ReceiptBlock,
    outbound_bridge: ContractAddress,
) -> Vec<OutboundWithdrawRecord> {
    let mut records = Vec::new();
    for tx in &block.transaction_outputs {
        if !tx.is_success() {
            continue;
        }
        for event in &tx.receipt.events {
            if let Some(record) = parse_outbound_withdraw_event(event, outbound_bridge) {
                records.push(record);
            }
        }
    }
    records
}

fn extract_inbound_withdraws(
    block: &ReceiptBlock,
    inbound_bridge: ContractAddress,
) -> Vec<BridgeWithdrawRecord> {
    let mut records = Vec::new();
    for tx in &block.transaction_outputs {
        if !tx.is_success() {
            continue;
        }
        for event in &tx.receipt.events {
            if let Some(record) = parse_inbound_withdraw_event(event, inbound_bridge) {
                records.push(record);
            }
        }
    }
    records
}

fn parse_outbound_withdraw_event(
    event: &TransactionEvent,
    outbound_bridge: ContractAddress,
) -> Option<OutboundWithdrawRecord> {
    let EventType::Call(action) = &event.event_type else {
        return None;
    };
    if action != "withdraw" {
        return None;
    }
    if event.contract != Some(outbound_bridge) {
        return None;
    }
    let EventData::Bytes(bytes) = &event.data else {
        return None;
    };
    let payload: OutboundWithdrawEventPayload = bincode::deserialize(bytes).ok()?;
    Some(OutboundWithdrawRecord {
        lane_index: payload.lane_index,
        nonce: payload.nonce,
        sender: payload.sender,
        foreign_recipient: payload.foreign_recipient,
        amount: payload.amount,
        token: payload.token,
        status: OutboundWithdrawStatus::Pending,
    })
}

fn parse_inbound_withdraw_event(
    event: &TransactionEvent,
    inbound_bridge: ContractAddress,
) -> Option<BridgeWithdrawRecord> {
    let EventType::Call(action) = &event.event_type else {
        return None;
    };
    if action != "withdraw" {
        return None;
    }
    if event.contract != Some(inbound_bridge) {
        return None;
    }
    let EventData::Bytes(bytes) = &event.data else {
        return None;
    };
    let payload: InboundWithdrawEventPayload = bincode::deserialize(bytes).ok()?;
    Some(BridgeWithdrawRecord {
        lane_index: payload.lane_index,
        nonce: payload.nonce,
        sender: payload.sender,
        foreign_recipient: payload.foreign_recipient,
        amount: payload.amount,
        token: payload.token,
        status: BridgeWithdrawStatus::Pending,
    })
}

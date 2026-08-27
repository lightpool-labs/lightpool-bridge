// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;

const MAX_EVENTS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeEventLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeEventKind {
    DepositSeen,
    ConfirmDepSubmitted,
    ConfirmDepOk,
    ConfirmDepFailed,
    WithdrawSeen,
    EvmRequestSent,
    EvmRequestFailed,
    EvmFinalized,
    EvmFinalizeFailed,
    DepositSubmitted,
    DepositOk,
    DepositFailed,
    ConfigUpdated,
    RouteReloaded,
}

#[derive(Debug, Clone, Serialize)]
pub struct BridgeEventRecord {
    pub id: u64,
    pub ts_ms: u64,
    pub route_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub kind: BridgeEventKind,
    pub level: BridgeEventLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,
    pub detail: String,
}

pub struct EventStore {
    next_id: AtomicU64,
    events: RwLock<VecDeque<BridgeEventRecord>>,
}

impl EventStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            next_id: AtomicU64::new(1),
            events: RwLock::new(VecDeque::with_capacity(MAX_EVENTS)),
        })
    }

    pub async fn emit(
        &self,
        route_id: impl Into<String>,
        token: Option<String>,
        kind: BridgeEventKind,
        level: BridgeEventLevel,
        message_id: Option<u64>,
        amount: Option<u64>,
        detail: impl Into<String>,
    ) {
        let mut event = BridgeEventRecord {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            ts_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            route_id: route_id.into(),
            token,
            kind,
            level,
            message_id,
            amount,
            detail: detail.into(),
        };
        let mut guard = self.events.write().await;
        guard.push_back(event);
        while guard.len() > MAX_EVENTS {
            guard.pop_front();
        }
    }

    pub async fn list(
        &self,
        route_id: Option<&str>,
        token: Option<&str>,
        limit: usize,
    ) -> Vec<BridgeEventRecord> {
        let guard = self.events.read().await;
        let limit = limit.clamp(1, 500);
        guard
            .iter()
            .rev()
            .filter(|e| route_id.map(|r| e.route_id == r).unwrap_or(true))
            .filter(|e| token.map(|t| e.token.as_deref() == Some(t)).unwrap_or(true))
            .take(limit)
            .cloned()
            .collect()
    }
}

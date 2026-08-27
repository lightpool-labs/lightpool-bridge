// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::path::PathBuf;
use std::sync::Arc;

use lightpool_crypto::{PublicKey, SecretKey};
use lightpool_types::Committee;
use serde::Serialize;

use crate::config::{BridgeConfigError, BridgeLinkConfig};
use crate::events::{BridgeEventRecord, EventsPage};
use crate::router::BridgeRouter;

#[derive(Clone)]
pub struct BridgeHandle {
    inner: Arc<BridgeRouter>,
}

impl BridgeHandle {
    pub fn new(
        config_path: PathBuf,
        config: BridgeLinkConfig,
        name: PublicKey,
        secret_key: SecretKey,
        committee: Committee,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Arc::new(BridgeRouter::new(
                config_path,
                config,
                name,
                secret_key,
                committee,
            )?),
        })
    }

    pub fn router(&self) -> Arc<BridgeRouter> {
        self.inner.clone()
    }

    pub fn config_path(&self) -> PathBuf {
        self.inner.config_path.clone()
    }

    pub async fn config(&self) -> BridgeLinkConfig {
        self.inner.config_snapshot().await
    }

    pub async fn update_config(
        &self,
        config: BridgeLinkConfig,
    ) -> Result<BridgeLinkConfig, BridgeConfigError> {
        let config = self.inner.update_config(config).await?;
        self.inner.sync_evm_subscribers().await;
        self.inner.sync_lp_foreign_subscribers().await;
        self.inner.sync_lp_local_subscribers().await;
        Ok(config)
    }

    pub async fn status(&self) -> BridgeStatusResponse {
        self.inner.status_snapshot().await
    }

    pub async fn events_page(
        &self,
        route_id: Option<&str>,
        token: Option<&str>,
        page: u32,
        page_size: u32,
    ) -> EventsPage {
        self.inner
            .events
            .list_page(route_id, token, page, page_size)
            .await
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<BridgeEventRecord> {
        self.inner.events.subscribe()
    }
}

#[derive(Debug, Serialize)]
pub struct BridgeStatusResponse {
    pub running: bool,
    pub config_path: String,
    pub validator: String,
    pub committee_epoch: u128,
    pub committee_size: usize,
    pub route_count: usize,
    pub enabled_route_count: usize,
    pub routes: Vec<RouteStatusSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct RouteStatusSnapshot {
    pub id: String,
    pub enabled: bool,
    pub foreign_kind: String,
    pub inbound_contract: String,
    pub lp_token: String,
    pub config_loaded: bool,
    pub lp_token_on_chain: Option<String>,
    pub evm_token_on_chain: Option<String>,
    pub next_withdraw_nonce: Option<u64>,
    pub last_scanned_block: u64,
    pub last_withdraw_nonce_scanned: u64,
    pub last_foreign_withdraw_nonce_scanned: u64,
    pub pending_deposits: u64,
    pub pending_evm_withdraws: u64,
    pub pending_lp_deposits: u64,
    pub seen_deposits: u64,
}

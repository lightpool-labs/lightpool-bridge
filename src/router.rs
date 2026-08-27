// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lightpool_crypto::{Keccak256, PublicKey, SecretKey};
use lightpool_types::address_type::Address;
use lightpool_types::contract::ContractAddress;
use lightpool_types::module::Module;
use lightpool_types::module_types::bridge::{
    BridgeConfig, BridgeDepositMessage, BridgeVote, BridgeWithdrawRecord, BridgeWithdrawStatus,
    OutboundWithdrawStatus, OutboundDepositMessage,
};
use lightpool_types::Committee;
use log::{debug, info, warn};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::actions::{CONFIRM_DEP_ACTION, DEPOSIT_ACTION};
use crate::config::{BridgeConfigError, BridgeLinkConfig};
use crate::events::{BridgeEventKind, BridgeEventLevel, EventStore};
use crate::handle::RouteStatusSnapshot;
use crate::evm::{
    cast_finalize_withdraw, cast_request_withdraw, eth_sign_digest, request_withdraw_digest,
    secret_key_hex, withdraw_id, EthSignature, EvmCommittee,
};
use crate::lp::LightpoolClient;
use crate::messages::{BridgeLinkVote, BridgeVoteKind};
use crate::route_config::{BridgeRoute, ForeignLeg};
use crate::util::{
    address_from_word, evm_address_hex, lock_source_hash, parse_b32, parse_contract_address,
    parse_evm_address,
    topic_address, topic_u64, u64_from_word,
};

#[derive(Debug, Serialize)]
struct ConfirmDepositParams {
    message: BridgeDepositMessage,
    votes: Vec<BridgeVote>,
}

#[derive(Debug, Serialize)]
struct DepositParams {
    message: OutboundDepositMessage,
    votes: Vec<BridgeVote>,
}

#[derive(Debug)]
struct VoteBucket {
    route_id: String,
    inbound_contract: ContractAddress,
    message: Option<BridgeDepositMessage>,
    votes: HashMap<PublicKey, BridgeLinkVote>,
    submitted: bool,
    submitting: bool,
}

#[derive(Debug, Clone)]
struct PendingEvmWithdraw {
    record: BridgeWithdrawRecord,
    id: [u8; 32],
    signature: EthSignature,
    requested: bool,
    requested_at: Option<std::time::Instant>,
    finalized: bool,
    failed: bool,
}

#[derive(Debug, Clone)]
struct PendingLpDeposit {
    message: OutboundDepositMessage,
    votes: Vec<BridgeVote>,
    submitted: bool,
    failed: bool,
}

pub(crate) struct RouteState {
    pub(crate) route_id: String,
    pub(crate) last_scanned_block: AtomicU64,
    last_withdraw_nonce: AtomicU64,
    last_foreign_withdraw_nonce: AtomicU64,
    seen_deposits: RwLock<HashSet<u64>>,
    bridge_config: RwLock<Option<BridgeConfig>>,
    pending_evm_withdraws: RwLock<HashMap<u64, PendingEvmWithdraw>>,
    pending_lp_deposits: RwLock<HashMap<u64, PendingLpDeposit>>,
}

pub struct BridgeRouter {
    pub config_path: PathBuf,
    config: Arc<RwLock<BridgeLinkConfig>>,
    name: PublicKey,
    secret_key: SecretKey,
    local: Arc<RwLock<LightpoolClient>>,
    committee: Arc<RwLock<Committee>>,
    pending: Arc<RwLock<HashMap<(String, BridgeVoteKind, u64), VoteBucket>>>,
    routes: Arc<RwLock<Vec<Arc<RouteState>>>>,
    evm_subscriber_tokens: Arc<RwLock<HashMap<String, CancellationToken>>>,
    lp_foreign_subscriber_tokens: Arc<RwLock<HashMap<String, CancellationToken>>>,
    lp_local_subscriber_tokens: Arc<RwLock<HashMap<String, CancellationToken>>>,
    pub events: Arc<EventStore>,
    deposit_topic: String,
}

impl BridgeRouter {
    pub fn new(
        config_path: PathBuf,
        config: BridgeLinkConfig,
        name: PublicKey,
        secret_key: SecretKey,
        committee: Committee,
    ) -> anyhow::Result<Self> {
        let deposit_topic = format!(
            "0x{}",
            hex::encode(Keccak256::digest(
                b"DepositInitiated(uint64,address,address,address,uint64,uint64)"
            ))
        );
        let local = Arc::new(RwLock::new(LightpoolClient::new(config.local.rpc_url.clone())));
        let events = EventStore::new();
        let routes = Arc::new(RwLock::new(Self::build_route_states(&config, &[])));
        Ok(Self {
            config_path,
            config: Arc::new(RwLock::new(config)),
            name,
            secret_key,
            local,
            committee: Arc::new(RwLock::new(committee)),
            pending: Arc::new(RwLock::new(HashMap::new())),
            routes,
            evm_subscriber_tokens: Arc::new(RwLock::new(HashMap::new())),
            lp_foreign_subscriber_tokens: Arc::new(RwLock::new(HashMap::new())),
            lp_local_subscriber_tokens: Arc::new(RwLock::new(HashMap::new())),
            events,
            deposit_topic,
        })
    }

    pub(crate) fn deposit_topic(&self) -> &str {
        &self.deposit_topic
    }

    pub(crate) async fn on_evm_deposit_log(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        log: Value,
    ) -> anyhow::Result<()> {
        self.prepare_route(state, route_def).await;
        let bridge_cfg = self.require_route_config(state).await?;
        let ForeignLeg::Evm {
            token_address,
            chain_id,
            ..
        } = &route_def.foreign
        else {
            return Ok(());
        };
        let expected_token = if token_address.trim().is_empty() {
            bridge_cfg.foreign_token
        } else {
            parse_evm_address(token_address)?
        };
        self.handle_evm_deposit_log(
            state,
            route_def,
            &log,
            &bridge_cfg,
            expected_token,
            *chain_id,
        )
        .await
    }

    pub(crate) async fn sync_evm_subscribers(self: &Arc<Self>) {
        let config = self.config.read().await;
        let states = self.routes.read().await.clone();
        let desired: Vec<(String, BridgeRoute)> = config
            .routes
            .iter()
            .filter(|r| r.enabled && matches!(r.foreign, ForeignLeg::Evm { .. }))
            .map(|r| (r.id.clone(), r.clone()))
            .collect();
        drop(config);

        let desired_ids: HashSet<String> = desired.iter().map(|(id, _)| id.clone()).collect();
        let mut tokens = self.evm_subscriber_tokens.write().await;

        for id in tokens.keys().cloned().collect::<Vec<_>>() {
            if !desired_ids.contains(&id) {
                if let Some(token) = tokens.remove(&id) {
                    token.cancel();
                }
            }
        }

        for (route_id, route_def) in desired {
            if let Some(old) = tokens.remove(&route_id) {
                old.cancel();
            }
            let Some(state) = states.iter().find(|s| s.route_id == route_id).cloned() else {
                continue;
            };
            let child = CancellationToken::new();
            tokens.insert(route_id.clone(), child.clone());
            let router = Arc::clone(self);
            tokio::spawn(crate::evm_ws::run_deposit_subscriber(
                router,
                state,
                route_def,
                child,
            ));
        }
    }

    pub(crate) async fn sync_lp_foreign_subscribers(self: &Arc<Self>) {
        let config = self.config.read().await;
        let states = self.routes.read().await.clone();
        let desired: Vec<(String, BridgeRoute)> = config
            .routes
            .iter()
            .filter(|r| r.enabled && matches!(r.foreign, ForeignLeg::Lightpool { .. }))
            .map(|r| (r.id.clone(), r.clone()))
            .collect();
        drop(config);

        let desired_ids: HashSet<String> = desired.iter().map(|(id, _)| id.clone()).collect();
        let mut tokens = self.lp_foreign_subscriber_tokens.write().await;

        for id in tokens.keys().cloned().collect::<Vec<_>>() {
            if !desired_ids.contains(&id) {
                if let Some(token) = tokens.remove(&id) {
                    token.cancel();
                }
            }
        }

        for (route_id, route_def) in desired {
            if let Some(old) = tokens.remove(&route_id) {
                old.cancel();
            }
            let Some(state) = states.iter().find(|s| s.route_id == route_id).cloned() else {
                continue;
            };
            let child = CancellationToken::new();
            tokens.insert(route_id.clone(), child.clone());
            let router = Arc::clone(self);
            tokio::spawn(crate::lp_ws::run_foreign_withdraw_subscriber(
                router,
                state,
                route_def,
                child,
            ));
        }
    }

    pub(crate) async fn sync_lp_local_subscribers(self: &Arc<Self>) {
        let config = self.config.read().await;
        let local_rpc_url = config.local.rpc_url.clone();
        let states = self.routes.read().await.clone();
        let desired: Vec<(String, BridgeRoute)> = config
            .routes
            .iter()
            .filter(|r| r.enabled)
            .map(|r| (r.id.clone(), r.clone()))
            .collect();
        drop(config);

        let desired_ids: HashSet<String> = desired.iter().map(|(id, _)| id.clone()).collect();
        let mut tokens = self.lp_local_subscriber_tokens.write().await;

        for id in tokens.keys().cloned().collect::<Vec<_>>() {
            if !desired_ids.contains(&id) {
                if let Some(token) = tokens.remove(&id) {
                    token.cancel();
                }
            }
        }

        for (route_id, route_def) in desired {
            if let Some(old) = tokens.remove(&route_id) {
                old.cancel();
            }
            let Some(state) = states.iter().find(|s| s.route_id == route_id).cloned() else {
                continue;
            };
            let child = CancellationToken::new();
            tokens.insert(route_id.clone(), child.clone());
            let router = Arc::clone(self);
            let rpc_url = local_rpc_url.clone();
            tokio::spawn(crate::lp_ws::run_local_withdraw_subscriber(
                router,
                state,
                route_def,
                rpc_url,
                child,
            ));
        }
    }

    pub(crate) async fn on_lp_local_withdraw(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        record: BridgeWithdrawRecord,
    ) -> anyhow::Result<()> {
        if record.status != BridgeWithdrawStatus::Pending {
            return Ok(());
        }
        if state
            .pending_lp_deposits
            .read()
            .await
            .get(&record.nonce)
            .is_some_and(|item| item.submitted || item.failed)
        {
            return Ok(());
        }
        self.prepare_route(state, route_def).await;
        state
            .last_withdraw_nonce
            .fetch_max(record.nonce, Ordering::Relaxed);
        let bridge_cfg = self.require_route_config(state).await?;
        self.handle_local_withdraw(state, route_def, &bridge_cfg, record)
            .await
    }

    pub(crate) async fn on_lp_foreign_withdraw(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        withdraw: lightpool_types::module_types::bridge::OutboundWithdrawRecord,
    ) -> anyhow::Result<()> {
        if withdraw.status != OutboundWithdrawStatus::Pending {
            return Ok(());
        }
        self.prepare_route(state, route_def).await;
        let bridge_cfg = self.require_route_config(state).await?;
        let ForeignLeg::Lightpool { chain_id, .. } = &route_def.foreign else {
            return Ok(());
        };
        state
            .last_foreign_withdraw_nonce
            .fetch_max(withdraw.nonce, Ordering::Relaxed);
        self.handle_foreign_withdraw(state, route_def, &bridge_cfg, *chain_id, &withdraw)
            .await
    }

    fn build_route_states(
        config: &BridgeLinkConfig,
        previous: &[Arc<RouteState>],
    ) -> Vec<Arc<RouteState>> {
        config
            .routes
            .iter()
            .filter(|r| r.enabled)
            .map(|route| {
                if let Some(existing) = previous.iter().find(|s| s.route_id == route.id) {
                    existing.clone()
                } else {
                    Arc::new(RouteState {
                        route_id: route.id.clone(),
                        last_scanned_block: AtomicU64::new(0),
                        last_withdraw_nonce: AtomicU64::new(0),
                        last_foreign_withdraw_nonce: AtomicU64::new(0),
                        seen_deposits: RwLock::new(HashSet::new()),
                        bridge_config: RwLock::new(None),
                        pending_evm_withdraws: RwLock::new(HashMap::new()),
                        pending_lp_deposits: RwLock::new(HashMap::new()),
                    })
                }
            })
            .collect()
    }

    pub async fn config_snapshot(&self) -> BridgeLinkConfig {
        self.config.read().await.clone()
    }

    pub async fn update_config(
        &self,
        mut config: BridgeLinkConfig,
    ) -> Result<BridgeLinkConfig, BridgeConfigError> {
        config.normalize_routes();
        crate::route_config::validate_config(&config).map_err(|message| {
            BridgeConfigError::Validation {
                path: self.config_path.display().to_string(),
                message,
            }
        })?;
        config.write(&self.config_path)?;
        {
            let mut guard = self.config.write().await;
            *guard = config.clone();
        }
        self.local
            .write()
            .await
            .set_rpc_url(config.local.rpc_url.clone());
        let previous = self.routes.read().await.clone();
        *self.routes.write().await = Self::build_route_states(&config, &previous);
        let enabled = config.routes.iter().filter(|r| r.enabled).count();
        self.events
            .emit(
                "global",
                None,
                BridgeEventKind::ConfigUpdated,
                BridgeEventLevel::Info,
                None,
                None,
                format!("config saved to {}", self.config_path.display()),
            )
            .await;
        self.events
            .emit(
                "global",
                None,
                BridgeEventKind::RouteReloaded,
                BridgeEventLevel::Info,
                None,
                None,
                format!("{enabled} enabled route(s) active"),
            )
            .await;
        Ok(config)
    }

    pub async fn status_snapshot(&self) -> crate::handle::BridgeStatusResponse {
        let config = self.config.read().await;
        let committee = self.committee.read().await;
        let active = self.routes.read().await.clone();
        let pending = self.pending.read().await;
        let mut routes = Vec::new();
        for route in &config.routes {
            let snapshot = if let Some(state) = active.iter().find(|s| s.route_id == route.id) {
                let bridge_cfg = state.bridge_config.read().await;
                let pending_deposits = pending
                    .iter()
                    .filter(|((rid, kind, _), b)| {
                        rid == &route.id && *kind == BridgeVoteKind::Deposit && !b.submitted
                    })
                    .count() as u64;
                let inbound = inbound_contract_for(route);
                RouteStatusSnapshot {
                    id: route.id.clone(),
                    enabled: route.enabled,
                    foreign_kind: foreign_kind_label(&route.foreign),
                    inbound_contract: format!("{inbound}"),
                    lp_token: route.local_inbound.lp_token.clone(),
                    config_loaded: bridge_cfg.is_some(),
                    lp_token_on_chain: bridge_cfg.as_ref().map(|c| format!("{}", c.token)),
                    evm_token_on_chain: bridge_cfg
                        .as_ref()
                        .map(|c| evm_address_hex(&c.foreign_token)),
                    next_withdraw_nonce: bridge_cfg.as_ref().map(|c| c.next_withdraw_nonce),
                    last_scanned_block: state.last_scanned_block.load(Ordering::Relaxed),
                    last_withdraw_nonce_scanned: state.last_withdraw_nonce.load(Ordering::Relaxed),
                    last_foreign_withdraw_nonce_scanned: state
                        .last_foreign_withdraw_nonce
                        .load(Ordering::Relaxed),
                    pending_deposits,
                    pending_evm_withdraws: state.pending_evm_withdraws.read().await.len() as u64,
                    pending_lp_deposits: state.pending_lp_deposits.read().await.len() as u64,
                    seen_deposits: state.seen_deposits.read().await.len() as u64,
                }
            } else {
                RouteStatusSnapshot {
                    id: route.id.clone(),
                    enabled: route.enabled,
                    foreign_kind: foreign_kind_label(&route.foreign),
                    inbound_contract: parse_inbound_contract(route)
                        .map(|c| format!("{c}"))
                        .unwrap_or_default(),
                    lp_token: route.local_inbound.lp_token.clone(),
                    config_loaded: false,
                    lp_token_on_chain: None,
                    evm_token_on_chain: None,
                    next_withdraw_nonce: None,
                    last_scanned_block: 0,
                    last_withdraw_nonce_scanned: 0,
                    last_foreign_withdraw_nonce_scanned: 0,
                    pending_deposits: 0,
                    pending_evm_withdraws: 0,
                    pending_lp_deposits: 0,
                    seen_deposits: 0,
                }
            };
            routes.push(snapshot);
        }
        crate::handle::BridgeStatusResponse {
            running: true,
            config_path: self.config_path.display().to_string(),
            validator: format!("{}", self.name),
            committee_epoch: committee.epoch,
            committee_size: committee.size(),
            route_count: config.routes.len(),
            enabled_route_count: config.routes.iter().filter(|r| r.enabled).count(),
            routes,
        }
    }

    fn token_label(route: &BridgeRoute, bridge_cfg: Option<&BridgeConfig>) -> Option<String> {
        if !route.local_inbound.lp_token.is_empty() {
            return Some(route.local_inbound.lp_token.clone());
        }
        bridge_cfg.map(|c| format!("{}", c.token))
    }

    fn is_evm_token_event(kind: BridgeEventKind) -> bool {
        matches!(
            kind,
            BridgeEventKind::DepositSeen
                | BridgeEventKind::EvmRequestSent
                | BridgeEventKind::EvmRequestFailed
                | BridgeEventKind::EvmFinalized
                | BridgeEventKind::EvmFinalizeFailed
        )
    }

    fn evm_token_label(route: &BridgeRoute, bridge_cfg: Option<&BridgeConfig>) -> Option<String> {
        if let ForeignLeg::Evm { token_address, .. } = &route.foreign {
            if !token_address.trim().is_empty() {
                return Some(token_address.trim().to_string());
            }
        }
        bridge_cfg.map(|c| evm_address_hex(&c.foreign_token))
    }

    async fn route_cfg(&self, state: &RouteState) -> Option<BridgeRoute> {
        let config = self.config.read().await;
        config
            .routes
            .iter()
            .find(|r| r.id == state.route_id && r.enabled)
            .cloned()
    }

    async fn emit_route(
        &self,
        route_id: &str,
        route_def: Option<&BridgeRoute>,
        bridge_cfg: Option<&BridgeConfig>,
        kind: BridgeEventKind,
        level: BridgeEventLevel,
        message_id: Option<u64>,
        amount: Option<u64>,
        detail: impl Into<String>,
    ) {
        let token = route_def.and_then(|r| {
            if Self::is_evm_token_event(kind) {
                Self::evm_token_label(r, bridge_cfg)
            } else {
                Self::token_label(r, bridge_cfg)
            }
        });
        self.events
            .emit(
                route_id,
                token,
                kind,
                level,
                message_id,
                amount,
                detail,
            )
            .await;
    }

    pub fn spawn(self: Arc<Self>, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run(cancel).await;
        })
    }

    async fn run(self: Arc<Self>, cancel: CancellationToken) {
        {
            let config = self.config.read().await;
            info!(
                "Bridge router started (local={}, routes={}/{})",
                config.local.rpc_url,
                config.routes.iter().filter(|r| r.enabled).count(),
                config.routes.len()
            );
        }
        self.sync_evm_subscribers().await;
        self.sync_lp_foreign_subscribers().await;
        self.sync_lp_local_subscribers().await;

        let _ = self.refresh_committee().await;
        for state in self.routes.read().await.iter() {
            if let Some(route_def) = self.route_cfg(state).await {
                let _ = self.refresh_route_config(state, &route_def).await;
            }
        }

        let evm_cancel = cancel.child_token();
        if self.has_enabled_evm_routes().await {
            info!(
                "EVM routes enabled: maintenance poll every {}ms (finalize withdraw only)",
                self.config.read().await.poll_interval_ms.max(100)
            );
            let router = Arc::clone(&self);
            let evm_token = evm_cancel.clone();
            tokio::spawn(async move {
                router.run_evm_maintenance(evm_token).await;
            });
        } else {
            info!("LightPool routes are WebSocket-driven; no poll loop");
        }

        cancel.cancelled().await;

        evm_cancel.cancel();
        {
            let mut evm_tokens = self.evm_subscriber_tokens.write().await;
            for (_, token) in evm_tokens.drain() {
                token.cancel();
            }
            let mut lp_foreign_tokens = self.lp_foreign_subscriber_tokens.write().await;
            for (_, token) in lp_foreign_tokens.drain() {
                token.cancel();
            }
            let mut lp_local_tokens = self.lp_local_subscriber_tokens.write().await;
            for (_, token) in lp_local_tokens.drain() {
                token.cancel();
            }
        }
        info!("Bridge router shutting down");
    }

    async fn has_enabled_evm_routes(&self) -> bool {
        self.config
            .read()
            .await
            .routes
            .iter()
            .any(|r| r.enabled && matches!(r.foreign, ForeignLeg::Evm { .. }))
    }

    async fn run_evm_maintenance(self: Arc<Self>, cancel: CancellationToken) {
        loop {
            let interval = {
                let config = self.config.read().await;
                Duration::from_millis(config.poll_interval_ms.max(100))
            };
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(interval) => {
                    if let Err(err) = self.evm_maintenance_tick().await {
                        warn!("Bridge EVM maintenance error: {}", err);
                    }
                }
            }
        }
    }

    async fn prepare_route(&self, state: &RouteState, route_def: &BridgeRoute) {
        let _ = self.refresh_committee().await;
        let _ = self.refresh_route_config(state, route_def).await;
    }

    async fn evm_maintenance_tick(&self) -> anyhow::Result<()> {
        self.refresh_committee().await?;
        let routes = self.routes.read().await.clone();
        for state in &routes {
            let Some(route_def) = self.route_cfg(state).await else {
                continue;
            };
            if !matches!(route_def.foreign, ForeignLeg::Evm { .. }) {
                continue;
            }
            self.refresh_route_config(state, &route_def).await?;
            self.try_submit_evm_withdraws(state, &route_def).await?;
        }
        Ok(())
    }

    async fn refresh_committee(&self) -> anyhow::Result<()> {
        match self.local.read().await.fetch_committee().await {
            Ok(committee) => {
                let mut guard = self.committee.write().await;
                if guard.epoch != committee.epoch || guard.size() != committee.size() {
                    info!(
                        "Bridge router loaded committee epoch={} size={}",
                        committee.epoch,
                        committee.size()
                    );
                }
                *guard = committee;
            }
            Err(err) => {
                if self.committee.read().await.size() == 0 {
                    debug!("Bridge router waiting for committee: {}", err);
                }
            }
        }
        Ok(())
    }

    async fn refresh_route_config(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
    ) -> anyhow::Result<()> {
        let contract = inbound_contract_for(route_def);
        match self.local.read().await.fetch_bridge_config(contract).await {
            Ok(cfg) => {
                let mut guard = state.bridge_config.write().await;
                if guard.is_none() {
                    info!(
                        "Route {} loaded inbound config token={} chain_id={}",
                        state.route_id, cfg.token, cfg.foreign_chain_id
                    );
                }
                *guard = Some(cfg);
            }
            Err(err) => {
                if state.bridge_config.read().await.is_none() {
                    debug!(
                        "Route {} waiting for inbound config: {}",
                        state.route_id, err
                    );
                }
            }
        }
        Ok(())
    }

    async fn require_route_config(&self, state: &RouteState) -> anyhow::Result<BridgeConfig> {
        state
            .bridge_config
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("route {} inbound config not loaded", state.route_id))
    }

    pub(crate) async fn poll_local_withdrawals(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
    ) -> anyhow::Result<()> {
        self.prepare_route(state, route_def).await;
        let bridge_cfg = match self.require_route_config(state).await {
            Ok(cfg) => cfg,
            Err(_) => return Ok(()),
        };
        let contract = inbound_contract_for(route_def);
        let next = bridge_cfg.next_withdraw_nonce;
        if next <= 1 {
            return Ok(());
        }
        let mut last = state.last_withdraw_nonce.load(Ordering::Relaxed);
        while last + 1 < next {
            let nonce = last + 1;
            match self
                .local
                .read()
                .await
                .fetch_withdraw_record(contract, nonce)
                .await
            {
                Ok(record) => {
                    if record.status == BridgeWithdrawStatus::Pending {
                        self.handle_local_withdraw(state, route_def, &bridge_cfg, record)
                            .await?;
                    }
                    last = nonce;
                    state.last_withdraw_nonce.store(last, Ordering::Relaxed);
                }
                Err(err) => {
                    debug!(
                        "Route {} withdraw {} not ready: {}",
                        state.route_id, nonce, err
                    );
                    break;
                }
            }
        }
        Ok(())
    }

    async fn handle_local_withdraw(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        bridge_cfg: &BridgeConfig,
        record: BridgeWithdrawRecord,
    ) -> anyhow::Result<()> {
        let nonce = record.nonce;
        match &route_def.foreign {
            ForeignLeg::Evm { .. } => {
                self.enqueue_evm_withdraw(state, route_def, bridge_cfg, record)
                    .await?;
                self.try_submit_evm_withdraws(state, route_def).await
            }
            ForeignLeg::Lightpool {
                rpc_url,
                chain_id,
                outbound_bridge_contract,
                ..
            } => {
                self.enqueue_lp_deposit(
                    state,
                    route_def,
                    rpc_url,
                    *chain_id,
                    outbound_bridge_contract,
                    bridge_cfg,
                    record,
                )
                .await?;
                self.try_submit_lp_deposit_nonce(state, route_def, nonce)
                    .await
            }
        }
    }

    fn already_processed_detail(detail: &str) -> bool {
        detail.to_ascii_lowercase().contains("already processed")
    }

    fn evm_withdraw_permanent_failure(detail: &str) -> bool {
        let lower = detail.to_ascii_lowercase();
        lower.contains("invalidcommittee")
            || lower.contains("invalid committee")
            || lower.contains("not in authorities")
            || lower.contains("unauthorized")
    }

    async fn enqueue_evm_withdraw(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        bridge_cfg: &BridgeConfig,
        record: BridgeWithdrawRecord,
    ) -> anyhow::Result<()> {
        let committee = self.committee.read().await;
        let epoch = if committee.size() == 0 {
            0
        } else {
            committee.epoch as u64
        };
        drop(committee);

        let mut pending = state.pending_evm_withdraws.write().await;
        if pending.contains_key(&record.nonce) {
            return Ok(());
        }
        let user = *record.sender.as_bytes();
        let id = withdraw_id(
            record.nonce,
            user,
            record.foreign_recipient,
            record.amount,
            epoch,
        );
        let digest = request_withdraw_digest(
            id,
            user,
            record.foreign_recipient,
            bridge_cfg.foreign_token,
            record.amount,
            record.nonce,
            epoch,
        );
        let signature = eth_sign_digest(digest, &self.secret_key)?;
        info!(
            "Route {} observed LP withdraw nonce={} amount={}",
            state.route_id, record.nonce, record.amount
        );
        self.emit_route(
            &state.route_id,
            Some(route_def),
            Some(bridge_cfg),
            BridgeEventKind::WithdrawSeen,
            BridgeEventLevel::Info,
            Some(record.nonce),
            Some(record.amount),
            format!("withdraw to 0x{}", hex::encode(record.foreign_recipient)),
        )
        .await;
        pending.insert(
            record.nonce,
            PendingEvmWithdraw {
                record,
                id,
                signature,
                requested: false,
                requested_at: None,
                finalized: false,
                failed: false,
            },
        );
        Ok(())
    }

    async fn enqueue_lp_deposit(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        foreign_rpc: &str,
        _foreign_chain_id: u64,
        outbound_bridge_contract: &str,
        _bridge_cfg: &BridgeConfig,
        record: BridgeWithdrawRecord,
    ) -> anyhow::Result<()> {
        let mut pending = state.pending_lp_deposits.write().await;
        if pending.contains_key(&record.nonce) {
            return Ok(());
        }
        let foreign_contract = parse_contract_address(outbound_bridge_contract)?;
        let foreign_client = LightpoolClient::new(foreign_rpc);
        let foreign_cfg = foreign_client.fetch_outbound_config(foreign_contract).await?;
        let local_chain_id = self.config.read().await.local.chain_id;
        let inbound = inbound_contract_for(route_def);
        let message = OutboundDepositMessage {
            bridge: foreign_contract,
            message_id: record.nonce,
            source_chain_id: local_chain_id,
            token: foreign_cfg.token,
            amount: record.amount,
            sender_foreign: *record.sender.as_bytes(),
            recipient: Address::new(record.foreign_recipient),
            source_tx_hash: lock_source_hash(local_chain_id, inbound, record.nonce),
            source_block: 0,
            epoch: foreign_cfg.epoch,
        };
        let vote = self.sign_outbound_deposit_vote(&message);
        info!(
            "Route {} queued foreign deposit nonce={} amount={}",
            state.route_id, record.nonce, record.amount
        );
        self.emit_route(
            &state.route_id,
            Some(route_def),
            Some(_bridge_cfg),
            BridgeEventKind::WithdrawSeen,
            BridgeEventLevel::Info,
            Some(record.nonce),
            Some(record.amount),
            "queued foreign LP deposit".to_string(),
        )
        .await;
        pending.insert(
            record.nonce,
            PendingLpDeposit {
                message,
                votes: vec![vote],
                submitted: false,
                failed: false,
            },
        );
        Ok(())
    }

    fn sign_outbound_deposit_vote(&self, message: &OutboundDepositMessage) -> BridgeVote {
        use lightpool_crypto::{Digest, Signature};
        let digest = Digest::from_data(message);
        BridgeVote {
            validator: self.name,
            signature: Signature::new(&digest, &self.secret_key),
        }
    }

    async fn is_leader(&self) -> bool {
        let committee = self.committee.read().await;
        let keys = committee.voting_public_keys();
        if keys.is_empty() {
            return true;
        }
        let dispute = self.config.read().await.dispute_period_seconds.max(1);
        let round = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / dispute) as usize;
        keys[round % keys.len()] == self.name
    }

    async fn try_submit_evm_withdraws(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
    ) -> anyhow::Result<()> {
        if !self.is_leader().await {
            return Ok(());
        }
        let ForeignLeg::Evm {
            rpc_url,
            bridge_address,
            ..
        } = &route_def.foreign
        else {
            return Ok(());
        };
        let bridge_cfg = match self.require_route_config(state).await {
            Ok(cfg) => cfg,
            Err(_) => return Ok(()),
        };
        let cfg = self.config.read().await;
        let cast_bin = cfg.cast_bin.clone();
        let dispute_secs = cfg.dispute_period_seconds.max(1) + 2;
        drop(cfg);
        let committee_guard = self.committee.read().await;
        let epoch = if committee_guard.size() == 0 {
            0
        } else {
            committee_guard.epoch as u64
        };
        let evm_committee = if committee_guard.size() == 0 {
            EvmCommittee {
                epoch: 0,
                validators: Vec::new(),
                stakes: Vec::new(),
            }
        } else {
            EvmCommittee::from_validator_committee(&committee_guard)
        };
        drop(committee_guard);
        let pk_hex = secret_key_hex(&self.secret_key)?;
        let dispute = Duration::from_secs(dispute_secs);

        let nonces: Vec<u64> = state
            .pending_evm_withdraws
            .read()
            .await
            .keys()
            .copied()
            .collect();
        for nonce in nonces {
            let snapshot = state.pending_evm_withdraws.read().await.get(&nonce).cloned();
            let Some(mut item) = snapshot else { continue };
            if item.failed {
                continue;
            }

            if !item.requested {
                match cast_request_withdraw(
                    &cast_bin,
                    rpc_url,
                    &pk_hex,
                    bridge_address,
                    item.id,
                    &item.record,
                    bridge_cfg.foreign_token,
                    epoch,
                    &evm_committee,
                    &[item.signature.clone()],
                )
                .await
                {
                    Ok(_) => {
                        info!(
                            "Route {} requested EVM withdraw nonce={}",
                            state.route_id, nonce
                        );
                        self.emit_route(
                            &state.route_id,
                            Some(route_def),
                            Some(&bridge_cfg),
                            BridgeEventKind::EvmRequestSent,
                            BridgeEventLevel::Info,
                            Some(nonce),
                            Some(item.record.amount),
                            "EVM requestWithdraw sent",
                        )
                        .await;
                        item.requested = true;
                        item.requested_at = Some(std::time::Instant::now());
                        state.pending_evm_withdraws.write().await.insert(nonce, item);
                    }
                    Err(err) => {
                        let amount = item.record.amount;
                        let mut detail = err.to_string();
                        if detail.contains("estimate gas") || detail.contains("estimateGas") {
                            detail.push_str(
                                " (hint: EVM Bridge genesis committee likely mismatches LightPool; \
                                 redeploy Bridge with VALIDATOR_STAKE matching getCommitteeInfo, default 100)",
                            );
                        }
                        if Self::evm_withdraw_permanent_failure(&detail) {
                            item.failed = true;
                            state.pending_evm_withdraws.write().await.insert(nonce, item);
                        }
                        warn!(
                            "Route {} requestWithdraw failed for {}: {}",
                            state.route_id, nonce, detail
                        );
                        self.emit_route(
                            &state.route_id,
                            Some(route_def),
                            Some(&bridge_cfg),
                            BridgeEventKind::EvmRequestFailed,
                            BridgeEventLevel::Warn,
                            Some(nonce),
                            Some(amount),
                            detail,
                        )
                        .await;
                    }
                }
                continue;
            }

            if item.finalized {
                continue;
            }
            let Some(started) = item.requested_at else { continue };
            if started.elapsed() < dispute {
                continue;
            }
            match cast_finalize_withdraw(&cast_bin, rpc_url, &pk_hex, bridge_address, item.id).await
            {
                Ok(_) => {
                    info!(
                        "Route {} finalized EVM withdraw nonce={}",
                        state.route_id, nonce
                    );
                    self.emit_route(
                        &state.route_id,
                        Some(route_def),
                        Some(&bridge_cfg),
                        BridgeEventKind::EvmFinalized,
                        BridgeEventLevel::Info,
                        Some(nonce),
                        Some(item.record.amount),
                        "EVM withdraw finalized",
                    )
                    .await;
                    item.finalized = true;
                    state.pending_evm_withdraws.write().await.insert(nonce, item);
                }
                Err(err) => {
                    warn!(
                        "Route {} finalizeWithdraw failed for {}: {}",
                        state.route_id, nonce, err
                    );
                    self.emit_route(
                        &state.route_id,
                        Some(route_def),
                        Some(&bridge_cfg),
                        BridgeEventKind::EvmFinalizeFailed,
                        BridgeEventLevel::Warn,
                        Some(nonce),
                        None,
                        err.to_string(),
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    async fn try_submit_lp_deposit_nonce(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        nonce: u64,
    ) -> anyhow::Result<()> {
        if !self.is_leader().await {
            return Ok(());
        }
        let ForeignLeg::Lightpool {
            rpc_url,
            outbound_bridge_contract,
            ..
        } = &route_def.foreign
        else {
            return Ok(());
        };
        let foreign_contract = parse_contract_address(outbound_bridge_contract)?;
        let foreign_client = LightpoolClient::new(rpc_url);
        let bridge_cfg = self.require_route_config(state).await.ok();

        let snapshot = state.pending_lp_deposits.read().await.get(&nonce).cloned();
        let Some(mut item) = snapshot else {
            return Ok(());
        };
        if item.submitted || item.failed {
            return Ok(());
        }

        let params = DepositParams {
            message: item.message.clone(),
            votes: item.votes.clone(),
        };
        self.emit_route(
            &state.route_id,
            Some(route_def),
            bridge_cfg.as_ref(),
            BridgeEventKind::DepositSubmitted,
            BridgeEventLevel::Info,
            Some(nonce),
            Some(item.message.amount),
            "submitting foreign deposit",
        )
        .await;
        match foreign_client
            .submit(
                foreign_contract,
                DEPOSIT_ACTION,
                bincode::serialize(&params)?,
                self.name,
                &self.secret_key,
            )
            .await
        {
            Ok(receipt) if receipt.is_success() => {
                info!(
                    "Route {} submitted foreign deposit nonce={}",
                    state.route_id, nonce
                );
                self.emit_route(
                    &state.route_id,
                    Some(route_def),
                    bridge_cfg.as_ref(),
                    BridgeEventKind::DepositOk,
                    BridgeEventLevel::Info,
                    Some(nonce),
                    Some(item.message.amount),
                    "foreign deposit confirmed",
                )
                .await;
                item.submitted = true;
                state.pending_lp_deposits.write().await.insert(nonce, item);
            }
            Ok(receipt) => {
                let detail = format!("{:?}", receipt.status);
                if Self::already_processed_detail(&detail) {
                    info!(
                        "Route {} foreign deposit nonce={} already processed",
                        state.route_id, nonce
                    );
                    self.emit_route(
                        &state.route_id,
                        Some(route_def),
                        bridge_cfg.as_ref(),
                        BridgeEventKind::DepositOk,
                        BridgeEventLevel::Info,
                        Some(nonce),
                        Some(item.message.amount),
                        "foreign deposit already processed",
                    )
                    .await;
                    item.submitted = true;
                    state.pending_lp_deposits.write().await.insert(nonce, item);
                    return Ok(());
                }
                warn!(
                    "Route {} foreign deposit failed nonce={}: {}",
                    state.route_id, nonce, detail
                );
                self.emit_route(
                    &state.route_id,
                    Some(route_def),
                    bridge_cfg.as_ref(),
                    BridgeEventKind::DepositFailed,
                    BridgeEventLevel::Error,
                    Some(nonce),
                    None,
                    detail.clone(),
                )
                .await;
                if detail.contains("not in authorities")
                    || detail.contains("Unauthorized")
                    || detail.contains("epoch mismatch")
                {
                    item.failed = true;
                    state.pending_lp_deposits.write().await.insert(nonce, item);
                }
            }
            Err(err) => {
                let detail = err.to_string();
                if Self::already_processed_detail(&detail) {
                    info!(
                        "Route {} foreign deposit nonce={} already processed",
                        state.route_id, nonce
                    );
                    item.submitted = true;
                    state.pending_lp_deposits.write().await.insert(nonce, item);
                    return Ok(());
                }
                warn!(
                    "Route {} foreign deposit error nonce={}: {}",
                    state.route_id, nonce, detail
                );
                self.emit_route(
                    &state.route_id,
                    Some(route_def),
                    bridge_cfg.as_ref(),
                    BridgeEventKind::DepositFailed,
                    BridgeEventLevel::Error,
                    Some(nonce),
                    None,
                    detail,
                )
                .await;
            }
        }
        Ok(())
    }

    async fn try_submit_lp_deposits(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
    ) -> anyhow::Result<()> {
        let nonces: Vec<u64> = state
            .pending_lp_deposits
            .read()
            .await
            .keys()
            .copied()
            .collect();
        for nonce in nonces {
            self.try_submit_lp_deposit_nonce(state, route_def, nonce)
                .await?;
        }
        Ok(())
    }

    async fn handle_evm_deposit_log(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        log: &Value,
        bridge_cfg: &BridgeConfig,
        expected_token: [u8; 20],
        source_chain_id: u64,
    ) -> anyhow::Result<()> {
        let topics = log
            .get("topics")
            .and_then(|t| t.as_array())
            .ok_or_else(|| anyhow::anyhow!("log missing topics"))?;
        if topics.len() < 4 {
            return Ok(());
        }

        let deposit_id = topic_u64(topics[1].as_str().unwrap_or_default())?;
        {
            let mut seen = state.seen_deposits.write().await;
            if !seen.insert(deposit_id) {
                return Ok(());
            }
        }

        let sender_foreign = topic_address(topics[2].as_str().unwrap_or_default())?;
        let recipient_bytes = topic_address(topics[3].as_str().unwrap_or_default())?;
        let data_hex = log
            .get("data")
            .and_then(|d| d.as_str())
            .unwrap_or("0x")
            .trim_start_matches("0x");
        let data = hex::decode(data_hex)?;
        if data.len() < 96 {
            return Err(anyhow::anyhow!("deposit log data too short"));
        }
        let log_token = address_from_word(&data[0..32])?;
        if log_token != expected_token {
            return Ok(());
        }
        let amount = u64_from_word(&data[32..64])?;
        let source_block = u64_from_word(&data[64..96])?;

        let committee = self.committee.read().await;
        let epoch = if committee.size() == 0 {
            0
        } else {
            committee.epoch as u64
        };
        drop(committee);

        let tx_hash = parse_b32(
            log.get("transactionHash")
                .and_then(|v| v.as_str())
                .unwrap_or_default(),
        )?;

        let message = BridgeDepositMessage {
            message_id: deposit_id,
            source_chain_id,
            token: bridge_cfg.token,
            amount,
            sender_foreign,
            recipient: Address::new(recipient_bytes),
            source_tx_hash: tx_hash,
            source_block,
            epoch,
        };

        info!(
            "Route {} observed EVM deposit id={} amount={}",
            state.route_id, deposit_id, amount
        );
        self.emit_route(
            &state.route_id,
            Some(route_def),
            Some(bridge_cfg),
            BridgeEventKind::DepositSeen,
            BridgeEventLevel::Info,
            Some(deposit_id),
            Some(amount),
            format!("EVM deposit recipient={}", message.recipient),
        )
        .await;

        let vote = BridgeLinkVote::from_deposit(&message, self.name, &self.secret_key);
        self.ingest_deposit_vote(state, route_def, vote, message).await;
        self.try_submit_deposits().await?;
        Ok(())
    }

    pub(crate) async fn poll_foreign_withdraws(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
    ) -> anyhow::Result<()> {
        self.prepare_route(state, route_def).await;
        let bridge_cfg = match self.require_route_config(state).await {
            Ok(cfg) => cfg,
            Err(_) => return Ok(()),
        };
        let ForeignLeg::Lightpool {
            rpc_url,
            chain_id,
            outbound_bridge_contract,
            ..
        } = &route_def.foreign
        else {
            return Ok(());
        };
        let foreign_contract = parse_contract_address(outbound_bridge_contract)?;
        let foreign_client = LightpoolClient::new(rpc_url);
        let foreign_cfg = match foreign_client.fetch_outbound_config(foreign_contract).await {
            Ok(cfg) => cfg,
            Err(_) => return Ok(()),
        };
        let next = foreign_cfg.next_withdraw_nonce;
        if next <= 1 {
            return Ok(());
        }
        let mut last = state.last_foreign_withdraw_nonce.load(Ordering::Relaxed);
        while last + 1 < next {
            let nonce = last + 1;
            match foreign_client
                .fetch_outbound_withdraw(foreign_contract, nonce)
                .await
            {
                Ok(withdraw) => {
                    if withdraw.status == OutboundWithdrawStatus::Pending {
                        self.handle_foreign_withdraw(
                            state,
                            route_def,
                            &bridge_cfg,
                            *chain_id,
                            &withdraw,
                        )
                        .await?;
                    }
                    last = nonce;
                    state
                        .last_foreign_withdraw_nonce
                        .store(last, Ordering::Relaxed);
                }
                Err(err) => {
                    debug!(
                        "Route {} foreign withdraw {} not ready: {}",
                        state.route_id, nonce, err
                    );
                    break;
                }
            }
        }
        Ok(())
    }

    async fn handle_foreign_withdraw(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        bridge_cfg: &BridgeConfig,
        source_chain_id: u64,
        withdraw: &lightpool_types::module_types::bridge::OutboundWithdrawRecord,
    ) -> anyhow::Result<()> {
        {
            let mut seen = state.seen_deposits.write().await;
            if !seen.insert(withdraw.nonce) {
                return Ok(());
            }
        }

        let committee = self.committee.read().await;
        let epoch = if committee.size() == 0 {
            0
        } else {
            committee.epoch as u64
        };
        drop(committee);

        let foreign_contract = match &route_def.foreign {
            ForeignLeg::Lightpool {
                outbound_bridge_contract,
                ..
            } => parse_contract_address(outbound_bridge_contract)?,
            _ => unreachable!(),
        };

        let message = BridgeDepositMessage {
            message_id: withdraw.nonce,
            source_chain_id,
            token: bridge_cfg.token,
            amount: withdraw.amount,
            sender_foreign: *withdraw.sender.as_bytes(),
            recipient: Address::new(withdraw.foreign_recipient),
            source_tx_hash: lock_source_hash(source_chain_id, foreign_contract, withdraw.nonce),
            source_block: 0,
            epoch,
        };

        info!(
            "Route {} observed foreign withdraw nonce={} amount={}",
            state.route_id, withdraw.nonce, withdraw.amount
        );
        self.emit_route(
            &state.route_id,
            Some(route_def),
            Some(bridge_cfg),
            BridgeEventKind::DepositSeen,
            BridgeEventLevel::Info,
            Some(withdraw.nonce),
            Some(withdraw.amount),
            format!("foreign LP deposit recipient={}", message.recipient),
        )
        .await;

        let vote = BridgeLinkVote::from_deposit(&message, self.name, &self.secret_key);
        self.ingest_deposit_vote(state, route_def, vote, message).await;
        self.try_submit_deposits().await
    }

    async fn ingest_deposit_vote(
        &self,
        state: &RouteState,
        route_def: &BridgeRoute,
        vote: BridgeLinkVote,
        message: BridgeDepositMessage,
    ) {
        let contract = inbound_contract_for(route_def);
        let mut pending = self.pending.write().await;
        let bucket = pending
            .entry((state.route_id.clone(), vote.kind, vote.message_id))
            .or_insert_with(|| VoteBucket {
                route_id: state.route_id.clone(),
                inbound_contract: contract,
                message: None,
                votes: HashMap::new(),
                submitted: false,
                submitting: false,
            });
        if bucket.message.is_none() {
            bucket.message = Some(message);
        }
        bucket.votes.insert(vote.validator, vote);
    }

    fn deposit_already_processed(err: &anyhow::Error) -> bool {
        Self::already_processed_detail(&err.to_string())
    }

    async fn claim_deposit_submit(&self, route_id: &str, message_id: u64) -> bool {
        let mut pending = self.pending.write().await;
        let Some(bucket) = pending.get_mut(&(
            route_id.to_string(),
            BridgeVoteKind::Deposit,
            message_id,
        )) else {
            return false;
        };
        if bucket.submitted || bucket.submitting {
            return false;
        }
        bucket.submitting = true;
        true
    }

    async fn finish_deposit_submit(
        &self,
        route_id: &str,
        message_id: u64,
        token: ContractAddress,
        amount: u64,
        result: anyhow::Result<()>,
        detail_ok: &str,
    ) {
        let already = match &result {
            Ok(()) => false,
            Err(err) => {
                if !Self::deposit_already_processed(err) {
                    let mut pending = self.pending.write().await;
                    if let Some(bucket) = pending.get_mut(&(
                        route_id.to_string(),
                        BridgeVoteKind::Deposit,
                        message_id,
                    )) {
                        bucket.submitting = false;
                    }
                    warn!(
                        "Route {} confirm_dep failed for {}: {}",
                        route_id, message_id, err
                    );
                    self.events
                        .emit(
                            route_id,
                            Some(format!("{token}")),
                            BridgeEventKind::ConfirmDepFailed,
                            BridgeEventLevel::Error,
                            Some(message_id),
                            Some(amount),
                            err.to_string(),
                        )
                        .await;
                    return;
                }
                true
            }
        };

        {
            let mut pending = self.pending.write().await;
            if let Some(bucket) = pending.get_mut(&(
                route_id.to_string(),
                BridgeVoteKind::Deposit,
                message_id,
            )) {
                bucket.submitting = false;
                bucket.submitted = true;
            }
        }

        if already {
            info!(
                "Route {} deposit {} already processed on LightPool",
                route_id, message_id
            );
        } else {
            info!(
                "Route {} submitted confirm_dep for deposit {}",
                route_id, message_id
            );
        }
        self.events
            .emit(
                route_id,
                Some(format!("{token}")),
                BridgeEventKind::ConfirmDepOk,
                BridgeEventLevel::Info,
                Some(message_id),
                Some(amount),
                if already {
                    "confirm_dep already processed"
                } else {
                    detail_ok
                },
            )
            .await;
    }

    async fn try_submit_deposits(&self) -> anyhow::Result<()> {
        let committee = self.committee.read().await;
        let keys = committee.voting_public_keys();
        if keys.is_empty() {
            let pending_snapshot: Vec<(String, ContractAddress, u64, BridgeDepositMessage)> = {
                let pending = self.pending.read().await;
                pending
                    .iter()
                    .filter_map(|((route_id, kind, id), bucket)| {
                        if *kind != BridgeVoteKind::Deposit || bucket.submitted || bucket.submitting {
                            return None;
                        }
                        let message = bucket.message.as_ref()?;
                        Some((
                            route_id.clone(),
                            bucket.inbound_contract,
                            *id,
                            message.clone(),
                        ))
                    })
                    .collect()
            };
            for (route_id, contract, message_id, message) in pending_snapshot {
                if !self.claim_deposit_submit(&route_id, message_id).await {
                    continue;
                }
                self.events
                    .emit(
                        &route_id,
                        Some(format!("{}", message.token)),
                        BridgeEventKind::ConfirmDepSubmitted,
                        BridgeEventLevel::Info,
                        Some(message_id),
                        Some(message.amount),
                        "submitting confirm_dep (testing)",
                    )
                    .await;
                let result = self
                    .submit_confirm_deposit(contract, &message, Vec::new())
                    .await;
                self.finish_deposit_submit(
                    &route_id,
                    message_id,
                    message.token,
                    message.amount,
                    result,
                    "confirm_dep ok",
                )
                .await;
            }
            return Ok(());
        }
        let dispute = self.config.read().await.dispute_period_seconds.max(1);
        let round = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / dispute) as usize;
        let leader = keys[round % keys.len()];
        if leader != self.name {
            return Ok(());
        }

        let threshold = committee.quorum_threshold() as u64;
        let pending_snapshot: Vec<(String, ContractAddress, u64, BridgeDepositMessage, Vec<BridgeVote>)> =
            {
                let pending = self.pending.read().await;
                pending
                    .iter()
                    .filter_map(|((route_id, kind, id), bucket)| {
                        if *kind != BridgeVoteKind::Deposit || bucket.submitted || bucket.submitting {
                            return None;
                        }
                        let message = bucket.message.as_ref()?;
                        let mut power = 0u64;
                        let mut votes = Vec::new();
                        for (pk, vote) in &bucket.votes {
                            let stake = committee.stake(pk) as u64;
                            if stake == 0 {
                                continue;
                            }
                            power = power.saturating_add(stake);
                            votes.push(vote.clone().into_bridge_vote());
                        }
                        if power < threshold {
                            return None;
                        }
                        Some((
                            route_id.clone(),
                            bucket.inbound_contract,
                            *id,
                            message.clone(),
                            votes,
                        ))
                    })
                    .collect()
            };

        for (route_id, contract, message_id, message, votes) in pending_snapshot {
            if !self.claim_deposit_submit(&route_id, message_id).await {
                continue;
            }
            self.events
                .emit(
                    &route_id,
                    Some(format!("{}", message.token)),
                    BridgeEventKind::ConfirmDepSubmitted,
                    BridgeEventLevel::Info,
                    Some(message_id),
                    Some(message.amount),
                    format!("submitting confirm_dep with {} vote(s)", votes.len()),
                )
                .await;
            let result = self
                .submit_confirm_deposit(contract, &message, votes)
                .await;
            self.finish_deposit_submit(
                &route_id,
                message_id,
                message.token,
                message.amount,
                result,
                "confirm_dep ok",
            )
            .await;
        }
        Ok(())
    }

    async fn submit_confirm_deposit(
        &self,
        contract: ContractAddress,
        message: &BridgeDepositMessage,
        votes: Vec<BridgeVote>,
    ) -> anyhow::Result<()> {
        let params = ConfirmDepositParams {
            message: message.clone(),
            votes,
        };
        let receipt = self
            .local
            .read()
            .await
            .submit(
                contract,
                CONFIRM_DEP_ACTION,
                bincode::serialize(&params)?,
                self.name,
                &self.secret_key,
            )
            .await?;
        if !receipt.is_success() {
            return Err(anyhow::anyhow!(
                "confirm_dep execution failed: {:?}",
                receipt.status
            ));
        }
        Ok(())
    }

    pub async fn ingest_vote(&self, vote: BridgeLinkVote) {
        let mut pending = self.pending.write().await;
        if let Some((route_id, _, _)) = pending
            .keys()
            .find(|(_, kind, id)| *kind == vote.kind && *id == vote.message_id)
            .cloned()
        {
            if let Some(bucket) = pending.get_mut(&(route_id, vote.kind, vote.message_id)) {
                bucket.votes.insert(vote.validator, vote);
            }
        }
    }

    pub fn local_name(&self) -> PublicKey {
        self.name
    }

    pub fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }
}

fn parse_inbound_contract(route: &BridgeRoute) -> anyhow::Result<ContractAddress> {
    parse_contract_address(&route.local_inbound.bridge_contract)
}

pub fn inbound_contract_for(route: &BridgeRoute) -> ContractAddress {
    parse_inbound_contract(route).unwrap_or_else(|_| ContractAddress::new(Module::BRIDGE, [0u8; 7]))
}

fn foreign_kind_label(foreign: &ForeignLeg) -> String {
    match foreign {
        ForeignLeg::Evm { .. } => "evm".to_string(),
        ForeignLeg::Lightpool { .. } => "lightpool".to_string(),
    }
}

use crate::handle::BridgeHandle;

pub fn spawn_bridge_router(
    config_path: PathBuf,
    config: BridgeLinkConfig,
    name: PublicKey,
    secret_key: SecretKey,
    committee: Committee,
    admin_listen: Option<std::net::SocketAddr>,
    cancel: CancellationToken,
) -> anyhow::Result<BridgeHandle> {
    if !config.enabled {
        anyhow::bail!("bridge config has enabled=false");
    }
    let handle = BridgeHandle::new(config_path, config, name, secret_key, committee)?;
    if let Some(listen) = admin_listen {
        let admin_handle = handle.clone();
        tokio::spawn(async move {
            if let Err(err) = crate::admin::run_embedded(admin_handle, listen).await {
                log::error!("Bridge admin server failed: {}", err);
            }
        });
    }
    let router = handle.router();
    tokio::spawn(async move {
        router.run(cancel).await;
    });
    Ok(handle)
}

// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lightpool_crypto::{Keccak256, PublicKey, SecretKey, Signature};
use lightpool_types::address_type::Address;
use lightpool_types::contract::ContractAddress;
use lightpool_types::effects::TransactionReceipt;
use lightpool_types::module::Module;
use lightpool_types::module_types::bridge::{
    BridgeConfig, BridgeDepositMessage, BridgeVote, BridgeWithdrawRecord, BridgeWithdrawStatus,
};
use lightpool_types::name;
use lightpool_types::transaction::{Action, SignedTransaction, Transaction};
use lightpool_types::Committee;
use lightpool_types::Name;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::SocketAddr;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::config::BridgeLinkConfig;
use crate::evm::{
    cast_finalize_withdraw, cast_request_withdraw, eth_sign_digest, request_withdraw_digest,
    secret_key_hex, withdraw_id, EthSignature, EvmCommittee,
};
use crate::messages::{BridgeLinkVote, BridgeVoteKind};

const CONFIRM_DEP_ACTION: Name = name!("confirm_dep");
const GET_CONFIG_ACTION: Name = name!("get_config");
const GET_WITHDRAW_ACTION: Name = name!("get_withdraw");

#[derive(Debug, Serialize)]
struct ConfirmDepositParams {
    message: BridgeDepositMessage,
    votes: Vec<BridgeVote>,
}

#[derive(Debug, Serialize)]
struct GetWithdrawParams {
    nonce: u64,
}

#[derive(Debug, Default)]
struct VoteBucket {
    message: Option<BridgeDepositMessage>,
    votes: HashMap<PublicKey, BridgeLinkVote>,
    submitted: bool,
}

#[derive(Debug, Clone)]
struct PendingEvmWithdraw {
    record: BridgeWithdrawRecord,
    id: [u8; 32],
    signature: EthSignature,
    requested: bool,
    requested_at: Option<std::time::Instant>,
    finalized: bool,
}

pub struct BridgeLinkService {
    config: BridgeLinkConfig,
    name: PublicKey,
    secret_key: SecretKey,
    committee: Arc<RwLock<Committee>>,
    pending: Arc<RwLock<HashMap<(BridgeVoteKind, u64), VoteBucket>>>,
    bridge_config: Arc<RwLock<Option<BridgeConfig>>>,
    pending_withdraws: Arc<RwLock<HashMap<u64, PendingEvmWithdraw>>>,
    last_scanned_block: AtomicU64,
    last_withdraw_nonce: AtomicU64,
    deposit_topic: String,
    http: reqwest::Client,
    seen_deposits: Arc<RwLock<HashSet<u64>>>,
}

impl BridgeLinkService {
    pub fn new(
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
        Ok(Self {
            config,
            name,
            secret_key,
            committee: Arc::new(RwLock::new(committee)),
            pending: Arc::new(RwLock::new(HashMap::new())),
            bridge_config: Arc::new(RwLock::new(None)),
            pending_withdraws: Arc::new(RwLock::new(HashMap::new())),
            last_scanned_block: AtomicU64::new(0),
            last_withdraw_nonce: AtomicU64::new(0),
            deposit_topic,
            http: reqwest::Client::new(),
            seen_deposits: Arc::new(RwLock::new(HashSet::new())),
        })
    }

    pub fn spawn(self, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run(cancel).await;
        })
    }

    async fn run(self, cancel: CancellationToken) {
        info!(
            "Bridge Link started (evm_rpc={}, bridge={}, confirmations={})",
            self.config.evm_rpc_url,
            self.config.evm_bridge_address,
            self.config.evm_confirmations
        );

        let interval = Duration::from_millis(self.config.poll_interval_ms.max(100));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("Bridge Link shutting down");
                    break;
                }
                _ = tokio::time::sleep(interval) => {
                    if let Err(err) = self.tick().await {
                        warn!("Bridge Link tick error: {}", err);
                    }
                }
            }
        }
    }

    async fn tick(&self) -> anyhow::Result<()> {
        self.refresh_committee().await?;
        self.refresh_bridge_config().await?;
        self.poll_evm_deposits().await?;
        self.poll_lightpool_withdrawals().await?;
        self.try_submit_as_leader().await?;
        self.try_submit_withdraws_as_leader().await?;
        Ok(())
    }

    async fn refresh_committee(&self) -> anyhow::Result<()> {
        match self.fetch_committee().await {
            Ok(committee) => {
                let mut guard = self.committee.write().await;
                if guard.epoch != committee.epoch || guard.size() != committee.size() {
                    info!(
                        "Bridge Link loaded committee epoch={} size={}",
                        committee.epoch,
                        committee.size()
                    );
                }
                *guard = committee;
                Ok(())
            }
            Err(err) => {
                if self.committee.read().await.size() == 0 {
                    debug!("Bridge Link waiting for committee: {}", err);
                }
                Ok(())
            }
        }
    }

    async fn fetch_committee(&self) -> anyhow::Result<Committee> {
        #[derive(Deserialize)]
        struct CommitteeMemberInfo {
            consensus_pubkey: PublicKey,
            stake: u32,
            #[serde(default)]
            mempool_address: String,
            #[serde(default)]
            consensus_address: String,
        }

        #[derive(Deserialize)]
        struct CommitteeInfo {
            epoch: u128,
            members: Vec<CommitteeMemberInfo>,
        }

        let body = json!({
            "jsonrpc": "2.0",
            "method": "getCommitteeInfo",
            "params": [],
            "id": 1,
        });
        let response = self
            .http
            .post(&self.config.lightpool_rpc_url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let value: Value = response.json().await?;
        if let Some(err) = value.get("error") {
            return Err(anyhow::anyhow!("getCommitteeInfo error: {}", err));
        }
        let result = value
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("getCommitteeInfo missing result"))?;
        let parsed: CommitteeInfo = serde_json::from_value(result)?;
        let placeholder: SocketAddr = "0.0.0.0:0".parse()?;
        let info = parsed
            .members
            .into_iter()
            .map(|m| {
                let consensus = m
                    .consensus_address
                    .parse::<SocketAddr>()
                    .unwrap_or(placeholder);
                let mempool = m
                    .mempool_address
                    .parse::<SocketAddr>()
                    .unwrap_or(placeholder);
                (
                    m.consensus_pubkey,
                    m.stake,
                    consensus,
                    placeholder,
                    mempool,
                )
            })
            .collect();
        Ok(Committee::new(info, parsed.epoch))
    }

    async fn refresh_bridge_config(&self) -> anyhow::Result<()> {
        match self.fetch_bridge_config().await {
            Ok(cfg) => {
                let mut guard = self.bridge_config.write().await;
                if guard.is_none() {
                    info!(
                        "Bridge Link loaded on-chain config: lp_token={}, evm_token=0x{}, chain_id={}, epoch={}",
                        cfg.token,
                        hex::encode(cfg.evm_token),
                        cfg.evm_chain_id,
                        cfg.epoch
                    );
                }
                *guard = Some(cfg);
                Ok(())
            }
            Err(err) => {
                if self.bridge_config.read().await.is_none() {
                    debug!("Bridge Link waiting for on-chain bridge config: {}", err);
                }
                Ok(())
            }
        }
    }

    async fn require_bridge_config(&self) -> anyhow::Result<BridgeConfig> {
        self.bridge_config
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("bridge config not loaded from LightPool yet"))
    }

    async fn lightpool_call(&self, action: Action) -> anyhow::Result<Vec<u8>> {
        let tx = Transaction::new(Address::zero(), u64::MAX, vec![action]);
        let signed = SignedTransaction::new(tx, Signature::default());

        #[derive(Serialize)]
        struct CallParams {
            tx: SignedTransaction,
        }
        #[derive(Deserialize)]
        struct CallResponse {
            bytes: Vec<u8>,
        }

        let body = json!({
            "jsonrpc": "2.0",
            "method": "call",
            "params": [CallParams { tx: signed }],
            "id": 1,
        });
        let response = self
            .http
            .post(&self.config.lightpool_rpc_url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let value: Value = response.json().await?;
        if let Some(err) = value.get("error") {
            return Err(anyhow::anyhow!("lightpool call error: {}", err));
        }
        let result = value
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("lightpool call missing result"))?;
        let parsed: CallResponse = serde_json::from_value(result)?;
        Ok(parsed.bytes)
    }

    async fn fetch_bridge_config(&self) -> anyhow::Result<BridgeConfig> {
        let action = Action::new(bridge_module_contract(), GET_CONFIG_ACTION, Vec::new());
        let bytes = self.lightpool_call(action).await?;
        bincode::deserialize(&bytes).map_err(|e| anyhow::anyhow!("decode bridge config: {}", e))
    }

    async fn fetch_withdraw_record(&self, nonce: u64) -> anyhow::Result<BridgeWithdrawRecord> {
        let action = Action::new(
            bridge_module_contract(),
            GET_WITHDRAW_ACTION,
            bincode::serialize(&GetWithdrawParams { nonce })?,
        );
        let bytes = self.lightpool_call(action).await?;
        bincode::deserialize(&bytes).map_err(|e| anyhow::anyhow!("decode withdraw record: {}", e))
    }

    async fn poll_lightpool_withdrawals(&self) -> anyhow::Result<()> {
        let bridge_cfg = match self.require_bridge_config().await {
            Ok(cfg) => cfg,
            Err(_) => return Ok(()),
        };
        let next = bridge_cfg.next_withdraw_nonce;
        if next <= 1 {
            return Ok(());
        }
        let mut last = self.last_withdraw_nonce.load(Ordering::Relaxed);
        while last + 1 < next {
            let nonce = last + 1;
            match self.fetch_withdraw_record(nonce).await {
                Ok(record) => {
                    if record.status == BridgeWithdrawStatus::Pending {
                        self.enqueue_withdraw(&bridge_cfg, record).await?;
                    }
                    last = nonce;
                    self.last_withdraw_nonce.store(last, Ordering::Relaxed);
                }
                Err(err) => {
                    debug!("Bridge Link withdraw {} not ready: {}", nonce, err);
                    break;
                }
            }
        }
        Ok(())
    }

    async fn enqueue_withdraw(
        &self,
        bridge_cfg: &BridgeConfig,
        record: BridgeWithdrawRecord,
    ) -> anyhow::Result<()> {
        let mut pending = self.pending_withdraws.write().await;
        if pending.contains_key(&record.nonce) {
            return Ok(());
        }
        let user = *record.sender.as_bytes();
        let id = withdraw_id(
            record.nonce,
            user,
            record.evm_recipient,
            record.amount,
            bridge_cfg.epoch,
        );
        let digest = request_withdraw_digest(
            id,
            user,
            record.evm_recipient,
            bridge_cfg.evm_token,
            record.amount,
            record.nonce,
            bridge_cfg.epoch,
        );
        let signature = eth_sign_digest(digest, &self.secret_key)?;
        info!(
            "Bridge Link observed LP withdraw nonce={} amount={} dest=0x{}",
            record.nonce,
            record.amount,
            hex::encode(record.evm_recipient)
        );
        pending.insert(
            record.nonce,
            PendingEvmWithdraw {
                record,
                id,
                signature,
                requested: false,
                requested_at: None,
                finalized: false,
            },
        );
        Ok(())
    }

    async fn is_leader(&self) -> bool {
        let committee = self.committee.read().await;
        let keys = committee.voting_public_keys();
        if keys.is_empty() {
            return false;
        }
        let round = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / self.config.dispute_period_seconds.max(1)) as usize;
        keys[round % keys.len()] == self.name
    }

    async fn try_submit_withdraws_as_leader(&self) -> anyhow::Result<()> {
        if !self.is_leader().await {
            return Ok(());
        }
        let bridge_cfg = match self.require_bridge_config().await {
            Ok(cfg) => cfg,
            Err(_) => return Ok(()),
        };
        let pk_hex = secret_key_hex(&self.secret_key)?;
        let committee = EvmCommittee::from_authorities(bridge_cfg.epoch, &bridge_cfg.authorities);
        let dispute = Duration::from_secs(self.config.dispute_period_seconds.max(1) + 2);

        let nonces: Vec<u64> = self.pending_withdraws.read().await.keys().copied().collect();
        for nonce in nonces {
            let snapshot = self.pending_withdraws.read().await.get(&nonce).cloned();
            let Some(mut item) = snapshot else { continue };

            if !item.requested {
                match cast_request_withdraw(
                    &self.config.cast_bin,
                    &self.config.evm_rpc_url,
                    &pk_hex,
                    &self.config.evm_bridge_address,
                    item.id,
                    &item.record,
                    bridge_cfg.evm_token,
                    bridge_cfg.epoch,
                    &committee,
                    &[item.signature.clone()],
                )
                .await
                {
                    Ok(_) => {
                        info!("Bridge Link requested EVM withdraw nonce={}", nonce);
                        item.requested = true;
                        item.requested_at = Some(std::time::Instant::now());
                        self.pending_withdraws.write().await.insert(nonce, item);
                    }
                    Err(err) => {
                        warn!("Bridge Link requestWithdraw failed for {}: {}", nonce, err);
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
            match cast_finalize_withdraw(
                &self.config.cast_bin,
                &self.config.evm_rpc_url,
                &pk_hex,
                &self.config.evm_bridge_address,
                item.id,
            )
            .await
            {
                Ok(_) => {
                    info!("Bridge Link finalized EVM withdraw nonce={}", nonce);
                    item.finalized = true;
                    self.pending_withdraws.write().await.insert(nonce, item);
                }
                Err(err) => {
                    warn!("Bridge Link finalizeWithdraw failed for {}: {}", nonce, err);
                }
            }
        }
        Ok(())
    }

    async fn poll_evm_deposits(&self) -> anyhow::Result<()> {
        let bridge_cfg = match self.require_bridge_config().await {
            Ok(cfg) => cfg,
            Err(_) => return Ok(()),
        };

        let latest = self.eth_block_number().await?;
        if latest < self.config.evm_confirmations {
            return Ok(());
        }
        let safe = latest - self.config.evm_confirmations;

        let mut from = self.last_scanned_block.load(Ordering::Relaxed);
        if from == 0 {
            from = if self.config.start_block > 0 {
                self.config.start_block
            } else {
                safe.saturating_sub(5)
            };
        } else {
            from = from.saturating_add(1);
        }
        if from > safe {
            return Ok(());
        }

        let logs = self.eth_get_logs(from, safe).await?;
        for log in logs {
            if let Err(err) = self.handle_deposit_log(&log, &bridge_cfg).await {
                warn!("Bridge Link failed to handle deposit log: {}", err);
            }
        }
        self.last_scanned_block.store(safe, Ordering::Relaxed);
        Ok(())
    }

    async fn handle_deposit_log(
        &self,
        log: &Value,
        bridge_cfg: &BridgeConfig,
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
            let mut seen = self.seen_deposits.write().await;
            if !seen.insert(deposit_id) {
                return Ok(());
            }
        }

        let sender_evm = topic_address(topics[2].as_str().unwrap_or_default())?;
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
        if log_token != bridge_cfg.evm_token {
            return Err(anyhow::anyhow!(
                "deposit evm token mismatch: got 0x{}, expected 0x{}",
                hex::encode(log_token),
                hex::encode(bridge_cfg.evm_token)
            ));
        }
        let amount = u64_from_word(&data[32..64])?;
        let source_block = u64_from_word(&data[64..96])?;

        let tx_hash = parse_b32(
            log.get("transactionHash")
                .and_then(|v| v.as_str())
                .unwrap_or_default(),
        )?;

        let message = BridgeDepositMessage {
            message_id: deposit_id,
            source_chain_id: bridge_cfg.evm_chain_id,
            token: bridge_cfg.token,
            amount,
            sender_evm,
            recipient: Address::new(recipient_bytes),
            source_tx_hash: tx_hash,
            source_block,
            epoch: bridge_cfg.epoch,
        };

        info!(
            "Bridge Link observed EVM deposit id={} amount={} recipient={}",
            deposit_id, amount, message.recipient
        );

        let vote = BridgeLinkVote::from_deposit(&message, self.name, &self.secret_key);
        self.ingest_vote_with_message(vote, message).await;
        Ok(())
    }

    async fn ingest_vote_with_message(&self, vote: BridgeLinkVote, message: BridgeDepositMessage) {
        let mut pending = self.pending.write().await;
        let bucket = pending.entry((vote.kind, vote.message_id)).or_default();
        if bucket.message.is_none() {
            bucket.message = Some(message);
        }
        bucket.votes.insert(vote.validator, vote);
    }

    async fn try_submit_as_leader(&self) -> anyhow::Result<()> {
        let committee = self.committee.read().await;
        let keys = committee.voting_public_keys();
        if keys.is_empty() {
            return Ok(());
        }
        let round = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / self.config.dispute_period_seconds.max(1)) as usize;
        let leader = keys[round % keys.len()];
        if leader != self.name {
            return Ok(());
        }

        let threshold = committee.quorum_threshold() as u64;
        let pending_snapshot: Vec<(u64, BridgeDepositMessage, Vec<BridgeVote>)> = {
            let pending = self.pending.read().await;
            pending
                .iter()
                .filter_map(|((kind, id), bucket)| {
                    if *kind != BridgeVoteKind::Deposit || bucket.submitted {
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
                        debug!(
                            "Bridge Link deposit {} waiting quorum power={} threshold={}",
                            id, power, threshold
                        );
                        return None;
                    }
                    Some((*id, message.clone(), votes))
                })
                .collect()
        };

        for (message_id, message, votes) in pending_snapshot {
            match self.submit_confirm_deposit(&message, votes).await {
                Ok(()) => {
                    let mut pending = self.pending.write().await;
                    if let Some(bucket) = pending.get_mut(&(BridgeVoteKind::Deposit, message_id)) {
                        bucket.submitted = true;
                    }
                    info!("Bridge Link submitted confirm_dep for deposit {}", message_id);
                }
                Err(err) => {
                    warn!(
                        "Bridge Link confirm_dep submit failed for {}: {}",
                        message_id, err
                    );
                }
            }
        }
        Ok(())
    }

    async fn submit_confirm_deposit(
        &self,
        message: &BridgeDepositMessage,
        votes: Vec<BridgeVote>,
    ) -> anyhow::Result<()> {
        let params = ConfirmDepositParams {
            message: message.clone(),
            votes,
        };
        let action = Action::new(
            bridge_module_contract(),
            CONFIRM_DEP_ACTION,
            bincode::serialize(&params)?,
        );
        let sender = Address::from_public_key(&self.name);
        let tx = Transaction::new(sender, u64::MAX, vec![action]);
        let digest = tx.digest();
        let signature = Signature::new(&digest, &self.secret_key);
        let signed = SignedTransaction::new(tx, signature);

        #[derive(Serialize)]
        struct SubmitParams {
            tx: SignedTransaction,
        }
        #[derive(Deserialize)]
        struct SubmitResponse {
            receipt: TransactionReceipt,
        }

        let body = json!({
            "jsonrpc": "2.0",
            "method": "submitTransaction",
            "params": [SubmitParams { tx: signed }],
            "id": 1,
        });
        let response = self
            .http
            .post(&self.config.lightpool_rpc_url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let value: Value = response.json().await?;
        if let Some(err) = value.get("error") {
            return Err(anyhow::anyhow!("lightpool rpc error: {}", err));
        }
        let result = value
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("lightpool rpc missing result"))?;
        let parsed: SubmitResponse = serde_json::from_value(result)?;
        if !parsed.receipt.is_success() {
            return Err(anyhow::anyhow!(
                "confirm_dep execution failed: {:?}",
                parsed.receipt.status
            ));
        }
        Ok(())
    }

    async fn eth_block_number(&self) -> anyhow::Result<u64> {
        let result = self.eth_rpc("eth_blockNumber", json!([])).await?;
        let hex = result
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("eth_blockNumber not string"))?;
        Ok(u64::from_str_radix(hex.trim_start_matches("0x"), 16)?)
    }

    async fn eth_get_logs(&self, from: u64, to: u64) -> anyhow::Result<Vec<Value>> {
        let params = json!([{
            "fromBlock": format!("0x{:x}", from),
            "toBlock": format!("0x{:x}", to),
            "address": self.config.evm_bridge_address,
            "topics": [self.deposit_topic],
        }]);
        let result = self.eth_rpc("eth_getLogs", params).await?;
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    async fn eth_rpc(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let response = self
            .http
            .post(&self.config.evm_rpc_url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let value: Value = response.json().await?;
        if let Some(err) = value.get("error") {
            return Err(anyhow::anyhow!("evm rpc error: {}", err));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("evm rpc missing result"))
    }

    pub async fn ingest_vote(&self, vote: BridgeLinkVote) {
        let mut pending = self.pending.write().await;
        let bucket = pending.entry((vote.kind, vote.message_id)).or_default();
        bucket.votes.insert(vote.validator, vote);
    }

    pub fn local_name(&self) -> PublicKey {
        self.name
    }

    pub fn secret_key(&self) -> &SecretKey {
        &self.secret_key
    }
}

pub fn spawn_bridge_link(
    config: BridgeLinkConfig,
    name: PublicKey,
    secret_key: SecretKey,
    committee: Committee,
    cancel: CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    if !config.enabled {
        info!("Bridge Link disabled");
        return None;
    }
    if config.evm_bridge_address.is_empty() {
        warn!("Bridge Link enabled but evm_bridge_address is empty; not starting");
        return None;
    }
    match BridgeLinkService::new(config, name, secret_key, committee) {
        Ok(service) => Some(service.spawn(cancel)),
        Err(err) => {
            warn!("Bridge Link failed to start: {}", err);
            None
        }
    }
}

fn bridge_module_contract() -> ContractAddress {
    ContractAddress::new(Module::BRIDGE, [0u8; 7])
}

fn topic_u64(topic: &str) -> anyhow::Result<u64> {
    let hex_body = topic.trim_start_matches("0x");
    if hex_body.len() < 16 {
        return Err(anyhow::anyhow!("topic too short for u64"));
    }
    Ok(u64::from_str_radix(&hex_body[hex_body.len() - 16..], 16)?)
}

fn topic_address(topic: &str) -> anyhow::Result<[u8; 20]> {
    let hex_body = topic.trim_start_matches("0x");
    if hex_body.len() < 40 {
        return Err(anyhow::anyhow!("topic too short for address"));
    }
    let bytes = hex::decode(&hex_body[hex_body.len() - 40..])?;
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn u64_from_word(word: &[u8]) -> anyhow::Result<u64> {
    if word.len() != 32 {
        return Err(anyhow::anyhow!("expected 32-byte word"));
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&word[24..32]);
    Ok(u64::from_be_bytes(buf))
}

fn address_from_word(word: &[u8]) -> anyhow::Result<[u8; 20]> {
    if word.len() != 32 {
        return Err(anyhow::anyhow!("expected 32-byte word"));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&word[12..32]);
    Ok(out)
}

fn parse_b32(value: &str) -> anyhow::Result<[u8; 32]> {
    let hex_body = value.trim_start_matches("0x");
    let bytes = hex::decode(hex_body)?;
    if bytes.len() != 32 {
        return Err(anyhow::anyhow!("expected 32-byte hash"));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

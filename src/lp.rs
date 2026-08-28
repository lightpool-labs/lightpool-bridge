// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use crate::actions::{GET_CONFIG_ACTION, GET_OUTBOUND_WITHDRAW_ACTION, GET_WITHDRAW_ACTION};
use lightpool_crypto::{PublicKey, SecretKey, Signature};
use lightpool_types::address_type::Address;
use lightpool_types::contract::ContractAddress;
use lightpool_types::effects::TransactionReceipt;
use lightpool_types::module_types::bridge::{
    BridgeConfig, BridgeWithdrawRecord, OutboundBridgeConfig, OutboundWithdrawRecord,
};
use lightpool_types::transaction::{Action, SignedTransaction, Transaction};
use lightpool_types::Committee;
use lightpool_types::Name;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::SocketAddr;

#[derive(Clone)]
pub struct LightpoolClient {
    rpc_url: String,
    http: reqwest::Client,
}

impl LightpoolClient {
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            http: reqwest::Client::new(),
        }
    }

    pub fn set_rpc_url(&mut self, rpc_url: impl Into<String>) {
        self.rpc_url = rpc_url.into();
    }

    pub async fn fetch_committee(&self) -> anyhow::Result<Committee> {
        #[derive(Deserialize)]
        struct CommitteeMemberInfo {
            consensus_pubkey: PublicKey,
            stake: u64,
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
        let value = self.post_json(&body).await?;
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

    pub async fn call(&self, contract: ContractAddress, action: Action) -> anyhow::Result<Vec<u8>> {
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
        let value = self.post_json(&body).await?;
        let result = value
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("lightpool call missing result"))?;
        let parsed: CallResponse = serde_json::from_value(result)?;
        Ok(parsed.bytes)
    }

    pub async fn submit(
        &self,
        contract: ContractAddress,
        action: Name,
        params: Vec<u8>,
        sender: PublicKey,
        secret_key: &SecretKey,
    ) -> anyhow::Result<TransactionReceipt> {
        let action = Action::new(contract, action, params);
        let sender_addr = Address::from_public_key(&sender);
        let tx = Transaction::new(sender_addr, u64::MAX, vec![action]);
        let digest = tx.digest();
        let signature = Signature::new(&digest, secret_key);
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
        let value = self.post_json(&body).await?;
        let result = value
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("lightpool rpc missing result"))?;
        let parsed: SubmitResponse = serde_json::from_value(result)?;
        Ok(parsed.receipt)
    }

    pub async fn fetch_bridge_config(
        &self,
        contract: ContractAddress,
    ) -> anyhow::Result<BridgeConfig> {
        let bytes = self
            .call(
                contract,
                Action::new(contract, GET_CONFIG_ACTION, Vec::new()),
            )
            .await?;
        bincode::deserialize(&bytes).map_err(|e| anyhow::anyhow!("decode bridge config: {}", e))
    }

    pub async fn fetch_withdraw_record(
        &self,
        contract: ContractAddress,
        lane_index: u32,
        nonce: u64,
    ) -> anyhow::Result<BridgeWithdrawRecord> {
        #[derive(Serialize)]
        struct Params {
            lane_index: u32,
            nonce: u64,
        }
        let bytes = self
            .call(
                contract,
                Action::new(
                    contract,
                    GET_WITHDRAW_ACTION,
                    bincode::serialize(&Params { lane_index, nonce })?,
                ),
            )
            .await?;
        bincode::deserialize(&bytes).map_err(|e| anyhow::anyhow!("decode withdraw record: {}", e))
    }

    pub async fn fetch_outbound_config(
        &self,
        contract: ContractAddress,
    ) -> anyhow::Result<OutboundBridgeConfig> {
        let bytes = self
            .call(
                contract,
                Action::new(contract, GET_CONFIG_ACTION, Vec::new()),
            )
            .await?;
        bincode::deserialize(&bytes)
            .map_err(|e| anyhow::anyhow!("decode outbound bridge config: {}", e))
    }

    pub async fn fetch_outbound_withdraw(
        &self,
        contract: ContractAddress,
        nonce: u64,
    ) -> anyhow::Result<OutboundWithdrawRecord> {
        #[derive(Serialize)]
        struct Params {
            nonce: u64,
        }
        let bytes = self
            .call(
                contract,
                Action::new(
                    contract,
                    GET_OUTBOUND_WITHDRAW_ACTION,
                    bincode::serialize(&Params { nonce })?,
                ),
            )
            .await?;
        bincode::deserialize(&bytes)
            .map_err(|e| anyhow::anyhow!("decode outbound withdraw: {}", e))
    }

    async fn post_json(&self, body: &Value) -> anyhow::Result<Value> {
        let response = self
            .http
            .post(&self.rpc_url)
            .json(body)
            .send()
            .await?
            .error_for_status()?;
        let value: Value = response.json().await?;
        if let Some(err) = value.get("error") {
            return Err(anyhow::anyhow!("lightpool rpc error: {}", err));
        }
        Ok(value)
    }
}

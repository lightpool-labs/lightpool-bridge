// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use lightpool_crypto::{Keccak256, SecretKey};
use ethereum_types::{Address as EthAddress, U256};
use ethabi::{encode, Token};
use k256::ecdsa::{RecoveryId, Signature as K256Signature, SigningKey};
use lightpool_types::module_types::bridge::{BridgeAuthority, BridgeWithdrawRecord};
use std::process::Stdio;
use tokio::process::Command;
use anyhow::{anyhow, Context, Result};


#[derive(Debug, Clone)]
pub struct EthSignature {
    pub v: u8,
    pub r: [u8; 32],
    pub s: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct EvmCommittee {
    pub epoch: u64,
    pub validators: Vec<[u8; 20]>,
    pub stakes: Vec<u64>,
}

impl EvmCommittee {
    pub fn from_authorities(epoch: u64, authorities: &[BridgeAuthority]) -> Self {
        let mut members: Vec<( [u8; 20], u64 )> = authorities
            .iter()
            .map(|a| (a.consensus_pubkey.to_ethereum_address(), a.stake))
            .collect();
        members.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            epoch,
            validators: members.iter().map(|(a, _)| *a).collect(),
            stakes: members.iter().map(|(_, s)| *s).collect(),
        }
    }

    pub fn from_validator_committee(committee: &lightpool_types::Committee) -> Self {
        let mut members: Vec<([u8; 20], u64)> = committee
            .authorities
            .iter()
            .filter(|(_, authority)| authority.stake > 0)
            .map(|(pk, authority)| (pk.to_ethereum_address(), authority.stake))
            .collect();
        members.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            epoch: committee.epoch as u64,
            validators: members.iter().map(|(a, _)| *a).collect(),
            stakes: members.iter().map(|(_, s)| *s).collect(),
        }
    }
}

pub fn withdraw_id(
    nonce: u64,
    user: [u8; 20],
    destination: [u8; 20],
    amount: u64,
    epoch: u64,
) -> [u8; 32] {
    let encoded = encode(&[
        Token::Uint(U256::from(nonce)),
        Token::Address(EthAddress::from(user)),
        Token::Address(EthAddress::from(destination)),
        Token::Uint(U256::from(amount)),
        Token::Uint(U256::from(epoch)),
    ]);
    Keccak256::digest(encoded)
}

pub fn request_withdraw_digest(
    id: [u8; 32],
    user: [u8; 20],
    destination: [u8; 20],
    token: [u8; 20],
    amount: u64,
    nonce: u64,
    epoch: u64,
) -> [u8; 32] {
    let encoded = encode(&[
        Token::String("requestWithdraw".into()),
        Token::FixedBytes(id.to_vec()),
        Token::Address(EthAddress::from(user)),
        Token::Address(EthAddress::from(destination)),
        Token::Address(EthAddress::from(token)),
        Token::Uint(U256::from(amount)),
        Token::Uint(U256::from(nonce)),
        Token::Uint(U256::from(epoch)),
    ]);
    Keccak256::digest(encoded)
}

pub fn eth_sign_digest(digest: [u8; 32], secret: &SecretKey) -> Result<EthSignature> {
    let mut prefixed = Vec::with_capacity(2 + 26 + 32);
    prefixed.extend_from_slice(b"\x19Ethereum Signed Message:\n32");
    prefixed.extend_from_slice(&digest);
    let eth_signed = Keccak256::digest(prefixed);

    let signing_key = secret
        .signing_key()
        .map_err(|e| anyhow!("invalid secret key: {}", e))?;
    let (sig, recid): (K256Signature, RecoveryId) = SigningKey::sign_prehash_recoverable(
        &signing_key,
        &eth_signed,
    )
    .map_err(|e| anyhow!("eth sign failed: {}", e))?;
    let bytes = sig.to_bytes();
    let mut r = [0u8; 32];
    let mut s = [0u8; 32];
    r.copy_from_slice(&bytes[..32]);
    s.copy_from_slice(&bytes[32..]);
    Ok(EthSignature {
        v: recid.to_byte() + 27,
        r,
        s,
    })
}

pub fn secret_key_hex(secret: &SecretKey) -> Result<String> {
    let raw = base64::decode(secret.encode_base64())
        .map_err(|e| anyhow!("secret key base64 decode: {}", e))?;
    if raw.len() != 32 {
        return Err(anyhow!("secret key must be 32 bytes"));
    }
    Ok(format!("0x{}", hex::encode(raw)))
}

fn addr_hex(a: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(a))
}

fn bytes32_hex(b: &[u8; 32]) -> String {
    format!("0x{}", hex::encode(b))
}

pub async fn eth_block_number(rpc: &str) -> Result<u64> {
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(rpc)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_blockNumber",
            "params": [],
        }))
        .send()
        .await
        .context("eth_blockNumber request failed")?
        .json()
        .await
        .context("eth_blockNumber response parse failed")?;
    let hex = resp
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("eth_blockNumber missing result"))?;
    u64::from_str_radix(hex.trim_start_matches("0x"), 16)
        .map_err(|err| anyhow!("invalid eth_blockNumber {hex}: {err}"))
}

async fn eth_call_u64(rpc: &str, to: &str, data: &str) -> Result<u64> {
    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(rpc)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_call",
            "params": [{ "to": to, "data": data }, "latest"],
        }))
        .send()
        .await
        .context("eth_call request failed")?
        .json()
        .await
        .context("eth_call response parse failed")?;
    if let Some(err) = resp.get("error") {
        return Err(anyhow!("eth_call error: {err}"));
    }
    let hex = resp
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("eth_call missing result"))?;
    let body = hex.trim_start_matches("0x");
    if body.is_empty() {
        return Err(anyhow!("eth_call empty result"));
    }
    u64::from_str_radix(&body[body.len().saturating_sub(16)..], 16)
        .map_err(|err| anyhow!("invalid eth_call u64 {hex}: {err}"))
}

/// On-chain Bridge dispute parameters (DeployLocal: 5s, 1000ms).
#[derive(Debug, Clone, Copy)]
pub struct EvmDisputeParams {
    pub period_seconds: u64,
    pub block_duration_millis: u64,
}

impl Default for EvmDisputeParams {
    fn default() -> Self {
        Self {
            period_seconds: 5,
            block_duration_millis: 1000,
        }
    }
}

pub async fn fetch_dispute_params(rpc: &str, bridge: &str) -> Result<EvmDisputeParams> {
    let period_seconds = eth_call_u64(rpc, bridge, "0x0756183b").await?;
    let block_duration_millis = eth_call_u64(rpc, bridge, "0x9d5bc9e1").await?;
    Ok(EvmDisputeParams {
        period_seconds,
        block_duration_millis,
    })
}

/// Blocks that must elapse after request before Bridge.finalizeWithdraw can succeed.
/// Mirrors Bridge._inDispute: need elapsedBlocks * blockDurationMillis > 1000 * disputePeriodSeconds.
pub fn dispute_block_delay(params: EvmDisputeParams, confirmations: u64) -> u64 {
    let from_dispute = if params.block_duration_millis == 0 {
        params.period_seconds.saturating_add(1)
    } else {
        (1000 * params.period_seconds) / params.block_duration_millis + 1
    };
    from_dispute.max(confirmations.max(1))
}

pub fn still_in_dispute_error(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("0x2d3666c9") || lower.contains("stillindispute")
}

pub async fn cast_request_withdraw(
    cast_bin: &str,
    rpc: &str,
    private_key_hex: &str,
    bridge: &str,
    id: [u8; 32],
    record: &BridgeWithdrawRecord,
    foreign_token: [u8; 20],
    epoch: u64,
    committee: &EvmCommittee,
    signatures: &[EthSignature],
) -> Result<String> {
    let user = *record.sender.as_bytes();
    let req = format!(
        "({},{},{},{},{},{},{})",
        bytes32_hex(&id),
        addr_hex(&user),
        addr_hex(&record.foreign_recipient),
        addr_hex(&foreign_token),
        record.amount,
        record.nonce,
        epoch
    );
    let validators = committee
        .validators
        .iter()
        .map(addr_hex)
        .collect::<Vec<_>>()
        .join(",");
    let stakes = committee
        .stakes
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let active = format!("({},[{}],[{}])", committee.epoch, validators, stakes);
    let sigs = signatures
        .iter()
        .map(|s| format!("({},{},{})", s.v, bytes32_hex(&s.r), bytes32_hex(&s.s)))
        .collect::<Vec<_>>()
        .join(",");
    let sigs_arg = format!("[{}]", sigs);

    let output = Command::new(cast_bin)
        .args([
            "send",
            bridge,
            "requestWithdraw((bytes32,address,address,address,uint64,uint64,uint64),(uint64,address[],uint64[]),(uint8,bytes32,bytes32)[])",
            &req,
            &active,
            &sigs_arg,
            "--rpc-url",
            rpc,
            "--private-key",
            private_key_hex,
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to spawn cast for requestWithdraw")?;
    if !output.status.success() {
        return Err(anyhow!(
            "cast requestWithdraw failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub async fn cast_finalize_withdraw(
    cast_bin: &str,
    rpc: &str,
    private_key_hex: &str,
    bridge: &str,
    id: [u8; 32],
) -> Result<String> {
    let output = Command::new(cast_bin)
        .args([
            "send",
            bridge,
            "finalizeWithdraw(bytes32)",
            &bytes32_hex(&id),
            "--rpc-url",
            rpc,
            "--private-key",
            private_key_hex,
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to spawn cast for finalizeWithdraw")?;
    if !output.status.success() {
        return Err(anyhow!(
            "cast finalizeWithdraw failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

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

pub async fn cast_request_withdraw(
    cast_bin: &str,
    rpc: &str,
    private_key_hex: &str,
    bridge: &str,
    id: [u8; 32],
    record: &BridgeWithdrawRecord,
    evm_token: [u8; 20],
    epoch: u64,
    committee: &EvmCommittee,
    signatures: &[EthSignature],
) -> Result<String> {
    let user = *record.sender.as_bytes();
    let req = format!(
        "({},{},{},{},{},{},{})",
        bytes32_hex(&id),
        addr_hex(&user),
        addr_hex(&record.evm_recipient),
        addr_hex(&evm_token),
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

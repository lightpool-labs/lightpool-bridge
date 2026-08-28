// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use lightpool_types::contract::ContractAddress;
use lightpool_types::module::Module;
use lightpool_types::module_types::bridge::{
    BridgeConfig, BridgeWithdrawRecord, InboundTokenLane, OutboundBridgeConfig, OutboundTokenLane,
};

use crate::route_config::{BridgeRoute, ForeignLeg};

pub fn default_inbound_contract() -> ContractAddress {
    let mut rest = [0u8; 7];
    rest.copy_from_slice(&1u64.to_be_bytes()[1..]);
    ContractAddress::new(Module::BRIDGE, rest)
}

pub fn parse_contract_address(raw: &str) -> anyhow::Result<ContractAddress> {
    let hex = raw.trim().trim_start_matches("0x");
    if hex.is_empty() {
        return Ok(default_inbound_contract());
    }
    let bytes = hex::decode(hex)?;
    if bytes.len() != ContractAddress::CONTRACT_ADDRESS_LENGTH {
        return Err(anyhow::anyhow!(
            "contract address must be {} bytes, got {}",
            ContractAddress::CONTRACT_ADDRESS_LENGTH,
            bytes.len()
        ));
    }
    let mut arr = [0u8; ContractAddress::CONTRACT_ADDRESS_LENGTH];
    arr.copy_from_slice(&bytes);
    Ok(ContractAddress::from_bytes(arr))
}

pub fn evm_address_hex(addr: &[u8; 20]) -> String {
    format!("0x{}", hex::encode(addr))
}

pub fn parse_evm_address(raw: &str) -> anyhow::Result<[u8; 20]> {
    let hex = raw.trim().trim_start_matches("0x");
    let bytes = hex::decode(hex)?;
    if bytes.len() != 20 {
        return Err(anyhow::anyhow!("evm address must be 20 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub fn topic_u64(topic: &str) -> anyhow::Result<u64> {
    let hex_body = topic.trim_start_matches("0x");
    if hex_body.len() < 16 {
        return Err(anyhow::anyhow!("topic too short for u64"));
    }
    Ok(u64::from_str_radix(&hex_body[hex_body.len() - 16..], 16)?)
}

pub fn topic_address(topic: &str) -> anyhow::Result<[u8; 20]> {
    let hex_body = topic.trim_start_matches("0x");
    if hex_body.len() < 40 {
        return Err(anyhow::anyhow!("topic too short for address"));
    }
    let bytes = hex::decode(&hex_body[hex_body.len() - 40..])?;
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub fn u64_from_word(word: &[u8]) -> anyhow::Result<u64> {
    if word.len() != 32 {
        return Err(anyhow::anyhow!("expected 32-byte word"));
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&word[24..32]);
    Ok(u64::from_be_bytes(buf))
}

pub fn address_from_word(word: &[u8]) -> anyhow::Result<[u8; 20]> {
    if word.len() != 32 {
        return Err(anyhow::anyhow!("expected 32-byte word"));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&word[12..32]);
    Ok(out)
}

pub fn parse_b32(value: &str) -> anyhow::Result<[u8; 32]> {
    let hex_body = value.trim_start_matches("0x");
    let bytes = hex::decode(hex_body)?;
    if bytes.len() != 32 {
        return Err(anyhow::anyhow!("expected 32-byte hash"));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub fn lock_source_hash(chain_id: u64, bridge: ContractAddress, nonce: u64) -> [u8; 32] {
    use lightpool_crypto::Keccak256;
    let mut buf = Vec::with_capacity(16);
    buf.extend_from_slice(&chain_id.to_be_bytes());
    buf.extend_from_slice(bridge.as_bytes());
    buf.extend_from_slice(&nonce.to_be_bytes());
    Keccak256::digest(buf)
}

pub fn foreign_token_bytes(raw: &str) -> anyhow::Result<[u8; 20]> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow::anyhow!("foreign token is empty"));
    }
    if trimmed.contains("0x02") || trimmed.len() <= 34 {
        let contract = parse_contract_address(trimmed)?;
        let bytes = contract.as_bytes();
        let mut out = [0u8; 20];
        out[20 - bytes.len()..].copy_from_slice(bytes);
        return Ok(out);
    }
    parse_evm_address(trimmed)
}

pub fn inbound_lane_for_route<'a>(
    cfg: &'a BridgeConfig,
    route: &BridgeRoute,
) -> anyhow::Result<&'a InboundTokenLane> {
    if !route.local_inbound.lp_token.trim().is_empty() {
        let lp = parse_contract_address(&route.local_inbound.lp_token)?;
        if let Some(lane) = cfg.lane_by_lp_token(lp) {
            return Ok(lane);
        }
    }
    match &route.foreign {
        ForeignLeg::Evm { token_address, .. } | ForeignLeg::Lightpool { foreign_token: token_address, .. } => {
            if !token_address.trim().is_empty() {
                let foreign = foreign_token_bytes(token_address)?;
                if let Some(lane) = cfg.lane_by_foreign_token(foreign) {
                    return Ok(lane);
                }
            }
        }
    }
    Err(anyhow::anyhow!("no inbound lane matched for route {}", route.id))
}

pub fn contract_to_foreign_token(token: ContractAddress) -> [u8; 20] {
    let bytes = token.as_bytes();
    let mut out = [0u8; 20];
    out[20 - bytes.len()..].copy_from_slice(bytes);
    out
}

pub fn outbound_lane_for_inbound_withdraw<'a>(
    cfg: &'a OutboundBridgeConfig,
    record: &BridgeWithdrawRecord,
) -> anyhow::Result<&'a OutboundTokenLane> {
    let foreign_ref = contract_to_foreign_token(record.token);
    if let Some(lane) = cfg.lane_by_foreign_token(foreign_ref) {
        return Ok(lane);
    }
    if let Some(lane) = cfg.lane_by_index(record.lane_index) {
        return Ok(lane);
    }
    cfg.lane_by_lp_token(record.token).ok_or_else(|| {
        anyhow::anyhow!(
            "no outbound lane for local token {} (lane_index={})",
            record.token,
            record.lane_index
        )
    })
}

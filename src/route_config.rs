// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use serde::{Deserialize, Serialize};

use crate::config::{BridgeConfigError, BridgeLinkConfig};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalChainConfig {
    #[serde(default = "default_lightpool_rpc")]
    pub rpc_url: String,
    #[serde(default = "default_local_chain_id")]
    pub chain_id: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalInboundConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bridge_contract: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lp_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForeignLeg {
    Evm {
        rpc_url: String,
        chain_id: u64,
        bridge_address: String,
        token_address: String,
        #[serde(default = "default_confirmations")]
        confirmations: u64,
        #[serde(default)]
        start_block: u64,
    },
    Lightpool {
        rpc_url: String,
        chain_id: u64,
        outbound_bridge_contract: String,
        foreign_token: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeRoute {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub local_inbound: LocalInboundConfig,
    pub foreign: ForeignLeg,
}

fn default_true() -> bool {
    true
}

fn default_lightpool_rpc() -> String {
    "http://127.0.0.1:26300".to_string()
}

fn default_confirmations() -> u64 {
    1
}

fn default_local_chain_id() -> u64 {
    1
}

impl Default for LocalChainConfig {
    fn default() -> Self {
        Self {
            rpc_url: default_lightpool_rpc(),
            chain_id: default_local_chain_id(),
        }
    }
}

impl BridgeRoute {
    pub fn legacy_reth(id: &str, cfg: &BridgeLinkConfig) -> Self {
        Self {
            id: id.to_string(),
            enabled: true,
            local_inbound: LocalInboundConfig {
                bridge_contract: String::new(),
                lp_token: String::new(),
            },
            foreign: ForeignLeg::Evm {
                rpc_url: cfg.evm_rpc_url.clone(),
                chain_id: 1337,
                bridge_address: cfg.evm_bridge_address.clone(),
                token_address: String::new(),
                confirmations: cfg.evm_confirmations,
                start_block: cfg.start_block,
            },
        }
    }
}

pub fn validate_config(cfg: &BridgeLinkConfig) -> Result<(), String> {
    if cfg.routes.is_empty() {
        return Ok(());
    }
    let mut seen = std::collections::HashSet::new();
    for route in &cfg.routes {
        let id = route.id.trim();
        if id.is_empty() {
            return Err("route id must not be empty".to_string());
        }
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate route id: {}", id));
        }
        match &route.foreign {
            ForeignLeg::Evm {
                rpc_url,
                bridge_address,
                ..
            } => {
                if route.enabled && rpc_url.trim().is_empty() {
                    return Err(format!("route {}: evm rpc_url is empty", id));
                }
                if route.enabled && bridge_address.trim().is_empty() {
                    return Err(format!("route {}: evm bridge_address is empty", id));
                }
            }
            ForeignLeg::Lightpool {
                rpc_url,
                outbound_bridge_contract,
                foreign_token,
                ..
            } => {
                if route.enabled && rpc_url.trim().is_empty() {
                    return Err(format!("route {}: lightpool rpc_url is empty", id));
                }
                if route.enabled && outbound_bridge_contract.trim().is_empty() {
                    return Err(format!(
                        "route {}: outbound_bridge_contract is empty",
                        id
                    ));
                }
                if route.enabled && foreign_token.trim().is_empty() {
                    return Err(format!("route {}: foreign_token is empty", id));
                }
            }
        }
    }
    Ok(())
}

impl BridgeLinkConfig {
    pub fn normalize_routes(&mut self) {
        if self.local.rpc_url.is_empty() {
            self.local.rpc_url = self.lightpool_rpc_url.clone();
        }
        if self.lightpool_rpc_url.is_empty() {
            self.lightpool_rpc_url = self.local.rpc_url.clone();
        }

        if self.routes.is_empty() && !self.evm_bridge_address.trim().is_empty() {
            self.routes.push(BridgeRoute::legacy_reth("reth-default", self));
        }

        if let Some(route) = self
            .routes
            .iter()
            .find(|r| r.enabled)
            .or_else(|| self.routes.first())
        {
            if let ForeignLeg::Evm {
                rpc_url,
                bridge_address,
                confirmations,
                start_block,
                ..
            } = &route.foreign
            {
                if self.evm_rpc_url.is_empty() {
                    self.evm_rpc_url = rpc_url.clone();
                }
                if self.evm_bridge_address.is_empty() {
                    self.evm_bridge_address = bridge_address.clone();
                }
                self.evm_confirmations = *confirmations;
                self.start_block = *start_block;
            }
        }
    }

    pub fn write(&self, path: impl AsRef<std::path::Path>) -> Result<(), BridgeConfigError> {
        validate_config(self).map_err(|message| BridgeConfigError::Validation {
            path: path.as_ref().display().to_string(),
            message,
        })?;
        let path = path.as_ref();
        let bytes = serde_json::to_vec_pretty(self).map_err(|err| {
            BridgeConfigError::Validation {
                path: path.display().to_string(),
                message: err.to_string(),
            }
        })?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes).map_err(|source| BridgeConfigError::Write {
            path: tmp.display().to_string(),
            source,
        })?;
        std::fs::rename(&tmp, path).map_err(|source| BridgeConfigError::Write {
            path: path.display().to_string(),
            source,
        })?;
        Ok(())
    }
}

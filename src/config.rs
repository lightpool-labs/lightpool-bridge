// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use lightpool_crypto::{derive_public_key_from_secret, PublicKey, SecretKey};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use thiserror::Error;

use crate::route_config::{BridgeRoute, LocalChainConfig};

#[derive(Debug, Error)]
pub enum BridgeConfigError {
    #[error("failed to read bridge config '{path}': {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse bridge config '{path}': {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to read wallet '{path}': {source}")]
    WalletRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse wallet '{path}': {source}")]
    WalletParse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid wallet '{path}': {message}")]
    InvalidWallet { path: String, message: String },
    #[error("invalid bridge config '{path}': {message}")]
    Validation { path: String, message: String },
    #[error("failed to write bridge config '{path}': {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeLinkConfig {
    #[serde(default)]
    pub wallet_path: String,
    #[serde(default)]
    pub evm_rpc_url: String,
    #[serde(default)]
    pub evm_bridge_address: String,
    #[serde(default = "default_confirmations")]
    pub evm_confirmations: u64,
    #[serde(default = "default_lightpool_rpc")]
    pub lightpool_rpc_url: String,
    /// Foundry cast binary used to submit EVM txs (request/finalize withdraw).
    #[serde(default = "default_cast_bin")]
    pub cast_bin: String,
    #[serde(default)]
    pub local: LocalChainConfig,
    #[serde(default)]
    pub routes: Vec<BridgeRoute>,
}

#[derive(Debug, Deserialize)]
struct WalletFile {
    private_key: String,
}

fn default_lightpool_rpc() -> String {
    "http://127.0.0.1:26300".to_string()
}

fn default_confirmations() -> u64 {
    1
}

fn default_cast_bin() -> String {
    "cast".to_string()
}

impl Default for BridgeLinkConfig {
    fn default() -> Self {
        Self {
            wallet_path: String::new(),
            evm_rpc_url: String::new(),
            evm_bridge_address: String::new(),
            evm_confirmations: default_confirmations(),
            lightpool_rpc_url: default_lightpool_rpc(),
            cast_bin: default_cast_bin(),
            local: LocalChainConfig::default(),
            routes: Vec::new(),
        }
    }
}

impl BridgeLinkConfig {
    pub fn read(path: impl AsRef<Path>) -> Result<Self, BridgeConfigError> {
        let path_str = path.as_ref().display().to_string();
        let data = fs::read(path.as_ref()).map_err(|source| BridgeConfigError::Read {
            path: path_str.clone(),
            source,
        })?;
        let mut config: Self = serde_json::from_slice(&data).map_err(|source| {
            BridgeConfigError::Parse {
                path: path_str,
                source,
            }
        })?;
        config.normalize_routes();
        Ok(config)
    }

    pub fn load_wallet(&self) -> Result<(PublicKey, SecretKey), BridgeConfigError> {
        if self.wallet_path.is_empty() {
            return Err(BridgeConfigError::InvalidWallet {
                path: self.wallet_path.clone(),
                message: "wallet_path is empty".to_string(),
            });
        }
        let path = &self.wallet_path;
        let data = fs::read(path).map_err(|source| BridgeConfigError::WalletRead {
            path: path.clone(),
            source,
        })?;
        let wallet: WalletFile =
            serde_json::from_slice(&data).map_err(|source| BridgeConfigError::WalletParse {
                path: path.clone(),
                source,
            })?;
        let secret_key = secret_key_from_hex(path, &wallet.private_key)?;
        let public_key = derive_public_key_from_secret(&secret_key).map_err(|e| {
            BridgeConfigError::InvalidWallet {
                path: path.clone(),
                message: format!("failed to derive public key: {}", e),
            }
        })?;
        Ok((public_key, secret_key))
    }
}

fn secret_key_from_hex(path: &str, hex_str: &str) -> Result<SecretKey, BridgeConfigError> {
    let bytes = hex::decode(hex_str.trim()).map_err(|e| BridgeConfigError::InvalidWallet {
        path: path.to_string(),
        message: format!("invalid private_key hex: {}", e),
    })?;
    if bytes.len() != 32 {
        return Err(BridgeConfigError::InvalidWallet {
            path: path.to_string(),
            message: format!("private_key must be 32 bytes, got {}", bytes.len()),
        });
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    let encoded = base64::encode(arr);
    SecretKey::decode_base64(&encoded).map_err(|e| BridgeConfigError::InvalidWallet {
        path: path.to_string(),
        message: format!("invalid secret key: {}", e),
    })
}

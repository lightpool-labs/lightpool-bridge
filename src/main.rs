// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use anyhow::{bail, Context, Result};
use clap::Parser;
use lightpool_bridge::{spawn_bridge_link, BridgeLinkConfig};
use lightpool_types::Committee;
use log::info;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(name = "lightpool-bridge", about = "Off-chain LightPool bridge process")]
struct Args {
    /// Bridge config JSON
    #[arg(long, value_name = "FILE")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let config = BridgeLinkConfig::read(&args.config)
        .with_context(|| format!("failed to read config {}", args.config))?;

    if !config.enabled {
        bail!("bridge config has enabled=false; refusing to start");
    }
    if config.evm_bridge_address.is_empty() {
        bail!("evm_bridge_address is empty");
    }

    let (name, secret_key) = config.load_wallet().context("failed to load wallet")?;
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Received Ctrl-C, shutting down bridge");
        cancel_clone.cancel();
    });

    let committee = Committee::new(Vec::new(), 0);
    let Some(handle) = spawn_bridge_link(config, name, secret_key, committee, cancel) else {
        bail!("Bridge failed to start");
    };
    handle.await.context("Bridge task joined with error")?;
    Ok(())
}

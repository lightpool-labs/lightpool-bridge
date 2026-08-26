// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use lightpool_bridge::spawn_bridge_link;
use lightpool_types::Committee;
use log::info;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(name = "lightpool-bridge", about = "Off-chain LightPool bridge process")]
struct Args {
    /// Bridge config JSON
    #[arg(long, value_name = "FILE")]
    config: PathBuf,
    /// Admin UI listen address (embedded in bridge process)
    #[arg(long, default_value = "127.0.0.1:8787")]
    admin_listen: SocketAddr,
    /// Disable embedded admin UI
    #[arg(long)]
    no_admin: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let config = lightpool_bridge::BridgeLinkConfig::read(&args.config)
        .with_context(|| format!("failed to read config {}", args.config.display()))?;

    if !config.enabled {
        anyhow::bail!("bridge config has enabled=false; refusing to start");
    }

    let (name, secret_key) = config.load_wallet().context("failed to load wallet")?;
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Received Ctrl-C, shutting down bridge");
        cancel_clone.cancel();
    });

    let admin_listen = if args.no_admin {
        None
    } else {
        Some(args.admin_listen)
    };

    let committee = Committee::new(Vec::new(), 0);
    let _handle = spawn_bridge_link(
        args.config,
        config,
        name,
        secret_key,
        committee,
        admin_listen,
        cancel.clone(),
    )
    .context("Bridge failed to start")?;

    cancel.cancelled().await;
    Ok(())
}

// Copyright (c) LightPool Labs
// Author: xiaoyu1998

use std::path::PathBuf;

use lightpool_crypto::{PublicKey, SecretKey};
use lightpool_types::Committee;
use tokio_util::sync::CancellationToken;

use crate::config::BridgeLinkConfig;
use crate::handle::BridgeHandle;
use crate::router::spawn_bridge_router;

pub use crate::handle::BridgeStatusResponse;
pub use crate::router::BridgeRouter as BridgeLinkService;

pub fn spawn_bridge_link(
    config_path: PathBuf,
    config: BridgeLinkConfig,
    name: PublicKey,
    secret_key: SecretKey,
    committee: Committee,
    admin_listen: Option<std::net::SocketAddr>,
    cancel: CancellationToken,
) -> anyhow::Result<BridgeHandle> {
    spawn_bridge_router(
        config_path,
        config,
        name,
        secret_key,
        committee,
        admin_listen,
        cancel,
    )
}

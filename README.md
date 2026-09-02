# lightpool-bridge

Standalone off-chain bridge link for LightPool: watches foreign chains and local LightPool, collects validator signatures, and submits confirmations / unlocks. Includes EVM hub contracts under `contracts/` and an embedded Admin UI.

## Architecture

![LightPool bridge architecture](docs/images/bridge.jpg)

## Layout

- `src/` — Rust binary `lightpool-bridge` (router, EVM/LP WebSocket subscribers, Admin API)
- `admin/static/` — embedded Admin UI
- `contracts/` — Foundry EVM Bridge hub + deploy scripts
- `docs/LOCAL_E2E_BRIDGE_SETUP.md` — end-to-end local setup
- `docs/images/` — architecture diagrams
- `bridge.config.example.json` — config template

## Build

```bash
cd lightpool-bridge
cargo build --release
```

## Quick start

See [`docs/LOCAL_E2E_BRIDGE_SETUP.md`](docs/LOCAL_E2E_BRIDGE_SETUP.md).

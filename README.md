# lightpool-bridge

Standalone off-chain bridge link for LightPool: watches foreign chains and local LightPool, collects validator signatures, and submits confirmations / unlocks. Includes EVM hub contracts under `contracts/` and an embedded Admin UI.

## Architecture

```text
   ┌────────────────────────┐     ┌────────────────────────┐
   │      Reth (EVM)        │     │   Foreign LightPool    │
   │  RPC :8545  WS :8546   │     │  RPC :27300  WS :27400 │
   │  Bridge hub            │     │  outbound hub          │
   └───────────┬────────────┘     └───────────┬────────────┘
               │                              │
               │  WS DepositInitiated         │  WS outbound withdraw
               │  cast request/finalize       │  outbound deposit
               │                              │
               └──────────────┬───────────────┘
                              ▼
                ┌─────────────────────────────┐
                │      lightpool-bridge       │
                │  Admin UI :8787 · routes    │
                └──────────────┬──────────────┘
                               │
                               │  WS inbound withdraw
                               │  confirm_dep
                               ▼
                ┌─────────────────────────────┐
                │     Local LightPool         │
                │  RPC :26300  WS :26400      │
                │  inbound hub                │
                └─────────────────────────────┘
```

- **Local LightPool** — inbound hub (RPC/WS above)
- **EVM (Reth)** — Bridge hub
- **Foreign LightPool** — outbound hub
- **lightpool-bridge** — Admin UI :8787 · routes

## Layout

- `src/` — Rust binary `lightpool-bridge` (router, EVM/LP WebSocket subscribers, Admin API)
- `admin/static/` — embedded Admin UI
- `contracts/` — Foundry EVM Bridge hub + deploy scripts
- `docs/LOCAL_E2E_BRIDGE_SETUP.md` — end-to-end local setup
- `bridge.config.example.json` — config template

## Build

```bash
cd lightpool-bridge
cargo build --release
```

## Quick start

See [`docs/LOCAL_E2E_BRIDGE_SETUP.md`](docs/LOCAL_E2E_BRIDGE_SETUP.md).

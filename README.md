# lightpool-bridge

Standalone off-chain bridge link for LightPool: watches foreign chains and local LightPool, collects validator signatures, and submits confirmations / unlocks. Includes EVM hub contracts under `contracts/` and an embedded Admin UI.

## Architecture

Multi-token **hub** model: one bridge hub per leg, many token lanes; **lightpool-bridge** runs **one route per lane**.

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

| Direction | Watch | Submit |
| --- | --- | --- |
| **EVM → local LP** | Reth WS `DepositInitiated` (per token) | local `confirm_dep` |
| **Local LP → EVM** | local WS inbound `withdraw` | EVM `requestWithdraw`, then `finalizeWithdraw` after dispute |
| **Foreign LP → local** | foreign WS outbound `withdraw` | local `confirm_dep` |
| **Local → foreign LP** | local WS inbound `withdraw` | foreign outbound `deposit` |

- **Not** part of the LightPool node — separate process.
- **Contracts** (`contracts/`): multi-token EVM `Bridge` hub + MockUSDT (Foundry).
- LightPool hubs are created via node CLI (`init-bridge`, `create-outbound-bridge`); extra lanes via `reg_lane` (**hub owner only**).

## Docs

- [`docs/LOCAL_E2E_BRIDGE_SETUP.md`](docs/LOCAL_E2E_BRIDGE_SETUP.md) — full local setup: Reth + local LP + foreign LP + Admin routes + deposit/withdraw + reset

## Quick start

Requires `lightpool-node` and this repo under `~/work/lightpool-labs/`. Prefer the doc above for the full 4-terminal flow.

```bash
export WORK=~/work/lightpool-labs
export NODE=$WORK/lightpool-node
export BRIDGE=$WORK/lightpool-bridge

cd "$BRIDGE" && cargo build --release

# After Reth + local (+ optional foreign) nodes are up and bootstrap scripts have run:
"$BRIDGE/target/release/lightpool-bridge" --config "$BRIDGE/bridge-config.json"
```

Admin UI: **http://127.0.0.1:8787** (disable with `--no-admin`).

Bootstrap scripts live in `lightpool-node`:

- `scripts/event-contract-setup/00_bridge_bootstrap.py` — Reth Bridge + local inbound hub
- `scripts/event-contract-setup/01_lp_foreign_bootstrap.py` — foreign outbound + paired local inbound

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

Contracts:

```bash
cd contracts
forge install foundry-rs/forge-std   # once
forge build
```

## Config

Routes are usually added in the Admin UI. Minimal file shape:

```json
{
  "wallet_path": "/home/you/.lightpool/wallet.json",
  "cast_bin": "cast",
  "local": {
    "rpc_url": "http://127.0.0.1:26300",
    "chain_id": 1
  },
  "routes": []
}
```

Each route pairs a **local inbound** bridge/LP token with either an **EVM** foreign leg or a **LightPool** foreign leg (outbound hub). See the setup doc for field mapping from `.env.bridge` / `.env.lp-foreign`.

Config hot-reload via the UI; restart the process after changing `wallet_path`.

## Related

- LightPool node + bootstrap: [lightpool-node](https://github.com/lightpool-labs/lightpool-node)
- Local Reth: `lightpool-node/tools/reth`

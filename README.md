# lightpool-bridge

Standalone off-chain bridge process for LightPool, plus EVM contracts under `contracts/`.

## Architecture

```text
┌─────────────┐   DepositInitiated    ┌──────────────────┐
│ lightpool-node/tools/reth │ ────────────────────► │ lightpool-bridge │
│ EVM :8545   │ ◄── request/finalize ─│   (this binary)  │
└─────────────┘         cast          └────────┬─────────┘
                                               │ HTTP
                                               ▼
                                      ┌────────────────┐
                                      │ lightpool node │
                                      │ RPC :26300     │
                                      └────────────────┘
```

- **Contracts** (`contracts/`): MockUSDT + Bridge.sol (Foundry)
- **Process** (`src/`): watches EVM logs, talks to LightPool RPC, submits `confirm_dep` / EVM withdraw txs
- **Not** inside the LightPool node — run as its own process

## Local test flow (4 terminals)

Local setup guides:

- [`docs/LOCAL_BRIDGE_RUNBOOK.md`](docs/LOCAL_BRIDGE_RUNBOOK.md) — **unified** local + Reth + foreign LightPool, Add route params, deposit/withdraw
- [`docs/RETH_FOREIGN_NODE.md`](docs/RETH_FOREIGN_NODE.md) — Reth (EVM) as foreign node
- [`docs/LIGHTPOOL_FOREIGN_NODE.md`](docs/LIGHTPOOL_FOREIGN_NODE.md) — second LightPool node as foreign node

Requires `lightpool-node` and this repo under `~/work/lightpool-labs/`.

```bash
export WORK=~/work/lightpool-labs
export NODE=$WORK/lightpool-node
export BRIDGE=$WORK/lightpool-bridge

# 1 — Reth
$NODE/tools/reth/run-dev.sh

# 2 — LightPool
cd $NODE && source ./env.sh && lightpool node --role validator

# 4 — Deploy (needs 1 + 2)
cd $NODE/scripts/event-contract-setup
python3 00_bridge_bootstrap.py --phase deploy

# 3 — Bridge process
$BRIDGE/target/release/lightpool-bridge --config $BRIDGE/bridge-config.json

# 4 — Create inbound bridge
python3 00_bridge_bootstrap.py --phase create
```

## Layout

- `src/` — Rust binary `lightpool-bridge`
- `contracts/` — Foundry EVM contracts and deploy scripts
- `docs/LOCAL_BRIDGE_RUNBOOK.md` — unified local + Reth + LP-foreign runbook
- `docs/RETH_FOREIGN_NODE.md` — Reth + LightPool + bridge runbook
- `docs/LIGHTPOOL_FOREIGN_NODE.md` — two LightPool nodes + bridge runbook
- `bridge.config.example.json` — config template

Embedded admin UI (default http://127.0.0.1:8787):

```bash
$BRIDGE/target/release/lightpool-bridge --config $BRIDGE/bridge-config.json
```

Disable with `--no-admin`. Config hot-reload via the UI; restart to pick up `wallet_path` changes.

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

```json
{
  "enabled": true,
  "wallet_path": "/path/to/wallet.json",
  "evm_rpc_url": "http://127.0.0.1:8545",
  "evm_bridge_address": "0x...",
  "lightpool_rpc_url": "http://127.0.0.1:26300",
  "poll_interval_ms": 1000,
  "dispute_period_seconds": 5
}
```

## Related

- LightPool node + bootstrap: [lightpool-node](https://github.com/lightpool-labs/lightpool-node)
- Local Reth: `lightpool-node/tools/reth` (inside the node repo)

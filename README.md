# lightpool-bridge

Standalone off-chain bridge process for LightPool, plus EVM contracts under `contracts/`.

## Architecture

```text
┌─────────────┐   DepositInitiated    ┌──────────────────┐
│ tools/reth  │ ────────────────────► │ lightpool-bridge │
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

Full step-by-step: [`docs/MANUAL_COMMANDS.md`](docs/MANUAL_COMMANDS.md)

```bash
# A — Reth
./tools/reth/run-dev.sh

# B — LightPool (no --bridge-config)
cd lightpool && cargo run -p lightpool --release -- node --role validator

# C — Bridge process (after deploy writes config)
cd lightpool-bridge && cargo run --release -- \
  --config ../tools/bridge-local/bridge-config.json

# D — Deploy / init / deposit CLI
cd lightpool/scripts/event-contract-setup
python3 00_bridge_bootstrap.py --phase deploy   # needs Reth + node
# start Terminal C, then:
python3 00_bridge_bootstrap.py --phase init
```

## Layout

- `src/` — Rust binary `lightpool-bridge`
- `contracts/` — Foundry EVM contracts and deploy scripts
- `docs/MANUAL_COMMANDS.md` — deposit / withdraw runbook
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

- LightPool on-chain bridge module: `lightpool/crates/lightpool-x/src/modules/bridge`
- Local Reth: `tools/reth`

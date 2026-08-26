# LightPool ↔ Reth bridge — local setup (Reth as foreign node)

Foundry required. Parent directory is `lightpool-labs`. Both repos live inside it:

```text
~/work/lightpool-labs/
├── lightpool-node/
│   ├── tools/reth/          → Terminal A
│   ├── store/               → LightPool chain data (created on first run)
│   └── scripts/event-contract-setup/
└── lightpool-bridge/        → Terminal C
```

Fresh checkout:

```bash
mkdir -p ~/work/lightpool-labs && cd ~/work/lightpool-labs
git clone https://github.com/lightpool-labs/lightpool-node.git
git clone https://github.com/lightpool-labs/lightpool-bridge.git
```

Four terminals (A–D). Run **Env** once per terminal.

**Full retest from zero:** stop C → B → A, run **Clear data** at the end of this doc, then **Build** → A → B → D deploy → C → D init → deposit → withdraw.

## Env

Paste this block alone. Do not append other commands on the same line.

```bash
export WORK=~/work/lightpool-labs
export NODE=$WORK/lightpool-node
export BRIDGE=$WORK/lightpool-bridge
export PATH="$HOME/.foundry/bin:$NODE/tools/reth/bin:$NODE/bin:$PATH"
export RETH_RPC=http://127.0.0.1:8545
export LP_RPC=http://127.0.0.1:26300
export CFG=$BRIDGE/bridge-config.json
export ENV_BRIDGE=$NODE/scripts/event-contract-setup/.env.bridge
export PK=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
export RECV=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
export RECV_PK=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d
export AMOUNT=100000000
```

## Reset — clean slate

Stop running processes first (**Ctrl+C**, reverse start order): Terminal C (bridge) → B (LightPool) → A (Reth). Then run **Clear data** at the end of this file (after **Env**).

After reset, continue with **Build** and the terminal flow below. LP USDT after a fresh `init` is expected at `0x0200000000000001`.

## Build

Run after **Env**, in a separate step (skip if binaries already built):

```bash
"$NODE/tools/reth/download.sh"
cd "$NODE" && cargo build --release && source ./env.sh
cd "$BRIDGE" && cargo build --release
```

## A — Reth

Reth must expose WebSocket on `:8546` (default with `run-dev.sh`). The bridge subscribes to `DepositInitiated` logs over WS; config `evm_rpc_url` stays `http://127.0.0.1:8545`.

```bash
"$NODE/tools/reth/run-dev.sh"
```

## B — LightPool

Run from `$NODE` so the default store is `$NODE/store`.

```bash
cd "$NODE"
source ./env.sh
lightpool create-wallet --force
lightpool node --role validator
```

## D — Deploy

Needs A + B running.

```bash
cd "$NODE/scripts/event-contract-setup"
python3 00_bridge_bootstrap.py --phase deploy
```

Writes `$CFG` and `$ENV_BRIDGE`. Deploy reads validator stake from LightPool `getCommitteeInfo` (single-node default `100`).

## C — Bridge

```bash
"$BRIDGE/target/release/lightpool-bridge" --config "$CFG"
```

Admin UI: http://127.0.0.1:8787

Restart this process after every **deploy** so it loads the new `$CFG`.

## D — Init

Needs A + B + C running.

```bash
cd "$NODE/scripts/event-contract-setup"
python3 00_bridge_bootstrap.py --phase init
```

## D — Deposit

```bash
source "$ENV_BRIDGE"
export LP_RECIPIENT=$(lightpool --rpc-url "$LP_RPC" address | grep -oE '0x[0-9a-fA-F]{40}' | head -1)

cast send "$ETH_USDT" "transfer(address,uint256)" "$RECV" "$AMOUNT" \
  --rpc-url "$RETH_RPC" --private-key "$PK"
cast send "$ETH_USDT" "approve(address,uint256)" "$BRIDGE" "$AMOUNT" \
  --rpc-url "$RETH_RPC" --private-key "$RECV_PK"
cast send "$BRIDGE" "deposit(uint64,address)" "$AMOUNT" "$LP_RECIPIENT" \
  --rpc-url "$RETH_RPC" --private-key "$RECV_PK"

lightpool --rpc-url "$LP_RPC" balance --token-address "$LP_USDT" --account "$LP_RECIPIENT"
```

Wait for Terminal C `confirm_dep_ok` before checking balance.

## D — Withdraw

```bash
source "$ENV_BRIDGE"

lightpool --rpc-url "$LP_RPC" bridge-withdraw \
  --token-address "$LP_USDT" --amount 50 --evm-recipient "$RECV"

cast call "$ETH_USDT" "balanceOf(address)(uint256)" "$RECV" --rpc-url "$RETH_RPC"
```

Wait for Terminal C `evm_finalized` before checking balance.

## Clear data

Stop C → B → A first. Run **Env**, then:

```bash
rm -rf "$NODE/tools/reth/data/dev" "$NODE/store" && rm -f "$HOME/.lightpool/wallet.json" "$HOME/.lightpool/validator.json" "$CFG" "$ENV_BRIDGE"
```

Clears: Reth chain, LightPool `$NODE/store`, validator wallet/config, bridge config and `.env.bridge`.

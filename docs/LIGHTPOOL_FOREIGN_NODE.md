# LightPool ↔ LightPool bridge — local setup (LightPool as foreign node)

No Reth or Foundry required. Two independent LightPool validators act as **local** (inbound) and **foreign** (outbound) chains; `lightpool-bridge` relays deposits and withdraws between them.

```text
~/work/lightpool-labs/
├── lightpool-node/
│   ├── store/               → local chain (Terminal B)
│   ├── store-foreign/       → foreign chain (Terminal A)
│   └── scripts/event-contract-setup/
│       ├── 01_lp_foreign_bootstrap.py
│       └── .env.lp-foreign   → written by bootstrap
└── lightpool-bridge/        → Terminal C
```

Fresh checkout:

```bash
mkdir -p ~/work/lightpool-labs && cd ~/work/lightpool-labs
git clone https://github.com/lightpool-labs/lightpool-node.git
git clone https://github.com/lightpool-labs/lightpool-bridge.git
```

Four terminals (A–D). Run **Env** once per terminal.

**Full retest from zero:** stop C → B → A, run **Clear data** at the end of this doc, then **Build** → A → B → D bootstrap → C → deposit → withdraw.

## Flow

| Direction | User action | Bridge |
| --- | --- | --- |
| **Deposit** (foreign → local) | `outbound-lock` on foreign node | `foreign_lock_seen` → `confirm_dep` on local |
| **Withdraw** (local → foreign) | `bridge-withdraw` on local node | `withdraw_seen` → `release` on foreign |

## Env

Paste this block alone. Do not append other commands on the same line.

```bash
export WORK=~/work/lightpool-labs
export NODE=$WORK/lightpool-node
export BRIDGE=$WORK/lightpool-bridge
export PATH="$NODE/bin:$PATH"
export LP_RPC=http://127.0.0.1:26300
export LP_FOREIGN_RPC=http://127.0.0.1:27300
export CFG=$BRIDGE/bridge-config.json
export ENV_LP=$NODE/scripts/event-contract-setup/.env.lp-foreign
export FOREIGN_DIR=$HOME/.lightpool/foreign
export FOREIGN_WALLET=$FOREIGN_DIR/wallet.json
export FOREIGN_VALIDATOR=$FOREIGN_DIR/validator.json
export AMOUNT=100000000
```

## Reset — clean slate

Stop running processes first (**Ctrl+C**, reverse start order): Terminal C (bridge) → B (local node) → A (foreign node). Then run **Clear data** at the end of this file (after **Env**).

## Build

Run after **Env**, in a separate step (skip if binaries already built):

```bash
cd "$NODE" && cargo build --release && source ./env.sh
cd "$BRIDGE" && cargo build --release
```

## A — Foreign LightPool node

Uses RPC `:27300`, separate wallet and store. Create wallet and **validator.json** with non-default mempool/consensus ports before first start (otherwise defaults collide with the local node).

```bash
mkdir -p "$FOREIGN_DIR" "$NODE/store-foreign"

lightpool create-wallet --force --wallet-path "$FOREIGN_WALLET"

PUBKEY=$(lightpool --wallet-path "$FOREIGN_WALLET" address | grep "Public Key:" | awk '{print $3}')
cat > "$FOREIGN_VALIDATOR" <<EOF
{
  "consensus_pubkey": "$PUBKEY",
  "mempool_address": "127.0.0.1:27100",
  "consensus_address": "127.0.0.1:27200"
}
EOF

cd "$NODE"
source ./env.sh
lightpool node --role validator \
  --wallet "$FOREIGN_WALLET" \
  --store "$NODE/store-foreign" \
  --validator "$FOREIGN_VALIDATOR" \
  --front-listen-addr 0.0.0.0:27000 \
  --rpc-listen-addr 0.0.0.0:27300 \
  --ws-listen-addr 0.0.0.0:27400
```

## B — Local LightPool node

Default ports (`:26300` RPC). Run from `$NODE` so the default store is `$NODE/store`.

```bash
cd "$NODE"
source ./env.sh
lightpool create-wallet --force
lightpool node --role validator
```

## D — Bootstrap

Needs A + B running. Writes `$CFG` and `$ENV_LP`.

```bash
cd "$NODE/scripts/event-contract-setup"
python3 01_lp_foreign_bootstrap.py --phase all
```

Phases (optional splits): `local-init` (inbound bridge on local), `foreign-setup` (USDT + outbound bridge on foreign), `config` (rewrite `$CFG` from env).

## C — Bridge

```bash
"$BRIDGE/target/release/lightpool-bridge" --config "$CFG"
```

Admin UI: http://127.0.0.1:8787

Restart this process after every bootstrap so it loads the new `$CFG`.

## D — Deposit

Lock foreign USDT; bridge mints LP USDT on the local chain.

```bash
source "$ENV_LP"

LOCAL_RECIPIENT=$(lightpool --rpc-url "$LP_RPC" --wallet-path "$HOME/.lightpool/wallet.json" address \
  | grep -oE '0x[0-9a-fA-F]{40}' | head -1)

lightpool --rpc-url "$LP_FOREIGN_RPC" --wallet-path "$FOREIGN_WALLET" outbound-lock \
  --bridge-address "$OUTBOUND_BRIDGE" \
  --token-address "$FOREIGN_USDT" \
  --amount "$AMOUNT" \
  --foreign-recipient "$LOCAL_RECIPIENT"

lightpool --rpc-url "$LP_RPC" balance --token-address "$LOCAL_LP_USDT" --account "$LOCAL_RECIPIENT"
```

Wait for Terminal C `confirm_dep_ok` before checking balance.

## D — Withdraw

Burn local LP USDT; bridge releases foreign USDT on the foreign chain.

```bash
source "$ENV_LP"

FOREIGN_RECIPIENT=$(lightpool --rpc-url "$LP_FOREIGN_RPC" --wallet-path "$FOREIGN_WALLET" address \
  | grep -oE '0x[0-9a-fA-F]{40}' | head -1)

lightpool --rpc-url "$LP_RPC" bridge-withdraw \
  --token-address "$LOCAL_LP_USDT" \
  --amount 50 \
  --evm-recipient "$FOREIGN_RECIPIENT"

lightpool --rpc-url "$LP_FOREIGN_RPC" balance --token-address "$FOREIGN_USDT" --account "$FOREIGN_RECIPIENT"
```

Wait for Terminal C foreign release / `withdraw_seen` completion before checking foreign balance.

## Clear data

Stop C → B → A first. Run **Env**, then:

```bash
rm -rf "$NODE/store" "$NODE/store-foreign" \
  "$FOREIGN_DIR" \
  "$HOME/.lightpool/wallet.json" "$HOME/.lightpool/validator.json" \
  "$CFG" "$ENV_LP"
```

Clears: both chain stores, local and foreign wallets/validator config, bridge config and `.env.lp-foreign`.

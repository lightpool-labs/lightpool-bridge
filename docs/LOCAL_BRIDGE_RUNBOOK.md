# Local bridge runbook — first-time setup

Linear guide: **local LightPool** + **Reth (EVM)** + **foreign LightPool** + **lightpool-bridge**. Follow the steps in order.

```text
~/work/lightpool-labs/
├── lightpool-node/
│   ├── tools/reth/              → Terminal 1
│   ├── store/                   → local LightPool data
│   ├── store-foreign/           → foreign LightPool data
│   └── scripts/event-contract-setup/
│       ├── 00_bridge_bootstrap.py
│       ├── 01_lp_foreign_bootstrap.py
│       ├── .env.bridge
│       └── .env.lp-foreign
└── lightpool-bridge/            → Terminal 4 (bridge + Admin UI :8787)
```

Token amounts use **6 decimals** (USDT). `100000000` raw = **100 USDT**.

| Terminal | Process | RPC |
| --- | --- | --- |
| **1** | Reth | HTTP `8545`, WS `8546` |
| **2** | Foreign LightPool validator | `27300` |
| **3** | Local LightPool validator | `26300` |
| **4** | `lightpool-bridge` + Admin UI | `8787` |

Other commands run in a normal shell after **Step 1 — Env**.

---

## Step 1 — Env

Paste once per shell:

```bash
export WORK=~/work/lightpool-labs
export NODE=$WORK/lightpool-node
export BRIDGE=$WORK/lightpool-bridge
export PATH="$HOME/.foundry/bin:$NODE/tools/reth/bin:$NODE/bin:$PATH"

export LP_RPC=http://127.0.0.1:26300
export LP_FOREIGN_RPC=http://127.0.0.1:27300
export RETH_RPC=http://127.0.0.1:8545
export CFG=$BRIDGE/bridge-config.json
export ENV_BRIDGE=$NODE/scripts/event-contract-setup/.env.bridge
export ENV_LP=$NODE/scripts/event-contract-setup/.env.lp-foreign

export FOREIGN_DIR=$HOME/.lightpool/foreign
export FOREIGN_WALLET=$FOREIGN_DIR/wallet.json
export FOREIGN_VALIDATOR=$FOREIGN_DIR/validator.json

export PK=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
export RECV=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
export RECV_PK=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d
export AMOUNT=100000000
```

## Step 2 — Build

The `lightpool` CLI (`init-bridge`, `bridge-withdraw`, `outbound-withdraw`, …) ships with **lightpool-node** under `bin/` — no separate `lightpool` source checkout.

```bash
"$NODE/tools/reth/download.sh"
cd "$NODE" && cargo build --release && source ./env.sh
cd "$BRIDGE" && cargo build --release
```

After `source ./env.sh`, `lightpool` is on `PATH` from `$NODE/bin/`.

## Step 3 — Reth (Terminal 1)

```bash
"$NODE/tools/reth/run-dev.sh"
```

## Step 4 — Foreign LightPool (Terminal 2)

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

cd "$NODE" && source ./env.sh
lightpool node --role validator \
  --wallet "$FOREIGN_WALLET" \
  --store "$NODE/store-foreign" \
  --validator "$FOREIGN_VALIDATOR" \
  --front-listen-addr 0.0.0.0:27000 \
  --rpc-listen-addr 0.0.0.0:27300 \
  --ws-listen-addr 0.0.0.0:27400
```

## Step 5 — Local LightPool (Terminal 3)

```bash
cd "$NODE" && source ./env.sh
lightpool create-wallet --force
lightpool node --role validator
```

Wait until the validator committee is running.

## Step 6 — Bootstrap on-chain contracts

Run in order in one shell (Terminals 1–3 must be up):

```bash
cd "$NODE/scripts/event-contract-setup"

python3 00_bridge_bootstrap.py --phase deploy
python3 00_bridge_bootstrap.py --phase create
python3 01_lp_foreign_bootstrap.py --phase all
```

- `deploy` — MockUSDT + EVM Bridge → `.env.bridge`, empty-route `bridge-config.json`
- `create` — inbound bridge on local LP → `LP_USDT`, `INBOUND_BRIDGE` in `.env.bridge` (first instance, usually `…0001`)
- `01 --phase all` — **once** — LP-foreign inbound + foreign USDT/outbound → `.env.lp-foreign` (next instance, often `…0002` on a clean chain; **your addresses come from the script output, not from docs**)

Do **not** run `01 --phase all` twice. Each run allocates the next inbound instance (`…0002`, then `…0003`, …) and orphans the previous Admin UI route.

First run failed after `local-init`? Recover without a new instance:

```bash
python3 01_lp_foreign_bootstrap.py --phase foreign-setup
python3 01_lp_foreign_bootstrap.py --phase config
```

## Step 7 — Bridge (Terminal 4)

```bash
"$BRIDGE/target/release/lightpool-bridge" --config "$CFG"
```

Admin UI: **http://127.0.0.1:8787**

Bridge transport (LightPool routes are fully WebSocket-driven; no poll loop):

- **Local LP (all routes)** — local inbound `withdraw` via WebSocket (local RPC port **+ 100**, e.g. `26300` → `26400`). Foreign `deposit` submits in the same WS handler (ms-level).
- **LightPool foreign route** — foreign `outbound-withdraw` via WebSocket (foreign RPC port **+ 100**, e.g. `27300` → `27400`). Local `confirm_dep` submits in the same WS handler.
- **Reth route** — EVM deposit logs via WebSocket (`:8546`). Local withdraw via local LP WebSocket. EVM `requestWithdraw` submits immediately; `finalizeWithdraw` uses a poll loop (`poll_interval_ms`, default 1s) only because of the on-chain dispute period.

1. **Settings** → **Wallet path** → `~/.lightpool/wallet.json` → **Save**  
   Must be the **local validator wallet** (same key used when bootstrap runs `create-outbound-bridge`). If this path points at a different key than the outbound bridge authorities, foreign deposits fail with `bridge voter not in authorities`.
2. **+ Add route** → **EVM (Reth)** — copy **Reth route** fields below → **Save route**
3. **+ Add route** → **LightPool** — copy **LightPool foreign route** fields from `source "$ENV_LP"` below (must match `LOCAL_INBOUND_BRIDGE` + `LOCAL_LP_USDT` in `.env.lp-foreign`)

**Address map (clean chain, single bootstrap each):**

| Leg | Local inbound | Local LP token |
| --- | --- | --- |
| Reth | `$INBOUND_BRIDGE` (usually `…0001`) | `$LP_USDT` (usually `…0001`) |
| LP foreign | `$LOCAL_INBOUND_BRIDGE` (usually `…0002`) | `$LOCAL_LP_USDT` (usually `…0002`) |

Re-running bootstrap bumps the index (`…0003`, …). Always use the values printed by the last successful bootstrap / `.env.lp-foreign`, not the table above.

### Reth route — Admin UI fields

```bash
source "$ENV_BRIDGE"

echo "Route ID:                  reth-usdt"
echo "Enabled:                   yes"
echo "Kind:                      evm"
echo "Local inbound bridge:      $INBOUND_BRIDGE"
echo "Local LP token:            $LP_USDT"
echo "EVM RPC:                   $EVM_RPC_URL"
echo "Chain ID:                  $EVM_CHAIN_ID"
echo "Bridge contract:           $BRIDGE"
echo "Token address:             $ETH_USDT"
echo "Confirmations:             1"
echo "Start block:               0"
```

### LightPool foreign route — Admin UI fields

```bash
source "$ENV_LP"

echo "Route ID:                  foreign-lp-usdt"
echo "Enabled:                   yes"
echo "Kind:                      lightpool"
echo "Local inbound bridge:      $LOCAL_INBOUND_BRIDGE"
echo "Local LP token:            $LOCAL_LP_USDT"
echo "Foreign LP RPC:            $LP_FOREIGN_RPC"
echo "Foreign chain ID:          $FOREIGN_CHAIN_ID"
echo "Outbound bridge contract:  $OUTBOUND_BRIDGE"
echo "Foreign token:             $FOREIGN_USDT"
```

## Step 8 — Deposit via Reth (EVM → local)

```bash
source "$ENV_BRIDGE"

LP_RECIPIENT=$(lightpool --rpc-url "$LP_RPC" address | grep -oE '0x[0-9a-fA-F]{40}' | head -1)

cast send "$ETH_USDT" "transfer(address,uint256)" "$RECV" "$AMOUNT" \
  --rpc-url "$RETH_RPC" --private-key "$PK"
cast send "$ETH_USDT" "approve(address,uint256)" "$BRIDGE" "$AMOUNT" \
  --rpc-url "$RETH_RPC" --private-key "$RECV_PK"
cast send "$BRIDGE" "deposit(uint64,address)" "$AMOUNT" "$LP_RECIPIENT" \
  --rpc-url "$RETH_RPC" --private-key "$RECV_PK"

lightpool --rpc-url "$LP_RPC" balance --token-address "$LP_USDT" --account "$LP_RECIPIENT"
```

Wait for **`confirm_dep_ok`** in Terminal 4 before checking balance.

## Step 9 — Deposit via foreign LightPool

```bash
source "$ENV_LP"

LOCAL_RECIPIENT=$(lightpool --rpc-url "$LP_RPC" address | grep -oE '0x[0-9a-fA-F]{40}' | head -1)

lightpool --rpc-url "$LP_FOREIGN_RPC" --wallet-path "$FOREIGN_WALLET" outbound-withdraw \
  --bridge-address "$OUTBOUND_BRIDGE" \
  --token-address "$FOREIGN_USDT" \
  --amount "$AMOUNT" \
  --foreign-recipient "$LOCAL_RECIPIENT"

lightpool --rpc-url "$LP_RPC" balance --token-address "$LOCAL_LP_USDT" --account "$LOCAL_RECIPIENT"
```

Wait for **`confirm_dep_ok`** in Terminal 4.

## Step 10 — Withdraw to Reth (local → EVM)

```bash
source "$ENV_BRIDGE"

lightpool --rpc-url "$LP_RPC" bridge-withdraw \
  --bridge-address "$INBOUND_BRIDGE" \
  --token-address "$LP_USDT" \
  --amount 50 \
  --foreign-recipient "$RECV"

cast call "$ETH_USDT" "balanceOf(address)(uint256)" "$RECV" --rpc-url "$RETH_RPC"
```

Wait for **`evm_finalized`** in Terminal 4.

## Step 11 — Withdraw to foreign LightPool

```bash
source "$ENV_LP"

FOREIGN_RECIPIENT=$(lightpool --rpc-url "$LP_FOREIGN_RPC" --wallet-path "$FOREIGN_WALLET" address \
  | grep -oE '0x[0-9a-fA-F]{40}' | head -1)

lightpool --rpc-url "$LP_RPC" bridge-withdraw \
  --bridge-address "$LOCAL_INBOUND_BRIDGE" \
  --token-address "$LOCAL_LP_USDT" \
  --amount 50 \
  --foreign-recipient "$FOREIGN_RECIPIENT"

lightpool --rpc-url "$LP_FOREIGN_RPC" balance --token-address "$FOREIGN_USDT" --account "$FOREIGN_RECIPIENT"
```

Wait for **`deposit_ok`** in Terminal 4.

### `deposit_failed` — bridge voter not in authorities

Bootstrap before this fix used the foreign validator wallet for `create-outbound-bridge`, while the bridge signs deposits with `~/.lightpool/wallet.json`. Outbound bridge authorities must match the bridge Settings wallet.

**Fix (no full Reset):** create a new outbound bridge with the local wallet, update the route, restart the bridge:

```bash
source "$ENV_LP"
LOCAL_WALLET=${LIGHTPOOL_WALLET_PATH:-$HOME/.lightpool/wallet.json}

FOREIGN_TOKEN_REF=$(python3 -c "
raw = bytes.fromhex('${LOCAL_LP_USDT}'.replace('0x',''))
print('0x' + (b'\\x00'*(20-len(raw)) + raw).hex())
")

OUT=$(lightpool --rpc-url "$LP_FOREIGN_RPC" --wallet-path "$LOCAL_WALLET" create-outbound-bridge \
  --token-address "$FOREIGN_USDT" \
  --foreign-chain-id "$LOCAL_CHAIN_ID" \
  --foreign-token "$FOREIGN_TOKEN_REF" \
  --epoch 0 \
  --stake 100 2>&1)
echo "$OUT"
NEW_OUTBOUND=$(echo "$OUT" | grep -oE '0x08[0-9a-fA-F]{14}' | grep -v 0x0800000000000000 | tail -1)
echo "Update Admin UI Outbound bridge contract → $NEW_OUTBOUND"
echo "Update .env: OUTBOUND_BRIDGE=$NEW_OUTBOUND"
```

Stop Terminal 4, rebuild/restart `lightpool-bridge`, update the **LightPool foreign route** outbound address, then run Step 11 again with a small amount.

---

## Reset

Stop Terminal **4 → 3 → 2 → 1**, then:

```bash
rm -rf "$NODE/tools/reth/data/dev" "$NODE/store" "$NODE/store-foreign" \
  "$FOREIGN_DIR" \
  "$HOME/.lightpool/wallet.json" "$HOME/.lightpool/validator.json" \
  "$CFG" "$ENV_BRIDGE" "$ENV_LP"
```

Repeat from **Step 2 — Build**.

## Further reading

- [`RETH_FOREIGN_NODE.md`](RETH_FOREIGN_NODE.md)
- [`LIGHTPOOL_FOREIGN_NODE.md`](LIGHTPOOL_FOREIGN_NODE.md)
- Venue / app stack (liquidity maker needs LP USDT on local chain): `lightpool-node/doc/venue-stack-bridge.md` — `00_bridge_bootstrap.py --phase fund`

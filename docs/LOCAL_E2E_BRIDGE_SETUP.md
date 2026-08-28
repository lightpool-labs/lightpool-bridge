# Local end-to-end bridge setup (Reth + local LP + foreign LP)

Linear guide: **local LightPool** + **Reth (EVM)** + **foreign LightPool** + **lightpool-bridge**. Follow the steps in order.

## Bridge model (multi-token)

Each leg uses **one bridge** with **multiple tokens** (one foreign token ↔ one LP token):

<table>
<tbody>
<tr>
<td style="white-space:nowrap">Local LP inbound</td>
<td style="white-space:nowrap">Inbound instance</td>
<td>create creates bridge + first token; more tokens via reg_lane</td>
</tr>
<tr>
<td style="white-space:nowrap">Reth</td>
<td style="white-space:nowrap">EVM Bridge contract</td>
<td>Deployer: registerToken(erc20) per ERC20</td>
</tr>
<tr>
<td style="white-space:nowrap">Foreign LP outbound</td>
<td style="white-space:nowrap">Outbound instance</td>
<td>create-outbound-bridge creates bridge + first token; more tokens via reg_lane</td>
</tr>
</tbody>
</table>

**lightpool-bridge** watches **one route per token**. Several routes may share the same EVM $BRIDGE and local $INBOUND_BRIDGE — they differ by **token address** and **LP token**.

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

The `lightpool` CLI (`init-bridge`, `bridge-withdraw`, `outbound-withdraw`, …) ships with **lightpool-node** under `bin/` .

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


```bash
cd "$NODE/scripts/event-contract-setup"

python3 00_bridge_bootstrap.py --phase deploy
python3 00_bridge_bootstrap.py --phase create
python3 01_lp_foreign_bootstrap.py --phase all
```
## Step 7 — Bridge (Terminal 4)

```bash
"$BRIDGE/target/release/lightpool-bridge" --config "$CFG"
```

Admin UI: **http://127.0.0.1:8787**

### Reth route — Admin UI fields

```bash
source "$ENV_BRIDGE"

echo "Route ID:                  reth-usdt"
echo "Local bridge contract:     $INBOUND_BRIDGE"
echo "Local LP token:            $LP_USDT"
echo "EVM RPC:                   $EVM_RPC_URL"
echo "EVM chain ID:              $EVM_CHAIN_ID"
echo "EVM bridge contract:       $BRIDGE"
echo "EVM token address:         $ETH_USDT"
echo "EVM block confirmations:   1"
```

### LightPool foreign route — Admin UI fields

```bash
source "$ENV_LP"

echo "Route ID:                  foreign-lp-usdt"
echo "Local bridge contract:     $LOCAL_INBOUND_BRIDGE"
echo "Local LP token:            $LOCAL_LP_USDT"
echo "Foreign LP RPC:            $LP_FOREIGN_RPC"
echo "Foreign chain ID:          $FOREIGN_CHAIN_ID"
echo "Foreign bridge contract:   $OUTBOUND_BRIDGE"
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
cast send "$BRIDGE" "deposit(address,uint64,address)" "$ETH_USDT" "$AMOUNT" "$LP_RECIPIENT" \
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
---

## Reset

Stop Terminal **4 → 3 → 2 → 1**, then:

```bash
rm -rf "$NODE/tools/reth/data/dev" "$NODE/store" "$NODE/store-foreign" \
  "$FOREIGN_DIR" \
  "$HOME/.lightpool/wallet.json" "$HOME/.lightpool/validator.json" \
  "$CFG" "$ENV_BRIDGE" "$ENV_LP" \
  "$BRIDGE/bridge-config-events.db" \
  "$BRIDGE/bridge-config-events.db-wal" \
  "$BRIDGE/bridge-config-events.db-shm"
```

This clears chain stores, wallets/env, **bridge config** (`$CFG`), and the **Admin UI events DB** next to it (`bridge-config-events.db` + SQLite WAL files).

Repeat from **Step 2 — Build**.

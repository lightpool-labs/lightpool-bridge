# LightPool <-> EVM Bridge — manual deposit & withdraw sample

Sample steps to test deposit and withdraw against a local Reth + LightPool node.
Copy and run **one block at a time**. Do not run this file as a script.

Assumptions:
- Repo root: `~/work/lightpool-labs`
- Foundry: `~/.foundry/bin` (forge / cast)
- Reth binary: `tools/reth/bin/reth`
- LightPool CLI: `lightpool/target/release/lightpool-cli`
- Reth HTTP: `http://127.0.0.1:8545`
- LightPool RPC: `http://127.0.0.1:26300`
- Node wallet: `~/.lightpool/wallet.json` (validator consensus key)

Flow (no LightPool `mint`):
1. Deploy MockUSDT + Bridge on Reth (committee = LP validator eth address)
2. `init-bridge` on LightPool with `--evm-token` (creates LP USDT; stores EVM token mapping)
3. Start Link (loads EVM↔LP map from on-chain BridgeConfig via `get_config`)
4. Deposit on EVM → Link `confirm_dep` → LP USDT credited
5. `bridge-withdraw` on LP → Link `requestWithdraw` / `finalizeWithdraw` → EVM USDT unlocked

---

## 0) Shell PATH (once per terminal)

```bash
cd ~/work/lightpool-labs
export PATH="$HOME/.foundry/bin:$PWD/tools/reth/bin:$PATH"
export RETH_RPC=http://127.0.0.1:8545
export LP_RPC=http://127.0.0.1:26300
export LP_CLI=$PWD/lightpool/target/release/lightpool-cli
export NODE_WALLET=$HOME/.lightpool/wallet.json

# Reth --dev account0 (deployer)
export PK=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
export USER=0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266

# Reth --dev account1 (depositor)
export RECV=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
export RECV_PK=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d

# 100 USDT (6 decimals)
export AMOUNT=100000000

# Local EVM chain id (Reth --dev often 1337; confirm with cast)
export EVM_CHAIN_ID=1337
```

Check tools:

```bash
forge --version
cast --version
$LP_CLI --version || true
cast chain-id --rpc-url $RETH_RPC
```

---

## 1) Start Reth (Terminal A)

```bash
cd ~/work/lightpool-labs
tar -xzf tools/reth/downloads/reth-v2.4.1-x86_64-unknown-linux-gnu.tar.gz -C tools/reth/bin
chmod +x tools/reth/bin/reth
./tools/reth/run-dev.sh
```

Keep this terminal running.

```bash
cast block-number --rpc-url $RETH_RPC
```

---

## 2) Build LightPool (once)

```bash
cd ~/work/lightpool-labs/lightpool
cargo build --release -p lightpool -p lightpool-cli
```

---

## 3) Start LightPool validator WITHOUT Link first (Terminal B)

```bash
$LP_CLI --rpc-url $LP_RPC create-wallet --force || true

cd ~/work/lightpool-labs/lightpool
./target/release/lightpool --role validator
```

```bash
$LP_CLI --rpc-url $LP_RPC address
```

---

## 4) Deploy MockUSDT + Bridge on Reth (Terminal C)

EVM committee / finalizer must be the LightPool validator eth address
(same as `lightpool-cli address` when using the node wallet):

```bash
export VALIDATOR_ETH=$($LP_CLI --rpc-url $LP_RPC address | grep -oE '0x[0-9a-fA-F]{40}' | head -1)
echo "VALIDATOR_ETH=$VALIDATOR_ETH"

# Fund validator for gas (requestWithdraw / finalizeWithdraw)
cast send "$VALIDATOR_ETH" --value 10ether --rpc-url "$RETH_RPC" --private-key "$PK"

cd ~/work/lightpool-labs/lightpool-bridge
forge install foundry-rs/forge-std   # first time only
VALIDATOR_ETH=$VALIDATOR_ETH forge script script/DeployLocal.s.sol:DeployLocal \
  --rpc-url $RETH_RPC \
  --broadcast \
  --private-key $PK \
  -vvv
```

```bash
export ETH_USDT=0x........
export BRIDGE=0x........
echo "ETH_USDT=$ETH_USDT"
echo "BRIDGE=$BRIDGE"
```

---

## 5) Init bridge on LightPool (binds EVM token + creates LP USDT)

`--evm-token` must be the EVM USDT from step 4. Authority must be the validator consensus pubkey:

```bash
$LP_CLI --rpc-url $LP_RPC init-bridge \
  --evm-chain-id "$EVM_CHAIN_ID" \
  --evm-token "$ETH_USDT" \
  --name "Tether USD" \
  --symbol "USDT" \
  --epoch 0 \
  --node-wallet "$NODE_WALLET"
```

From the output, copy the LightPool token address:

```bash
export LP_USDT=0x........
echo "LP_USDT=$LP_USDT"
```

Do **not** run `create-token` or `mint` for bridge USDT.

---

## 6) Write bridge-config.json and restart validator WITH Link

LP token / EVM token / epoch / chain id are read from on-chain BridgeConfig (`get_config`). Do **not** put `lp_token` in this JSON.

```bash
cat > ~/work/lightpool-labs/tools/bridge-local/bridge-config.json <<EOF
{
  "enabled": true,
  "evm_rpc_url": "http://127.0.0.1:8545",
  "evm_bridge_address": "$BRIDGE",
  "evm_confirmations": 1,
  "lightpool_rpc_url": "http://127.0.0.1:26300",
  "poll_interval_ms": 1000,
  "dispute_period_seconds": 5,
  "start_block": 0
}
EOF
```

Stop Terminal B validator (Ctrl+C), then:

```bash
cd ~/work/lightpool-labs/lightpool
./target/release/lightpool --role validator \
  --bridge-config ~/work/lightpool-labs/tools/bridge-local/bridge-config.json
```

Log should show `Bridge Link started`.

---

## 7) Fund depositor + deposit on EVM

```bash
cast send "$ETH_USDT" "transfer(address,uint256)" "$RECV" "$AMOUNT" \
  --rpc-url "$RETH_RPC" --private-key "$PK"

export LP_RECIPIENT=$($LP_CLI --rpc-url $LP_RPC address | grep -oE '0x[0-9a-fA-F]{40}' | head -1)
echo "LP_RECIPIENT=$LP_RECIPIENT"

cast send "$ETH_USDT" "approve(address,uint256)" "$BRIDGE" "$AMOUNT" \
  --rpc-url "$RETH_RPC" --private-key "$RECV_PK"

cast send "$BRIDGE" "deposit(uint64,address)" "$AMOUNT" "$LP_RECIPIENT" \
  --rpc-url "$RETH_RPC" --private-key "$RECV_PK"

cast call "$ETH_USDT" "balanceOf(address)(uint256)" "$BRIDGE" --rpc-url "$RETH_RPC"
```

---

## 8) Wait for automatic credit, then check LP balance

Wait a few seconds (Link poll + leader submit + block execution). Watch validator logs for `confirm_dep`.

```bash
$LP_CLI --rpc-url $LP_RPC balance \
  --token-address "$LP_USDT" \
  --account "$LP_RECIPIENT"
```

Expected: `100` USDT (or `100.000000`). **No** `lightpool-cli mint`.

---

## 9) Withdraw back to EVM

Burn LP USDT and unlock on EVM (Link signs + `requestWithdraw`, then `finalizeWithdraw` after local dispute ~5s / a few blocks):

```bash
export WD_AMOUNT=50
export EVM_DEST=$RECV

$LP_CLI --rpc-url $LP_RPC bridge-withdraw \
  --token-address "$LP_USDT" \
  --amount "$WD_AMOUNT" \
  --evm-recipient "$EVM_DEST"
```

Watch validator logs for `requested EVM withdraw`, wait a few seconds, then `finalized EVM withdraw`.
If finalize keeps failing with `StillInDispute`, mine a few empty blocks:

```bash
for i in 1 2 3 4 5 6 7 8; do
  cast rpc evm_mine --rpc-url "$RETH_RPC" || cast send "$VALIDATOR_ETH" --value 0 --rpc-url "$RETH_RPC" --private-key "$PK"
done
```

```bash
# After finalize
cast call "$ETH_USDT" "balanceOf(address)(uint256)" "$EVM_DEST" --rpc-url "$RETH_RPC"

$LP_CLI --rpc-url $LP_RPC balance \
  --token-address "$LP_USDT" \
  --account "$LP_RECIPIENT"
```

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|--------|--------------|-----|
| `odd number of digits` | `$ETH_USDT` / `$BRIDGE` empty | re-export from deploy logs |
| Bridge Link not started | missing `--bridge-config` / `enabled` | check JSON + restart |
| confirm_dep unauthorized | authority pubkey ≠ validator key | re-run `init-bridge --node-wallet` |
| epoch mismatch | config epoch ≠ bridge init epoch | keep both `0` |
| deposit token mismatch | log token ≠ BridgeConfig.evm_token | re-run init-bridge with correct `--evm-token` |
| balance still 0 | Link not catching logs | check `evm_bridge_address`, chain id, start_block |

---

## Address checklist

```text
LP_USDT      =
ETH_USDT     =
BRIDGE       =
LP_RECIPIENT =
```

# LightPool <-> EVM Bridge — local flow

```text
A  tools/reth           → :8545
B  lightpool node       → :26300
C  lightpool-bridge     → --config
D  CLI / bootstrap
```

## Env

```bash
cd ~/work/lightpool-labs
export PATH="$HOME/.foundry/bin:$PWD/tools/reth/bin:$PATH"
export RETH_RPC=http://127.0.0.1:8545
export LP_RPC=http://127.0.0.1:26300
export LP=$PWD/lightpool/target/release/lightpool
export CFG=$PWD/tools/bridge-local/bridge-config.json
export PK=0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
export RECV=0x70997970C51812dc3A010C7d01b50e0d17dc79C8
export RECV_PK=0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d
export AMOUNT=100000000
```

```bash
cd lightpool && cargo build --release -p lightpool
cd lightpool-bridge && cargo build --release
```

## A — Reth

```bash
./tools/reth/download.sh   # once
./tools/reth/run-dev.sh
```

## B — LightPool

```bash
$LP create-wallet --force || true
$LP node --role validator
```

## D — Deploy

```bash
cd lightpool/scripts/event-contract-setup
python3 00_bridge_bootstrap.py --phase deploy
```

## C — Bridge

```bash
cd lightpool-bridge
cargo run --release -- --config "$CFG"
```

## D — Init

```bash
cd lightpool/scripts/event-contract-setup
python3 00_bridge_bootstrap.py --phase init
```

## D — Deposit

```bash
source ~/work/lightpool-labs/lightpool/scripts/event-contract-setup/.env.bridge
export LP_RECIPIENT=$($LP --rpc-url $LP_RPC address | grep -oE '0x[0-9a-fA-F]{40}' | head -1)

cast send "$ETH_USDT" "transfer(address,uint256)" "$RECV" "$AMOUNT" \
  --rpc-url "$RETH_RPC" --private-key "$PK"
cast send "$ETH_USDT" "approve(address,uint256)" "$BRIDGE" "$AMOUNT" \
  --rpc-url "$RETH_RPC" --private-key "$RECV_PK"
cast send "$BRIDGE" "deposit(uint64,address)" "$AMOUNT" "$LP_RECIPIENT" \
  --rpc-url "$RETH_RPC" --private-key "$RECV_PK"

$LP --rpc-url $LP_RPC balance --token-address "$LP_USDT" --account "$LP_RECIPIENT"
```

## D — Withdraw

```bash
$LP --rpc-url $LP_RPC bridge-withdraw \
  --token-address "$LP_USDT" --amount 50 --evm-recipient "$RECV"

cast call "$ETH_USDT" "balanceOf(address)(uint256)" "$RECV" --rpc-url "$RETH_RPC"
```

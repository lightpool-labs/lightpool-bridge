# lightpool-bridge

EVM-side bridge contracts for LightPool deposit / withdraw.

## Model

Hyperliquid-style flow without cold wallets:

1. Validators sign off-chain (Link collects signatures).
2. Leader submits `requestWithdraw` / `requestCommitteeUpdate` with hot quorum (`2/3+1` stake).
3. Requests stay **pending** for `disputePeriodSeconds` (default 200).
4. During dispute, hot quorum can **cancel** a pending withdraw or committee update.
5. After dispute, a finalizer calls `finalize*`.

Deposits lock ERC20 on this contract and emit `DepositInitiated` for LightPool validators to credit on L1.

## Layout

- `src/Bridge.sol` — main bridge contract
- `src/Committee.sol` — committee hash helpers
- `script/Deploy.s.sol` — deploy script

## Build

```bash
forge install foundry-rs/forge-std
forge build
```

## Related

- LightPool bridge module: `lightpool/crates/lightpool-x/src/modules/bridge`
- Off-chain Link task: `lightpool/crates/lightpool-link`

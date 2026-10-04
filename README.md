# TenX 10x vault program (Solana)

TenX gives every coin its own **10x vault**. This repository is the on-chain vault program, published so anyone can read and check the rules it enforces.

Site: https://tenxlock.com · X: https://x.com/TenxlockL

**Unaudited.** The 10x is paid by people who buy tickets later. Only some tickets reach 10x, and only while new tickets keep coming. Most tickets lose money.

## Program

| | |
|---|---|
| Program id | `TenXUym3KteNymLHEQwffEQdRVS3huUFkbkrggaXwLK` |
| Version | 3.3.0-sol.1 |
| SHA-256 of the release build (268,464 bytes) | `6b656489231abc602fc64b647602fbc536a7aa0ff92a2ea2a16f798ac2db7f02` |
| Upgrade authority | `3LYcZo8PzjJhM3FrnQeJyKmS8bNUTQjmrcXDNMk2bihK`, a dedicated key that never trades or launches coins. |

Check the deployed binary yourself:

```
solana program dump TenXUym3KteNymLHEQwffEQdRVS3huUFkbkrggaXwLK onchain.so
head -c 268464 onchain.so | sha256sum
```

Built with solana-cli / cargo-build-sbf 3.1.10 (platform-tools v1.52) and anchor-lang 1.2.0.

## Rules (all enforced by the program)

- **Every coin has two standard vaults** at fixed addresses derived from the coin and the tier: a 0.1 SOL ticket and a 0.01 SOL ticket. Every other parameter is fixed in the program, so nobody picks the numbers.
- **Anyone can open a vault.** Usually the first ticket buyer opens it in the same transaction as their ticket and pays its rent (about 0.044 SOL). A vault that never sold a ticket can be closed by anyone, and its rent goes back only to whoever opened it.
- **One ticket per wallet** per vault.
- **15% of every ticket buys the coin and burns it.** `buy` sets the burn share aside in the vault's buyer PDA; `burn_buy` (anyone can call it) spends exactly that on the coin's pump.fun curve and burns every token bought (a real SPL burn). After the coin graduates, or if no burn has succeeded for 30 days, `release_burn_reserve` (anyone) moves the reserve into the pool.
- The other 85% buys vault shares on the vault's own constant-product curve; earlier tickets get more shares for the same price.
- **A position is paid when selling its shares on the vault curve returns `ticket × 10`.** Order: most shares first, ties by buying order. Up to 10 payouts per purchase; anyone can call `settle`. Money goes straight to the owner's wallet.
- **Exit any time** at the vault price. A wallet that exited or was paid cannot buy that vault again.
- **No owner, no admin instruction, no pause, no withdraw instruction.** SOL leaves a vault only as 10x payouts, exits, the burn buy, and the rent refund of a vault that never sold a ticket.
- A vault holds up to 512 waiting tickets at a time.

## What is in here

| Path | What |
|---|---|
| `programs/tenx_spiral/src` | The vault program (Anchor 1.2). |
| `programs/tenx_spiral/tests/open.rs` | Standard vaults: opened at the derived address with fixed parameters, one per coin per tier, first buyer opens and buys in one transaction, unused vaults close with the rent going only to the opener, graduated coins refused. |
| `programs/tenx_spiral/tests/vault.rs` | Vault rules on LiteSVM: share math, one ticket per wallet, payouts most-shares-first at 10x, settlement bounds, exits, graduation, the 30-day stale-burn valve, the payout heap against a reference model under random exits, and solvency after every step. |
| `programs/tenx_spiral/tests/pump.rs` | `burn_buy` against the real pump.fun programs and real coins dumped from mainnet: buys on the curve, burns everything, mint supply drops by exactly the burned amount. |

The website, its backend and the client tools are not part of this repository.

## Build and test

```
cargo build-sbf --manifest-path programs/tenx_spiral/Cargo.toml
cargo test --manifest-path programs/tenx_spiral/Cargo.toml
```

`pump.rs` loads `fixtures/pump.so`, `fixtures/pump_fees.so` and `fixtures/acc_<address>.json` dumped from mainnet: `solana program dump 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P fixtures/pump.so`, the same for `pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ`, and `solana account <address> --output json-compact > fixtures/acc_<address>.json` for each account listed at the top of `pump.rs`.

## License

Business Source License 1.1 (see `LICENSE`): read, audit, build, test and verify freely; deploying this program, or a modified version, as your own in production is not permitted until the Change Date (2029-10-04), when it becomes Apache-2.0.

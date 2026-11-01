# Log fixtures

Redacted slices of **12 real Star Citizen sessions**, spanning game builds
`10989003` (Dec 2025) through `12913470` (Oct 2026) — roughly ten months of
potential format drift.

## What is in them

Each file keeps only the lines this client acts on:

- `SendCommodityBuyRequest`
- `SendCommoditySellRequest`
- `RmToken_CommodityTransactionResponse` (the failure line)
- `ClSetSelectedPlayerLocationInfo`

Everything else — some 19 000 lines per session — is dropped, because the
parser's first act is to reject those in nanoseconds and there is no value in
committing megabytes to prove it.

Lines are **verbatim otherwise**, including their trailing
`[Team_CoreGameplayFeatures][Shops][UI]` tag blocks, which the samples in
`INTEGRATIONS.md` omit.

## Redaction

`playerId[...]` is replaced with `playerId[000000000000]`. It is the only
identifying value on these lines. Nothing else is altered — in particular
`quantity`, `price`, `shopPricePerCentiSCU`, `boxSize` and `unitAmount` are
untouched, since the tests assert exact arithmetic over them and the published
UUIDv5 vectors hash some of them verbatim.

## Corpus totals

| | |
|---|---|
| buy lines | 22 |
| sell lines | 71 |
| failure lines | 16 |
| location-info lines | 236 |

`session-12913470-current.log` is the session the two published id vectors in
`INTEGRATIONS.md` §6 were taken from. It contains 4 buys, 3 sells and one
failure that lands 1.184 s after its buy, so a correct parse yields **6 trades,
not 7**.

## Why these matter

Three things in the format are only visible across many sessions, and each had
to be learned from this corpus rather than from one log:

1. **A line can carry several `Cargo Box Data` groups** — up to four. Parity is
   `sum(boxSize_i * unitAmount_i)`, not a single product. Reading only one pair
   makes parity appear to fail on ~11% of sells.
2. **`transactionMode` exists on sells only**, with two values
   (`ResourceContainer`, `Location`). It does not change unit semantics.
3. **`shopName` is sometimes location-bearing** (`SCShop_AdminOffice_GrimHex`,
   `TDD_SCShop-001`, `SCShop_ht_delta_shubin_m_store`) and sometimes a generic
   template (`SCShop_Admin_lt_base_g`).

## Refreshing

The game rotates `Game.log` into `<install>/LIVE/logbackups/` on each launch,
so history survives — but that directory can be pruned. These fixtures exist so
the test suite does not depend on it.

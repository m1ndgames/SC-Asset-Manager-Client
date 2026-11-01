# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Current state

**Phases 0-4 are done**, plus the History view and autostart. What exists and
passes (219 tests green, clippy clean):

- Scaffold: Tauri 2 + Svelte 5 (runes) + Tailwind v4, `@theme` copied verbatim, fonts
  self-hosted, four-tab shell, updater keypair generated (password-protected) and
  `pubkey` wired in.
- The pure core: `ids.rs`, `units.rs`, `log/parse.rs`, `log/scan.rs`, `log/discover.rs`,
  including both published UUIDv5 vectors and a corpus suite over 12 real sessions.
- `config.rs` (password never crosses into the webview), `backend/firebase.rs` +
  `backend/value.rs`, `probe.rs`, and a working Settings view. Firestore PATCH
  semantics settled against a real project — see below.
- **The watcher:** `log/tail.rs` (polling, share flags, rotation, the offset
  invariant), `log/correlate.rs` (the live grace window, clock-injected),
  `watchstate.rs` (`state.json`: offset + file identity + bounded id filter +
  log-clock watermark), `log/watcher.rs` (the pipeline, cold start, backfill
  filter) and `watcher_task.rs` (the thread and the events). Surfaced in
  Diagnostics with a live feed.

- **The queue and the buy write path:** `queue.rs` (the state machine, persisted to
  `queue.json` after every transition *and* every edit), `refdata.rs`
  (commodities/locations, disk-cached, offline-tolerant), `writer.rs` (INTEGRATIONS.md
  §8 once, backend-agnostic, tested against `backend/mock.rs`), `drain.rs` (the loop,
  the locks, the events) and the Review Queue view with the Location combobox.

- **The sell path:** `writer.rs` grew `write_sell` (trade first, then the asset),
  `plan_sell` (pure, and shared with `preview_sell` so the review card cannot
  disagree with the write), orphan trades, delete-at-zero and over-sell clamping.
  `SyncBackend` gained `upsert_trade`, and `find_assets` now takes
  `location: Option<&str>` — `Some` for the buy merge rule, `None` for a sell.
  The card shows which asset is hit, what remains, and the gross profit.

- **History and CSV export:** the view reads `get_history` and renders it; totals
  are computed in the view because nothing is written from that screen.
  `export_history_csv` builds and writes the file **in Rust** — the renderer has
  no filesystem access and must not acquire any. `csv_field` guards the
  leading-`=`/`+`/`-`/`@` case, since a field starting that way is executed as a
  formula by Excel and Sheets, and item names, locations and error messages are
  all ultimately untrusted strings.
- **`autostart_watcher`:** a config flag, off by default, added with
  `#[serde(default)]` rather than a schema bump. It delegates to the same
  `start_watcher` the button calls, with no offset, which is what keeps the
  "nothing is read until the user says so" invariant: that path defaults an
  unseen log to start-from-now and never backfills silently.

**Not built yet:** the buy merge rule (Phase 5) and PocketBase (Phase 6).
History has no retry/restore actions yet — it is read-only plus export.

`PLAN.md` is the design of record and settles most questions about structure, crate
choices and deliberate omissions. Read the relevant section before proposing an
alternative — several non-obvious decisions (no `notify`, no SQLite, no `dirs`, no
Firebase/PocketBase SDK) are recorded there specifically so they don't get
"fixed" later. Where reality has since contradicted it, this file says so.

## What this is

A Tauri 2 desktop app (Windows) that tails Star Citizen's `Game.log`, detects
commodity buy/sell transactions, shows each one for human review, and writes the
approved record directly into the user's SC-Asset-Manager sync backend (Firebase
Firestore or self-hosted PocketBase).

SC-Asset-Manager is a static SPA with **no API server** — the sync backend *is* the
integration point. This client signs in as an ordinary group member and writes to the
same shared `assets`/`trades` collections the web app listens on.

The authoritative data contract is
[`INTEGRATIONS.md`](https://github.com/m1ndgames/SC-Asset-Manager/blob/main/INTEGRATIONS.md)
in the main repo — field tables, unit semantics, document-id rules, the UUIDv5
scheme, buy/sell write ordering. PLAN.md references it rather than restating it, so
the two cannot drift. **When a field, unit or id question comes up, go to
INTEGRATIONS.md, not to memory.**

## Commands

The Tauri CLI is a pinned devDependency, not a global install — always `npm run tauri`,
never `cargo tauri`.

```bash
npm install
npm run tauri dev                # run the app (Vite + Rust core)
npm run tauri build              # MSI + NSIS bundles
npm run tauri info               # verify the Tauri/WebView2 toolchain
npm run check                    # svelte-check
npm run dev                      # Vite alone (commands unavailable; views inert)

cd src-tauri
cargo test                                   # all Rust tests
cargo test parse::                           # one module
cargo test --test corpus                     # corpus invariants over the fixtures
cargo test published_vector_buy -- --exact   # a single test
cargo test --test corpus -- --ignored --nocapture   # live scan of the real Game.log
cargo clippy --all-targets
cargo tree -d                                # duplicate deps / TLS stacks
```

Tests touching a real install are `#[ignore]`d; CI runs against the redacted fixtures
in `src-tauri/tests/fixtures/` (see the README there).

## Architecture: the parts that need more than one file to understand

**Everything consequential lives in Rust.** The webview makes zero outbound network
requests — even reference data (`commodities.json` etc.) is fetched by `refdata.rs` —
which is what keeps the CSP at `default-src 'self'` and credentials out of the
renderer. Don't add `fetch` to the frontend.

**The queue is the single source of truth, and it lives in Rust.** `queue.rs` owns the
state machine (Detected → PendingReview → Approved → Writing → Written, plus
Rejected/Discarded/Failed), persisted atomically to `queue.json` after every
transition *and* every user field edit. Svelte holds a read-only projection: it calls
commands and re-renders from the result, never mutating queue state locally. That is
why there is no reconciliation logic and no "unsaved edits" concept — preserve this
property.

**Pipeline shape:** `log/tail.rs` (polling, 1 Hz, truncation + partial-line
invariant) → `log/parse.rs` (pure `&str` → `ParsedLine`, no I/O, no clock) →
`log/correlate.rs` (3 s grace window, discards a detection when a matching error line
arrives) → `queue.rs` → `writer.rs` → `backend/{firebase,pocketbase}.rs` behind the
`SyncBackend` trait. `writer.rs` implements INTEGRATIONS.md §8 **once**, for both
backends.

**Keep `parse.rs` pure and `writer.rs` backend-agnostic.** They are the two
highest-value test targets, and both are tested without touching a network or a
filesystem (`backend/mock.rs` stands in for the backend).

## Invariants that are easy to break silently

- **Tailer offset.** Advance `offset` only up to and *including the last `\n`*; keep
  the remainder in `partial`. A half-written line must never be parsed. Decode with
  `from_utf8_lossy`.
- **Persistence ordering.** Per batch: enqueue → fsync `queue.json` → *then* advance
  and fsync the offset. The reverse order loses trades; this order at worst re-reads
  lines, and the deterministic id collapses the duplicate.
- **Two offsets, and only one is persisted.** `Tailer::offset()` is where *reading*
  got to; `Watcher`'s `safe_offset` is where *resolution* got to, and it is the only
  one written to `state.json`. It is held back for as long as anything sits in the
  grace window, because a held detection lives in `Correlator`'s memory and nowhere
  else. `log/watcher_tests.rs` asserts both directions: a crash mid-window re-reads
  the line, and a resolved line is never read twice.
- **A discard resolves its line too.** `mark_processed` is called on promotion *and*
  on discard, or a backfill resurrects a trade the game already told us failed.
- **Rotation clears the position, never the id filter.** `processed_ids` must survive
  a relaunch, or every trade of the previous session comes back.
- **Never identify `Game.log` by its creation time.** NTFS *file-system tunneling*
  resurrects a creation timestamp when a file of the same name reappears in the same
  directory within ~15 s — exactly what the game does on launch. The real `Game.log`
  reports a CreationTime of **May 2021**, hundreds of rotations stale. File identity
  comes from the log's own first line (`BackupNameAttachment=…`, a millisecond session
  start plus the archive name it will be given), with creation time as a fallback only
  and length as a last resort. `FileIdentity::differs_from` returns
  `Option<bool>` on purpose: collapsing "same file" and "cannot tell" into one boolean
  is how this stayed invisible. `replacement_is_detected_even_when_the_creation_time_is_resurrected`
  reproduces the tunneling and is the regression test.
- **Nothing is read until the user says so.** A first run reports the trade count it
  found and offers start-from-now (the default) or import; it never imports silently.
- **`logTimestamp` in the id is what separates two identical sales.** Two sells of the
  same commodity, same quantity, at the same kiosk minutes apart produce log lines that
  are *byte-identical but for the timestamp* — verified live, and both are committed in
  `session-12913470-261101.log`. Drop the timestamp from the UUIDv5 name string and the
  two collapse to one id, so the queue's own dedupe silently swallows the second real
  sale. The timestamp is not there to make the id unique-looking; it is there because
  nothing else on the line distinguishes them.
- **`quantityRaw` is never parsed.** The UUIDv5 name string takes every field verbatim
  from the log — `100800.000000` must stay exactly that string. There is a test
  asserting it never round-trips through `f64`; don't relax it.
- **Buy and sell use different units.** Buy `quantity` is cSCU, sell `quantity` is
  SCU. Buy unit price is `shopPricePerCentiSCU × 100`, *not* `price / quantity`
  (`price` is rounded to whole aUEC). Sell unit price is `amount / quantity`.
- **A sell writes the trade first, then mutates the asset.** No transaction exists;
  crashing between the two must leave a trade whose asset wasn't decremented, not a
  vanished asset.
- **A merged buy is the one non-idempotent operation in the design.** `merge_target_id`
  and `merge_applied` are written to `queue.json` *before* the backend call. A full
  rescan must warn and default to skipping merged buys.
- **Firestore document id and the record's `id` field must be identical.** A mismatch
  produces a record the web app can display but never update or delete. Guard it at
  runtime.
- **Always send an explicit `updateMask`**, listing exactly the fields this client owns
  (`ASSET_FIELD_PATHS` in `backend/firebase.rs`). Never list `quality` or `uexBuyId`:
  both are owned by the web app, and listing them would delete the user's values on
  every merged buy. See the verified semantics below.
- **Never silently drop a class-matching line.** A line matching
  `CEntityComponentCommodityUIProvider::` that fails to parse becomes
  `MalformedMatch { raw, missing }` and surfaces in Diagnostics. Silent drops are
  exactly how a game patch breaks the watcher invisibly.
- **Timestamps.** `chrono::to_rfc3339_opts(SecondsFormat::Millis, true)` is chosen
  because it reproduces byte-for-byte what JS `new Date().toISOString()` writes in the
  web app.
- **`μ` is U+03BC**, not U+00B5. There's an assertion for it; the difference is
  invisible in a diff.

## Parsing style

Classify, then sweep — do **not** write one monolithic regex. Cheap `contains()`
rejection, match the `<Class::Method>` token, take the timestamp as the verbatim text
between the first `<` and `>`, then sweep the remainder with a generic
`([A-Za-z]+)\[([^\[\]]*)\]` into a map and validate required keys per kind. The log
format already differs between buy and sell lines (`boxSize[16.000000]` vs bracketed
`[boxSize[8] | …]`) and carries trailing `[Team_…][Shops][UI]` tags the
INTEGRATIONS.md samples omit. Expect the parser to need patching after game updates;
the design's job is to make that need visible.

### Parity, stated correctly

The drift check is **`Σ(boxSize_i × unitAmount_i) == quantity-in-SCU`, summed over every
`Cargo Box Data` group on the line** — a line can carry up to four, and a mixed-cargo
sell lists one group per box size.

This matters: reading only one `boxSize`/`unitAmount` pair — which is what a naive
`field[value]` map gives you, since later keys overwrite earlier ones — makes parity
appear to fail on ~11% of sells. PLAN.md §3.5 and an earlier draft of this file both got
this wrong. Summed, it holds on **92 of 93** real trade lines (22/22 buys, 21/21
`Location` sells, 50/51 `ResourceContainer` sells), so it is a sound signal worth
surfacing. `log/parse.rs` keeps `box_groups` as an ordered `Vec` for exactly this reason.

A mismatch sets a warning flag on the review card — it does **not** reject the trade, and
`quantity` always remains authoritative.

Two further drift detectors that do not depend on cargo box data at all:
- the buy price cross-check (`shopPricePerCentiSCU × 100` vs `price / quantityScu`,
  within 0.1% on 22/22 real buys);
- the unit token rule (`quantity[... cSCU]` on buys 21/21, bare on sells 72/72).

## What the log does *not* tell you about location

Measured against a full session log, not assumed. This is the evidence for why a human
is in the loop, so don't re-litigate it without new data:

- **Nothing in the log names the station.** A trade made at Port Tressler produced zero
  occurrences of "Tressler" anywhere in a 680 KB log.
- **`LocationId` is not a place.** In `ClSetSelectedPlayerLocationInfo`, every non-null
  value matched the player's own `vehicleEntityId`. It identifies the *selected cargo
  container*. Never render it as a location — it is labelled "container" in the UI for
  this reason. (The committed fixtures cannot show this: they are slices of
  commodity-class lines with no vehicle lines in them.)
- **`shopId` and `kioskId` are per-session entity ids.** 24 distinct `shopId`s across 11
  sessions; exactly one appears in two. So "remember the location for this kiosk" is
  valid *within* a session only, which is why `suggestions.svelte.ts` is module state
  and never touches disk.
- **`shopName` is usually a generic template** (`SCShop_Admin_lt_base_g`). Some do
  self-identify — `SCShop_AdminOffice_GrimHex`, `SCShop_ht_delta_shubin_m_store`,
  `TDD_SCShop-001` — but admin offices everywhere share the one name.
- Unexplored lead: the kiosk entity carries `m_svLinkedLocationManagerId`. If one of
  those covers every kiosk at a station it would be a better within-session key than
  `shopId`. Needs a session with two kiosks at one station to test.

## What the log does *not* tell you about money

Auto-load/unload fees are invisible to the log, and `price`/`amount` are **gross**.

Measured at a Baijini Point kiosk on 1 Nov 2026 with the confirmation panel and the
resulting log line side by side:

| | |
|---|---|
| quoted | `1,064.66`/SCU x 8 SCU = **8,517.28** gross |
| `CARGO AUTO-LOAD` | **474** aUEC |
| actually paid out | **8,043** aUEC |
| what the log recorded | `amount[8517.000000]` — the **gross** |

Neither the fee nor the net appears anywhere in the log. The only trace of it is the
`autoLoading[1]` flag on the trade line.

Corroborated on the buy side across the whole corpus: the `shopPricePerCentiSCU x 100`
vs `price / quantity` cross-check drifts identically whether or not auto-load was used
— `autoLoading[0]` n=8, mean +0.00031%; `autoLoading[1]` n=14, mean +0.00040%. That is
ten-thousandths of a percent, i.e. nothing but `price` rounding to whole aUEC. A fee
folded into `price` would show percent-scale drift on one group and not the other.

**Consequence: every profit figure derivable from the log is gross commodity profit.**
On the measured trade the fee was 5.6% of gross and **21% of the apparent profit**, and
a normal run pays one on each leg. Therefore:

- write `sellPrice` from the log as the commodity price — that is what the field means,
  and the web app's Profit column is defined on it;
- surface `autoLoading` on the review card with a plain note that realised proceeds are
  lower;
- **never** model, estimate or subtract a fee. It varies with distance and station, it
  is not in the log, and a fabricated number in a shared database is worse than an
  honestly gross one.

## Hard scope boundaries

- **No UEX integration, ever.** This client must not set `uexBuyId` / `uexSellId`.
- Item/ammo purchases (`SendShopBuyRequest`) are out of scope — commodities only.
- LIVE channel only, with a manual path override. No PTU/EPTU handling.
- No tray/background operation. A plain window you open when you want to review.
- **Nothing is written until the user approves it.** The log does not record the
  location, which is why a human is in the loop at all; the Location combobox is the
  primary interaction of the whole app. Never silently pre-fill a remembered location —
  offer it as a visibly labelled suggestion.

## Config and secrets

`%APPDATA%\sc-asset-manager-client\` holds `config.toml` (with `schema = 1`),
`state.json`, `queue.json`, `history.json`, `refdata/` and `logs/`. All written
atomically (temp file + rename).

The backend password is stored in cleartext in `config.toml` — a deliberate choice
over an OS keyring. It must be stored nowhere else: **never** returned to the webview
(`get_config` returns `password_set: bool`), never logged (redact the key in the
tracing layer), never in an error message. Tokens stay in memory only.

Manual testing goes against a scratch Firebase project, never a live group database.

### Release signing

The updater keypair lives at `C:\Users\<user>\.tauri\sc-asset-manager-client.key`
and is **password-protected**. It is outside the repo and must stay there; only the
public half belongs in `tauri.conf.json` under `plugins.updater.pubkey`, stored as the
`.key.pub` file's contents verbatim — that file is already base64, so do not encode it
again.

A release build therefore needs both:

```bash
TAURI_SIGNING_PRIVATE_KEY=<contents of the .key file, or its path>
TAURI_SIGNING_PRIVATE_KEY_PASSWORD=<the password>
```

Without them `npm run tauri build` produces no updater artifacts.

**If the password is lost after a release ships, every installed copy becomes
permanently un-updatable** — the updater verifies against the baked-in public key and
there is no recovery path short of asking users to reinstall by hand. Rotating the key
means a new `pubkey`, which only helps versions released *after* the swap.

`tauri signer sign` also takes `--app-version`, which binds a signature to a version;
an updater configured with `requireSignedVersion` rejects signatures made without it.
Worth settling in Phase 8 rather than at the first release.

## UI conventions

Plain Vite + Svelte 5 (runes), **not SvelteKit** — one window, three views plus
Diagnostics, view switching via a `$state<View>` union in `App.svelte`.

The visual language is copied from SC-AssetManager's `src/app.css` `@theme` block
verbatim (`--color-bg #000b11`, `--color-surface #001f33`, `--color-border #0a3a5c`,
`--color-accent #42c8f4`, `--color-text #c8dde8`, `--color-muted #7aabcc`; Orbitron
display / Rajdhani body) along with `.rsi-panel` brackets, the scanline table header
and the noise overlay. One deliberate divergence: fonts are bundled as local `.woff2`
rather than loaded from Google Fonts, because a desktop app must render offline.

Unit price is displayed to 6 dp and **never rounded**. The SCU/cSCU/μSCU toggle must
rescale amount and price *together* — letting them end up on different denominators is
the easiest contract violation available.

## Errors

`thiserror`, not `anyhow`: Tauri commands must return typed, serialisable errors the
frontend can discriminate on. `AppError` serialises as `{ kind, message }`; `kind` is the
stable discriminant the UI switches on.

## Corrections to PLAN.md learned from real logs

PLAN.md was written against one session. A corpus of 12 sessions (game builds Dec 2025 →
Oct 2026) in `src-tauri/tests/fixtures/` has since contradicted it in four places:

1. **Parity must be summed over cargo box groups** — see above. The single most
   consequential correction.
2. **`transactionMode` appears on sells only**, with two values (`ResourceContainer`,
   `Location`). PLAN.md risk #4 lists it as unverified. It does not affect unit
   semantics.
3. **`ClSetSelectedPlayerLocationInfo` has benign `[Error]` variants** carrying no
   `LocationId` ("Non-vehicle Resource Container not found"). These are ordinary game
   chatter and are classified `Ignored`, not `Malformed` — otherwise they bury the
   malformed list that exists to make real drift visible.
4. **`reqwest` 0.13 renamed the `rustls-tls` feature to `rustls`.** PLAN.md's table says
   `rustls-tls`, which is a 0.12 name and fails to resolve. We use
   `["json", "rustls", "webpki-roots"]`; bundled roots keep the Windows cert store out
   of it. Verified single TLS stack: one `reqwest` 0.13.1, one `rustls` 0.23.46, shared
   with `tauri-plugin-updater`.

5. **A sell must match its asset on `item` alone, never on `item` + `location`.**
   PLAN.md's `find_assets(&self, item: &str, location: &str)` implies a location-keyed
   lookup for the sell path. That is wrong, and wrong in a way that looks correct in a
   unit test: INTEGRATIONS.md §4 defines an asset's `location` as *"Where it was
   bought"*, and nothing ever moves it. Buying at Port Tressler and selling at Baijini
   Point is not an edge case, it is the trading loop, so a location-keyed sell lookup
   would match nothing on essentially every real sell, write an orphan trade, and omit
   `buyPrice`/`buyLocation` — leaving the web app's Profit column at `—` forever. Per
   the contract those two fields "exist only to make the profit column work", so that
   is the whole feature silently lost.

   Note that §8 does **not** settle this: it says "the asset" throughout and never
   states a matching key for a sell. `assetId` is an *output* of the lookup, written
   onto the trade afterwards, not an input. So this is our decision to make, not
   something to re-read the contract for. Location *is* the right key for the buy
   merge rule (§8 says same `item` and same `location`) — the asymmetry is the point.

   Side effect: PLAN.md risk #2 (a Firestore composite index for `item == X &&
   location == Y`) does not arise for sells, since a single-field match needs no
   composite index.

Also minor: INTEGRATIONS.md §3 says "188.9409 × 1008 = 190,452.43, which rounds to the
logged 190,453". The product is right, the rounding claim is off by one — the logged
`price` is a whole aUEC higher because `shopPricePerCentiSCU` is itself only logged to six
decimals. Neither figure is exact, which is why the unit price comes from
`shopPricePerCentiSCU` and the total from `price`, never one from the other.

## Firestore PATCH semantics — settled, not assumed

INTEGRATIONS.md flags these as unverified and PLAN.md risk #1 asks for them to be
confirmed with a throwaway document. That has now been done against a real project
(`src-tauri/src/probe.rs`, driven by `tests/live_backend.rs`). All three answers:

| Question | Answer |
|---|---|
| Does a field left **out** of `updateMask` survive a write? | **Yes — untouched** |
| Is a field **in** the mask but absent from the body deleted? | **Yes — deleted** |
| Is a `PATCH` with **no** `updateMask` a whole-document write? | **Yes — omitted fields are dropped** |

So INTEGRATIONS.md's unverified reading was right, *and* the explicit-mask strategy is
safe. The combination is what the write pipeline depends on: the mask gives an upsert
its no-stale-fields property, while omitting `quality`/`uexBuyId` from the mask stops a
merged buy from destroying values the web app owns.

Also confirmed on the wire: an unrounded `buyPrice` (188.9409) round-trips exactly, the
document id and `id` field agree, `unit` keeps its contract spelling, an absent `quality`
stays absent rather than becoming 0, and a repeated `upsert_asset` is idempotent.

Re-run with `cargo test --test live_backend -- --ignored --nocapture`. It writes to the
configured project, so it is `#[ignore]`d.

## Log rotation

The game moves the previous `Game.log` into `<install>/LIVE/logbackups/` on each launch,
named `Game Build(<build>) <date> (<time>).log`. Rotation **preserves** rather than
destroys, so a session missed while the client was closed is recoverable from there —
but the directory can be pruned, which is why fixtures are committed.

The archive name comes from the log's own first line, which is also what identifies the
file to the watcher:

```
<2026-11-01T17:29:49.066Z> BackupNameAttachment=" Build(12913470) 01 Nov 26 (18 29 39)"  -- used by backup system
```

Note that the timestamp in the filename is **local time** while the line's own prefix is
UTC — the same instant written twice, an hour apart here. Don't treat the two as
disagreeing.

Because of NTFS tunneling (see the invariants above) the filesystem's own creation time
says nothing useful about which session a log belongs to; that first line is the only
cheap, reliable answer.

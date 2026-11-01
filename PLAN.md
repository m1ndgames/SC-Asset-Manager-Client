# SC-Asset-Manager-Client — implementation plan

A desktop app that watches Star Citizen's `Game.log`, detects commodity trades, lets
you review and correct each one, and writes the approved record into your
SC-AssetManager database.

---

## 1. How this works

SC-AssetManager is a static, client-side SPA. **It has no API server.** What it does
have is an optional sync backend — Firebase Firestore or a self-hosted PocketBase —
holding the shared `assets` and `trades` collections. That backend *is* the
integration point: a program that signs in as an ordinary group member and writes to
those collections shows up in everyone's browser in real time, through listeners the
web app already runs.

So this client talks to Firebase or PocketBase directly. A sync backend is a hard
prerequisite; in standalone mode the data lives in a browser's `localStorage` and is
unreachable from here.

**[`INTEGRATIONS.md`](https://github.com/m1ndgames/SC-Asset-Manager/blob/main/INTEGRATIONS.md)
in the main repo is the authoritative contract** — field tables, unit semantics,
document-id rules, the deterministic-id scheme, buy/sell semantics. This plan
references it rather than restating it, so the two cannot drift. Read it first.

### The one thing that forces a human in the loop

The log does not contain the location. `shopName` is usually a generic template
(`SCShop_Admin_lt_base_g`, identical at every admin office); the kiosk entity name is
a prefab name that repeats at completely different stations; `shopId` and `kioskId`
are per-session entity ids. Reverse-matching the price against UEX terminal data
doesn't work either, because UEX prices are crowd-sourced and frequently stale.

That is why this is a **semi-automatic** client: it parses what the log actually
provides, shows it to you, you supply the location and correct anything else, and
**nothing is written until you approve it.**

### Scope boundaries

| | |
|---|---|
| Commodity buys and sells | **in scope** |
| Item/ammo purchases (`SendShopBuyRequest`) | out of scope |
| UEX Corp integration | **never** — this client must not set `uexBuyId` / `uexSellId` |
| Game channels | LIVE only, with a manual path override |
| Background/tray operation | no — a plain window you open when you want to review |

---

## 2. Stack

**Tauri 2 + Svelte 5 + Tailwind v4.** The Rust core does log tailing, parsing and
backend writes; the frontend is the same stack SC-AssetManager uses, so the review
table is just another Svelte table and the two products can share a visual language.

**Plain Vite + Svelte, not SvelteKit.** One window, three views, no routing, no SSR,
no adapter. View switching is a `$state<View>` union in `App.svelte`. SvelteKit would
add `adapter-static`, an `ssr = false` layout and build friction inside Tauri for
nothing — and the web app's components are bound to its `localStorage` stores anyway,
so they'd need rewriting regardless of the framework.

All versions below were current at the time of writing.

| Crate | Version | Why |
|---|---|---|
| `tauri` / `tauri-build` | 2.12 / 2.7 | Current stable Tauri 2 |
| `tauri-plugin-dialog` | 2.8 | File picker for the manual log-path override |
| `tauri-plugin-single-instance` | 2.5 | **Two tailers would double-write.** A second launch focuses the existing window |
| `tauri-plugin-updater` | 2.14 | Auto-update (§13) |
| `tokio` | 1.54 | `rt-multi-thread`, `fs`, `time`, `sync`, `macros` |
| `reqwest` | 0.13 | `json` + **`rustls-tls`**, `default-features = false` — keeps OpenSSL/schannel variance off Windows |
| `serde` / `serde_json` / `toml` | 1.0 / 1.0 / 1.1 | Config and wire formats |
| `uuid` | 1.29 | `v5` feature, for deterministic ids |
| `chrono` | 0.4 | `to_rfc3339_opts(SecondsFormat::Millis, true)` reproduces exactly what JS `new Date().toISOString()` writes in the web app. That symmetry is the whole reason to prefer it over `time` |
| `regex` | 1.13 | Log line extraction |
| `async-trait` | 0.1 | Required for `Box<dyn Backend>` — native async-in-trait isn't dyn-safe |
| `thiserror` | 2.0 | Typed, serialisable error enums. No `anyhow` — commands must return something the frontend can discriminate on |
| `tracing` / `tracing-subscriber` | 0.1 / 0.3 | Diagnostics, written next to the config |

Deliberate omissions, recorded so nobody "fixes" them later:

- **No `notify`.** See §3.2.
- **No SQLite.** Persistence is JSON via `serde_json`; we store hundreds of records
  and `rusqlite` drags in a C dependency. Revisit above ~10 000 records.
- **No `directories`/`dirs`.** We need exactly one path:
  `std::env::var("APPDATA")` joined with `sc-asset-manager-client`. Tauri's own
  `app_config_dir()` would give `%APPDATA%\<bundle-identifier>\`, which isn't it.
- **No Firebase or PocketBase SDK.** Both protocols are three REST calls each; the
  trait in §5 is the abstraction.

**All network I/O happens in Rust.** The webview makes zero outbound requests — even
`/commodities.json` is fetched by `refdata.rs`. That keeps the Tauri CSP at
`default-src 'self'` and credentials out of the renderer entirely.

After scaffolding, run `cargo tree -d`: if a plugin still pins `reqwest 0.12`, drop to
`0.12` rather than compiling two TLS stacks.

---

## 3. Watching and parsing the log

### 3.1 Finding the log

The RSI launcher's `%APPDATA%\rsilauncher\launcher store.json` is **encrypted/binary,
not readable JSON** — verified, so it cannot be used to locate the library.

Order of resolution: config override → fixed-drive scan for
`<drive>\Roberts Space Industries\StarCitizen\LIVE\Game.log` → `%ProgramFiles%` and
`%ProgramFiles(x86)%`. That's a bounded number of `fs::metadata` calls; skip a drive
whose root doesn't stat, run it on a blocking task, never on the UI thread. Reference
install: `E:\Roberts Space Industries\StarCitizen\LIVE\`.

**Open the file with explicit Windows share flags from the start:**

```rust
use std::os::windows::fs::OpenOptionsExt;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;

OpenOptions::new()
    .read(true)
    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
    .open(&path)?
```

This is `std` — no crate — and it removes any chance of `ERROR_SHARING_VIOLATION`
against the handle CryEngine holds open. CryEngine's exact share mode on 4.x is
unverified, so writing it this way from day one is free insurance.

### 3.2 Polling, not filesystem events

CryEngine appends with buffering. `notify` on Windows wraps `ReadDirectoryChangesW`,
which coalesces under load, can miss events, needs its own debounce, and tells us
*nothing* about truncation — we would still have to stat and compare length. Our
latency budget is seconds anyway, because every detection is held for a grace period
(§3.4). A `tokio::time::interval(1s)` loop doing one `fs::metadata` plus a seek-read
handles append, truncation, deletion, replacement and "file doesn't exist yet" with
the same three branches.

If 1 Hz ever proves insufficient, adding `notify` purely as a wake-up hint is a
contained change inside `log/tail.rs`. That's the reversible half of the decision.

### 3.3 The poll loop and its one critical invariant

Tailer state is `{ path, identity: (created_at, len_at_open), offset: u64, partial: String }`.

Each tick:

1. **Missing file** → status `Searching`; re-run discovery every 10 s. (Normal — the
   game hasn't launched.)
2. **`len < offset`** → truncation. The game relaunched and rewrote the log. Reset
   `offset = 0`, clear `partial`, refresh identity, emit a rotation event.
3. **`created_at` changed** → replacement; same handling. Covers the rare case where
   a new log happens to match the old offset in length.
4. **`len > offset`** → seek, read to EOF, prepend `partial`, split on `\n`.

**The invariant:** advance `offset` by the byte count up to and *including the last
`\n` only*, and keep the remainder in `partial`. A half-written line is never parsed
and is re-read next tick. Decode with `from_utf8_lossy` — a stray byte in an entity
name must never stop the tailer.

### 3.4 The failure window

**Success is never logged.** A completed transaction produces no confirmation line at
all. Only failures are logged:

```
[Error] <CEntityComponentCommodityUIProvider::RmToken_CommodityTransactionResponse>
Commodity Transaction Response Error - playerId[...] result[CargoCreationFailed] type[Buying]
```

So a request line *is* the trade — provisionally. Hold each detection for a **3 s
grace period** (configurable) before it enters the review queue, and discard it if a
matching error arrives in that window.

Be honest about what this is: **correlation by proximity, not by a transaction id.**
The error line carries only `playerId`, `result` and `type[Buying|Selling]` — no
`resourceGUID`, no `shopId`. Matching is therefore oldest-unmatched-of-that-kind
(FIFO) inside the window. Correct for every case in the observed log, where
transactions are minutes apart; two buys fired within 3 s at different kiosks could
mis-attribute. **Mitigation is visibility, not correctness:** every discard appears in
History with its raw line and a "restore this trade" action.

### 3.5 Extraction — classify, then sweep

**Do not write one monolithic regex.** The two request lines already format the same
field differently:

```
buy:   Cargo Box Data: boxSize[16.000000] | unitAmount[63]
sell:  Cargo Box Data:  [boxSize[8] | unitAmount[80]]
```

Double space, wrapping brackets, integers rather than floats. Both also carry a
trailing `[Team_CoreGameplayFeatures][Shops][UI]` tag block that the samples in
`INTEGRATIONS.md` omit. A rigid pattern breaks on the next game patch.

Instead:

1. Cheap `contains("CEntityComponentCommodityUIProvider::")` — rejects ~19 000 lines
   in nanoseconds.
2. Match the `<Class::Method>` token → `Buy` / `Sell` / `TxError`.
3. Take the timestamp as the **verbatim text between the first `<` and `>`**
   (`2026-11-01T09:11:29.754Z`). Keep the string — `ids.rs` needs it unparsed. Parse a
   `DateTime<Utc>` separately, for display only.
4. Sweep the remainder with one generic `([A-Za-z]+)\[([^\[\]]*)\]` into a map. This
   tolerates field reordering, new fields and both `Cargo Box Data` forms;
   `boxSize` and `unitAmount` fall out for free.
5. Validate required keys per kind. A class-matching line we cannot parse becomes
   `MalformedMatch { raw, missing }` and surfaces in Diagnostics. **Never silently
   drop one** — that is exactly how a game patch breaks the watcher invisibly.

Derived values, per `INTEGRATIONS.md` §3:

- Buy `quantity` is **cSCU**; sell `quantity` is **SCU**. 1 SCU = 100 cSCU.
- Buy unit price is `shopPricePerCentiSCU × 100` — **not** `price / quantity`, because
  `price` is rounded to whole aUEC.
- Sell unit price is `amount / quantity`.
- **Parity check:** `boxSize × unitAmount` should equal the quantity in SCU. Verified
  against all seven real lines. A mismatch does *not* reject the trade — it sets a
  flag the review card renders as an amber warning showing both numbers. This is the
  early-warning system for format drift.

### 3.6 Cold start and the high-water mark

`state.json` holds `{ log_path, log_created_at, offset, last_seen_log_ts, processed_ids }`
(the id set bounded to the last ~500).

- **Identity matches and `offset <= len`** → resume. No reprocessing. The common case.
- **Identity differs or `offset > len`** → a session happened while we were closed.
  Backfill: parse the whole file in one pass with `hold_ms = 0` (every error that will
  ever exist is already in the file), then drop anything whose deterministic id is in
  `processed_ids` or whose log timestamp is `<= last_seen_log_ts`. Survivors enqueue
  marked `backfill: true` and render with a muted "earlier session" note.
- **First-ever run** → do *not* silently import a whole session. Ask: "Found 6 trades
  in the current session's log — import them for review, or start watching from now?"
  **Default to start-from-now**, but show the count so the choice is informed.

`processed_ids` records ids we have **enqueued**, not ids we have written — otherwise
a rejected trade returns on the next backfill.

**Persistence ordering matters.** Per batch: enqueue → fsync `queue.json` → *then*
advance and fsync `offset`. A crash in between re-reads those lines, and the
deterministic id collapses the duplicate into the same queue item. The reverse order
loses trades. Items still inside the grace window are deliberately not persisted —
the offset hasn't advanced past their lines yet, which is precisely why promotion,
not parsing, advances it.

---

## 4. Deterministic ids

Per `INTEGRATIONS.md` §6: UUIDv5, namespace
`c99fd589-c700-51a5-ac3b-1043be61f47f`, over

```
"<kind>|<logTimestamp>|<resourceGUID>|<shopId>|<quantityRaw>"
```

with every field taken **verbatim** from the log. `quantityRaw` in particular must not
be parsed into a number and reformatted — `100800.000000` must stay exactly that
string. Write a test that asserts it never round-trips through `f64`.

The two test vectors in that section go in as unit tests in the **first commit**, so
the id scheme is proven before anything depends on it. `to_pb_id()` ports the
PocketBase id mangling from the same section.

---

## 5. Backend abstraction

One trait, two implementations. The web app has no such abstraction — `CLAUDE.md`
records its two backends as parallel implementations — but this client has exactly
one write pipeline, so `writer.rs` must implement `INTEGRATIONS.md` §8 once.

```rust
#[async_trait]
pub trait SyncBackend: Send + Sync {
    fn kind(&self) -> BackendKind;

    async fn sign_in(&self, email: &str, password: &str) -> Result<Session, BackendError>;
    /// Refresh if expiring within a 5-minute skew. Cheap no-op when fresh.
    async fn ensure_token(&self) -> Result<Session, BackendError>;

    /// Whole-document upsert keyed by the record's `id`. MUST be idempotent:
    /// writing twice leaves one document, with no stale fields from a prior version.
    async fn upsert_asset(&self, asset: &AssetRecord) -> Result<(), BackendError>;
    async fn upsert_trade(&self, trade: &TradeRecord) -> Result<(), BackendError>;

    async fn delete_asset(&self, app_id: &str) -> Result<(), BackendError>;
    /// Ok(None) when absent — not an error. Read-modify-write on a partial sell.
    async fn get_asset(&self, app_id: &str) -> Result<Option<AssetRecord>, BackendError>;
    /// The §8 merge rule: assets with this exact item + location.
    async fn find_assets(&self, item: &str, location: &str)
        -> Result<Vec<AssetRecord>, BackendError>;

    /// Settings "Test connection". Must not mutate anything.
    async fn health_check(&self) -> Result<HealthReport, BackendError>;
}
```

`HealthReport` should carry the **nickname found on the user's remote profile**
alongside the configured one — see §14.

**Firebase.** `signInWithPassword` on identitytoolkit, refresh on securetoken,
Firestore REST for documents. **The document id and the `id` field must be
identical** (§6 of the contract) — guard it at runtime, because a mismatch produces a
record the web app can display but never update or delete. A separate
`backend/value.rs` handles Firestore's typed-`Value` encoding; it's the fiddliest code
in the project and deserves round-trip tests of its own.

**PocketBase.** `auth-with-password`, token in a bare `Authorization:` header (no
`Bearer`). There is no true upsert — `PATCH`, and on 404 `POST`. Always write both
`id = to_pb_id(uuid)` and `app_id = uuid`.

Tokens live in memory only; restart-without-prompting works because the password is
in the config file.

Two rules from `INTEGRATIONS.md` §8 that live in this layer:

- **A sell writes the trade first, then mutates the asset.** Two writes, no
  transaction. Crashing between them must leave a trade whose asset wasn't
  decremented — visible and correctable — not a vanished asset with no trade.
- **A buy merges** into an existing asset with the same item and location, with a
  **quantity-weighted average** `buyPrice`. (The web app's manual merge overwrites the
  price; don't copy that, it destroys the cost basis.)

---

## 6. Review queue

```
Detected ─(grace window)─> PendingReview ──approve──> Approved ──> Writing ──> Written
    │                           │                                      │
    └──> Discarded              └──reject──> Rejected                  └──> Failed ──retry──┘
```

State lives in **Rust**, persisted to `queue.json` atomically after every transition
*and* every user field edit. Svelte holds a read-only projection: `get_queue()` on
mount, then events. The frontend never mutates queue state — it calls commands and
re-renders from the result. There is consequently no reconciliation problem and no
"unsaved edits" concept.

`Detected` lives only in `correlate.rs`'s memory and is never persisted; the offset
hasn't advanced past it, so a crash re-reads the line.

**On close or crash:** `PendingReview` and `Failed` persist with partial edits and
reappear ordered by log timestamp. `Approved` persists and the writer drains it on
startup before processing anything new — safe because every write is an idempotent
upsert on a deterministic id.

**With one exception that needs its own guard.** A *merged* buy targets an existing
asset's id and *adds* quantity, so replaying it double-counts. The queue item
therefore carries `merge_target_id: Option<String>` and `merge_applied: bool`, written
to `queue.json` **before** the backend call; a resumed item with `merge_applied == true`
goes straight to `Written`. An item found in `Writing` on load is retried under the
same guard. This is the only non-idempotent operation in the design.

`Written`, `Rejected` and `Discarded` move to `history.json` and their ids enter
`processed_ids`, so a backfill can never resurrect them.

---

## 7. Module layout

```
src-tauri/src/
  main.rs / lib.rs     Tauri builder, plugins, managed state, commands, graceful
                       shutdown (flush queue + state to disk)
  config.rs            TOML load/save, atomic write, path resolution, redaction
  state.rs             AppState handed to .manage()
  model.rs             AssetRecord, TradeRecord, CargoUnit, ReviewItem — field-for-field
                       mirrors of types.ts, camelCase, skip_serializing_if on Options
  units.rs             cSCU/SCU/μSCU via the μSCU pivot, weighted average, clamping
  ids.rs               UUIDv5 namespace, name string, to_pb_id
  refdata.rs           fetch + disk-cache commodities/items/locations.json
  log/discover.rs      install + Game.log detection
  log/tail.rs          polling tailer, identity, truncation, partial-line buffer
  log/parse.rs         pure &str -> ParsedLine; no I/O, no clock
  log/correlate.rs     the grace window
  log/watcher.rs       the task wiring it together + high-water mark
  backend/mod.rs       the SyncBackend trait + errors
  backend/firebase.rs
  backend/pocketbase.rs
  backend/value.rs     Firestore typed-Value <-> serde_json
  backend/mock.rs      #[cfg(test)] in-memory backend for writer tests
  queue.rs             the authoritative state machine + persistence
  writer.rs            the approval pipeline — INTEGRATIONS.md §8, backend-agnostic
  history.rs           append-only outcomes + CSV export
  commands.rs          every #[tauri::command]; thin
  events.rs            event names + payloads, declared once

src/                               (Svelte 5, runes)
  App.svelte                       shell: header, tabs, view switch, toasts
  app.css                          @theme copied verbatim + @font-face + .rsi-panel
  lib/ipc.ts  lib/events.ts  lib/stores.svelte.ts  lib/types.ts  lib/format.ts
  components/  Panel DataTable Combobox UnitToggle Badge StatusBar Toast
  views/       ReviewQueue TradeCard Settings History Diagnostics
```

---

## 8. IPC surface

**Commands:** `get_config`, `set_config`, `test_backend`, `detect_installs`,
`set_log_path`, `start_watcher`, `stop_watcher`, `get_watcher_status`, `rescan_log`,
`get_refdata`, `refresh_refdata`, `get_queue`, `update_item`, `approve_item`,
`reject_item`, `retry_item`, `lookup_asset_match`, `get_history`,
`restore_history_item`, `export_history_csv`, `get_diagnostics`.

**Events:** `queue://item-added|item-changed|item-removed`, `watcher://status`,
`watcher://log-rotated`, `tx://discarded`, `backend://auth`, `refdata://updated`,
`toast`.

`get_config` never returns the password — the DTO carries `password_set: bool`.

---

## 9. UI

Three views plus a Diagnostics tab.

**Review Queue** (home) — one card per item, newest first. Read-only header with a
BUY/SELL badge, resolved item name, log timestamp, and the raw `resourceGUID` /
`shopName` in a muted line. A figures block showing quantity in SCU with the raw log
value beneath (`100 800.000000 cSCU`), unit price to 6 dp **never rounded**, total
aUEC, and the parity indicator from §3.5.

Editable: **Item** (combobox over `commodities.json`; amber and required if the GUID
didn't resolve), **Location** (combobox over `locations.json`, **autofocused — the
primary interaction of the whole app**), amount, unit price, and a SCU/cSCU/μSCU
toggle that rescales amount and price *together*, so the two can never end up with
different denominators. That mistake is the easiest contract violation available and
the UI should make it impossible.

Remember the last location chosen for a given `shopId` within a session and offer it
as a *visibly labelled suggestion*, never a silent pre-fill — a wrong auto-filled
location is worse than an empty one.

Sell cards additionally show the matched asset, computed profit, and — when nothing
matched — a choice between an orphan trade (Profit shows `—`) and back-filling a
synthetic asset. Over-sell shows the clamp explicitly. Buy cards show the merge
preview: `1 008 → 1 488 SCU, buyPrice 188.9409 → 201.3312 (quantity-weighted)`.

**Settings** — backend choice and credentials with `Test connection`; nickname;
SC-AssetManager base URL with refresh + counts; Game.log path with Detect and Browse;
poll interval and grace window; the security note from §10.

**History** — written / rejected / discarded / failed, with retry, restore, and CSV
export matching the web app's trade columns.

**Diagnostics** — watcher status, current offset, last line seen, the last 50
class-matching raw lines including `MalformedMatch` warnings, and a
`Re-scan from start` behind a confirm that quotes the replay warning from §14.

**Visual language** — copy the `@theme` block from SC-AssetManager's `src/app.css`
verbatim:

```
--color-bg #000b11   --color-surface #001f33   --color-border #0a3a5c
--color-accent #42c8f4   --color-text #c8dde8   --color-muted #7aabcc
Orbitron (display) / Rajdhani (body)
```

plus `.rsi-panel` corner brackets, the scanline table header and the noise overlay,
and the established idioms: `divide-y divide-border` tables,
`text-xs uppercase tracking-widest text-muted` labels, `px-3 py-1 bg-accent text-bg`
for the primary action. Window 1100 × 720, min 900 × 600, standard decorations.

One deliberate divergence: **bundle Orbitron and Rajdhani as local `.woff2`** rather
than loading them from Google Fonts the way `src/app.html` does. A desktop app has to
render correctly offline, it keeps the CSP tight, and this app is most useful exactly
when you're mid-session.

---

## 10. Configuration

`%APPDATA%\sc-asset-manager-client\config.toml`, with `state.json`, `queue.json`,
`history.json`, a `refdata/` cache and `logs/` alongside. Written atomically (temp
file + rename). Include a `schema = 1` key so a future migration isn't guesswork.

> **Security note.** The backend password is stored in cleartext in that file — a
> deliberate choice over an OS-keyring alternative. It is stored nowhere else: never
> returned to the webview, never logged (redact the key in the tracing layer), never
> in an error message. Tokens stay in memory.

---

## 11. Build order

Each phase ends with something you can run.

**Phase 0 — Scaffold.** Tauri + Svelte + Tailwind, `@theme` copied, fonts
self-hosted, tab shell with placeholder views. **Generate the updater signing keypair
now** — builds released without an updater config cannot be auto-updated later. Fix
the README (§15).

**Phase 1 — Prove the riskiest assumptions.** No tailer, no review UI:
- `log/parse.rs` + `ids.rs` + `units.rs`, with both published UUIDv5 vectors green;
- a `rescan_log(from_start)` that reads the real `Game.log` and dumps the detected
  trades with derived figures;
- `backend/firebase.rs` far enough for `sign_in`, `health_check` and **one** manual
  `upsert_asset`;
- then open the web app and **edit and delete that record there.**

That last step is the point. Two things can kill this project and neither involves
tailing or UI: the doc-id/field contract being subtly wrong (producing records the app
shows but can never update), and Firestore `PATCH` semantics (§14). Both are provable
in a day.

**Phase 2 — The tailer.** Polling loop, truncation detection, partial-line buffering,
high-water mark, grace window. Output goes to Diagnostics only. Manual test: buy in
game, watch it appear ~3 s later; then **relaunch the game mid-session** and confirm
rotation fires with nothing lost or duplicated.

**Phase 3 — Review queue + buy writes.** Full state machine, persistence, the Review
Queue view, `refdata.rs`. Non-merging buys only. End to end: buy in game → card →
pick location → Approve → row in the web app with the `LOG` badge.

**Phase 4 — Sell path.** `find_assets`, matched-asset panel, trade-before-asset
ordering, delete-at-zero, orphan trades, over-sell clamping.

**Phase 5 — Buy merge rule.** Weighted average, the `merge_applied` crash guard, the
merge preview. Separate from Phase 3 deliberately: it is the one non-idempotent
operation in the design.

**Phase 6 — Settings, install detection, PocketBase.** PocketBase last — once the
trait is proven the second implementation is mechanical.

**Phase 7 — History, CSV export, retries, polish.**

**Phase 8 — Packaging** (§13).

---

## 12. Testing

**Pure unit tests (CI, no fixtures beyond text):**

- **`log/parse.rs`** — the highest-value target. Fixtures: all seven real lines copied
  verbatim **including their trailing `[Team_…][Shops][UI]` tags**, plus adversarial
  cases: both `Cargo Box Data` forms; `quantity[640]` with no unit token vs
  `quantity[100800.000000 cSCU]` with one; `shopName[SCShop_AdminOffice_GrimHex]`
  (location-bearing) vs the generic template; a missing required field; a truncated
  final line; a non-UTF8 byte mid-line; CRLF vs LF.
- **`ids.rs`** — both published vectors, the namespace derivation itself, `to_pb_id`
  for leading-digit and leading-letter cases, and the "never reformat `quantityRaw`"
  assertion.
- **`units.rs`** — the μSCU pivot, weighted average on the §8 example, clamping, and
  `assert_eq!('μ' as u32, 0x03BC)` — the U+03BC vs U+00B5 trap, which is otherwise
  invisible in a diff.
- **`log/tail.rs`** against a temp file the test writes: append → read; append a
  *partial* line → assert nothing emitted and the offset didn't move past it; complete
  it → exactly one emission; truncate to 0 and append → rotation detected, nothing
  lost or duplicated. **This is the single most valuable test in the project**, because
  a tailer bug is either silent data loss or silent duplication.
- **`log/correlate.rs`** — error inside the window (discard), outside (promote), two
  requests one error (FIFO, documented ambiguity), error with no request (ignored).
- **`writer.rs` against `backend/mock.rs`** — §8 as executable spec: assert the trade
  is written *before* the asset is mutated; delete at exactly 0; merge arithmetic;
  orphan `assetId == trade.id` with `buyPrice`/`buyLocation` absent; `unit == buyUnit`;
  replaying one log line produces one record; a resumed `merge_applied` buy doesn't
  double-add.

**Integration, with a concrete expectation.** The reference `Game.log` from one
session contains **4 buy lines, 3 sell lines and 1 failure line**, and the failure
follows the buy at `09:11:29.754Z` by 1.2 s. A correct parse therefore yields
**3 buys + 3 sells = 6 trades, not 7** — one assertion that exercises extraction *and*
the grace window. Also assert the parity identity on every line and that every
`resourceGUID` resolves in `commodities.json`.

> **Snapshot the fixture early.** `Game.log` is live and is rewritten on every game
> launch — it grew from 16 437 to 19 045 lines during the hour this plan was written,
> though the event counts held at 4/3/1. Copy a redacted slice into
> `src-tauri/tests/fixtures/` before the next session overwrites it, and keep the
> full-file test `#[ignore]`d so CI doesn't need the real path.

**Manual:** a live session including a deliberately failed transaction (an
over-capacity buy reproduces `CargoCreationFailed`); game relaunch while the client
runs; client restart while the game runs; the backfill path; first-run with a
populated log; token expiry (force it by setting the refresh skew to 3 599 s rather
than waiting an hour); offline write → `Failed` → retry. All against a scratch Firebase
project, never the live group database.

---

## 13. Distribution

`cargo tauri build` produces MSI and NSIS bundles. `tauri-plugin-updater` handles
updates: keypair generated in Phase 0, private key in GitHub Actions secrets, public
key in `tauri.conf.json`. A release workflow on `v*` tags — mirroring the pattern
already in SC-AssetManager's `.github/workflows/` — publishes the installer plus
`latest.json`.

- **WebView2** is required. Windows 11 and current Windows 10 ship it. Keep Tauri's
  default `downloadBootstrapper` (~2 MB online step on older machines) rather than
  `offlineInstaller`, which inflates the bundle by ~130 MB. Expect an 8–15 MB MSI.
- **Unsigned binaries trigger SmartScreen.** There is no trick around it — an
  Authenticode certificate is the only real fix and it's a recurring cost. The honest
  fallback is to ship unsigned, document the warning, and publish SHA-256 hashes beside
  each release. Note this is unrelated to the updater's own signing keypair.

---

## 14. Open risks

1. **Firestore REST `PATCH` without an `updateMask`** is documented as unverified in
   `INTEGRATIONS.md`. **Make the question moot:** always send an explicit
   `updateMask.fieldPaths` enumerating exactly the fields in the body, and always
   construct the complete record (read-modify-write via `get_asset` for a partial
   sell). Under either interpretation the result is identical. One wrinkle to confirm
   with a throwaway document in Phase 1: a field listed in the mask but absent from
   the body should be *deleted*, which is the behaviour we want for clearing `quality`.
2. **Firestore composite index for `find_assets`.** `item == X && location == Y` needs
   a composite index the user's project almost certainly lacks, failing with a 400 and
   a "create this index" URL. **Start with fetch-all-and-filter client-side** — at a
   few hundred assets that's one request and zero setup. Add the indexed query only if
   the collection grows, and make it fall back.
3. **Error-line correlation is inherently ambiguous** (§3.4). A weakness of the log
   format, not of this design. Surfaced in History rather than solved.
4. **One session, one channel, one shop template observed.** All seven lines are
   `transactionMode[ResourceContainer]` at admin shops. Not seen: another
   `transactionMode`; a buy with the bracketed `Cargo Box Data` form; a sell carrying
   `shopPricePerCentiSCU`. This is exactly why §3.5 specifies classify-then-sweep with
   a loud `MalformedMatch` path. Expect the parser to need a patch after a game
   update; design so that need is *visible*.
5. **No PTU/EPTU install to test against**, and the plan is LIVE-only anyway. Multi-drive
   detection is written blind; network and removable drives are untested.
6. **Merged buys are not replay-safe by construction.** The `merge_applied` guard
   protects against a crash mid-write; it does *not* protect against a deliberate full
   rescan, which would double-add every merged buy. `rescan_log(from_start)` must warn
   explicitly and default to skipping merged buys.
7. **Nickname mismatch is a silent footgun.** One character's difference between the
   configured nickname and the one in the web app's Settings makes every record this
   client writes unsellable and un-pushable for a `user`-role member
   (`INTEGRATIONS.md` §7). `health_check` should read the remote profile and Settings
   should show both values side by side.
8. **`quality` is not in the log.** Omit the field entirely; never write `0`.
9. **Tauri version drift.** 2.12 was the newest confirmed on crates.io; the release
   index on tauri.app appeared staler. Pin `tauri = "2"`, commit `Cargo.lock`, verify
   with `cargo tauri info` on the first build.

---

## 15. README changes (Phase 0)

The repo README predates these decisions and now contradicts them:

- **Delete** `Automatic UEX logging enabled` from Settings — there is no UEX
  integration and never will be.
- **Rewrite** "It will then automatically book them" — it shows each detected trade
  for review and writes only on approval. "Automatically" is actively misleading about
  what the product does.
- **Add** that you must supply the location, because the game log does not contain one.
- **Rewrite** the Settings list: backend selection with credentials, SC-AssetManager
  base URL, nickname, Game.log path override.
- **Link** `INTEGRATIONS.md` in the main repo as the contract.

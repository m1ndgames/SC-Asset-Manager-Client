# SC-Asset-Manager-Client

Trade parsing from Game.log, with review, for SC-Asset-Manager.

https://sc-assetmanager.org
https://github.com/m1ndgames/SC-Asset-Manager

## Usage

Run the app. It locates your Star Citizen installation and watches `Game.log`
for commodity trades.

Each detected trade is shown for review. You confirm or correct it — in
particular the **location**, which the game log does not record — and it is
written to your SC-Asset-Manager database only once you approve it.

Nothing is written automatically. This is a semi-automatic client by design:
the log tells us what was traded, at what price, in what quantity, but not
*where*, so a human closes that gap.

## Installing

Download the MSI or the NSIS `-setup.exe` from the
[latest release](https://github.com/m1ndgames/SC-Asset-Manager-Client/releases/latest).

**The installers are not code-signed**, so Windows SmartScreen will warn the
first time you run one — "Windows protected your PC". An Authenticode
certificate is the only real fix for that, and it is a recurring cost, so for
now the honest answer is to verify the download yourself:

```powershell
Get-FileHash ".\SC.Asset.Manager.Client_0.1.0_x64_en-US.msi" -Algorithm SHA256
```

and compare it against `SHA256SUMS.txt`, published as an asset on the same
release. Checksums are attached before the release is made public, so they are
always there.

(The dots in the filename are GitHub's doing — it replaces the spaces in the
product name when it serves an asset. `SHA256SUMS.txt` lists the names as
served, so what you downloaded is what you look up.)

To get past the warning: **More info** → **Run anyway**.

Updates are checked only when you ask, in **Settings → Updates**, and the check
runs in the app's Rust core — the window itself never makes a network request.

## Requirements

- Windows, with WebView2 (shipped with Windows 11 and current Windows 10).
- A **sync backend** — either Firebase or PocketBase — enabled in
  SC-Asset-Manager. In standalone mode the web app's data lives in a browser's
  `localStorage` and is unreachable from here.

## Settings

- **Backend** — Firebase or PocketBase, with credentials. For Firebase, paste
  the same web config JSON the web app uses.
- **SC-Asset-Manager URL** — where reference data (`commodities.json`,
  `items.json`, `locations.json`) is fetched from.
- **Nickname** — must match the one in SC-Asset-Manager's own Settings.
  A single character's difference makes every record this client writes
  unsellable for a `user`-role member.
- **Game.log path** — detected automatically, overridable.
- **Poll interval** and **grace window**.

> The backend password is stored in cleartext in
> `%APPDATA%\sc-asset-manager-client\config.toml`. It is never returned to the
> UI, never logged, and never included in an error message. Tokens stay in
> memory only.

## How it works

SC-Asset-Manager has no API server — the sync backend *is* the integration
point. This client signs in as an ordinary group member and writes to the same
shared collections the web app uses, so records appear live in everyone's
browser.

The contract it implements is
[INTEGRATIONS.md](https://github.com/m1ndgames/SC-Asset-Manager/blob/main/INTEGRATIONS.md)
in the main repo.

Records written here carry `source: "log-watcher"` and show a `LOG` badge in
the web app. This client never sets `uexBuyId` / `uexSellId` — there is no UEX
integration and there will not be one.

## Development

```bash
npm install
npm run tauri dev          # run the app
npm run tauri build        # MSI + NSIS bundles

cd src-tauri
cargo test                 # all Rust tests
cargo test parse::         # one module
cargo test --test corpus   # corpus-wide invariants over the log fixtures
cargo clippy --all-targets
```

The Tauri CLI is a pinned devDependency, so there is no global install to keep
in sync.

## Releasing

Tagging `v*` runs [`.github/workflows/release.yml`](.github/workflows/release.yml),
which gates on `svelte-check`, `clippy -D warnings` and `cargo test`, checks the
tag against `tauri.conf.json`'s version, builds the bundles, drafts the release,
attaches `SHA256SUMS.txt`, and only then publishes it.

Two repository secrets are required:

| Secret | What it is |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | the contents of `sc-asset-manager-client.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the password that key was generated with |

These sign the *update manifest*, which is unrelated to Authenticode and does
nothing about SmartScreen. Without them the build still produces working
installers, so a release can look entirely successful while shipping no updater
artifacts at all — which silently strands every copy installed from it.

> **If that password is lost, every already-installed copy becomes permanently
> un-updatable.** The updater verifies against the public key baked into the
> binary; rotating the key only helps releases published *after* the swap, and
> there is no recovery path short of asking people to reinstall by hand.

Because `bundle.createUpdaterArtifacts` is on, `npm run tauri build` **exits 1**
without those variables set — after writing the installers, which are usable. That
is deliberate: the alternative is a build that exits 0 having silently produced no
updater artifacts. Set the variables, or accept the non-zero exit when you only
want the installers.

Bump the version in **both** `src-tauri/tauri.conf.json` and
`src-tauri/Cargo.toml` — there is a test asserting they agree, and the workflow
refuses a tag that does not match.

See [PLAN.md](PLAN.md) for the implementation plan and
[CLAUDE.md](CLAUDE.md) for the invariants that are easy to break silently.

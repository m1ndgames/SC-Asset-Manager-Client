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

## Installing

Download the MSI or the NSIS `-setup.exe` from the
[latest release](https://github.com/m1ndgames/SC-Asset-Manager-Client/releases/latest).

**The installers are not code-signed**, so Windows SmartScreen will warn the
first time you run one — "Windows protected your PC". An Authenticode
certificate is the only real fix for that, and it is a recurring cost, so for
now the honest answer is to verify the download yourself:

```powershell
Get-FileHash ".\SC Asset Manager Client_0.1.0_x64_en-US.msi" -Algorithm SHA256
```

and compare it against `SHA256SUMS.txt`, published as an asset on the same
release. Checksums are attached before the release is made public, so they are
always there.

## Requirements

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

> The backend password is stored in cleartext in `%APPDATA%\sc-asset-manager-client\config.toml`.

## How it works

SC-Asset-Manager has no API server — the sync backend *is* the integration
point. This client signs in as an ordinary group member and writes to the same
shared collections the web app uses, so records appear live in everyone's
browser.

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

See [PLAN.md](PLAN.md) for the implementation plan and
[CLAUDE.md](CLAUDE.md) for the invariants that are easy to break silently.

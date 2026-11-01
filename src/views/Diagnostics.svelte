<script lang="ts">
  import Panel from "../components/Panel.svelte";
  import {
    detectLogPath,
    rescanLog,
    isAppError,
    type ScanReport,
    type LogLocation,
  } from "../lib/ipc";

  let location = $state<LogLocation | null>(null);
  let report = $state<ScanReport | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function detect() {
    error = null;
    try {
      location = await detectLogPath();
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    }
  }

  async function rescan() {
    busy = true;
    error = null;
    try {
      report = await rescanLog(location?.path ?? undefined);
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
      report = null;
    } finally {
      busy = false;
    }
  }

  const n = (v: number) => v.toLocaleString("en-US");
  /** Unit prices are shown to 6 dp and never rounded. */
  const price = (v: number) =>
    v.toLocaleString("en-US", {
      minimumFractionDigits: 2,
      maximumFractionDigits: 6,
    });

  detect();
</script>

<div class="flex flex-col gap-4">
  <Panel title="Game.log">
    {#snippet actions()}
      <button
        class="bg-accent px-3 py-1 font-display text-xs tracking-widest text-bg uppercase disabled:opacity-40"
        onclick={rescan}
        disabled={busy || !location?.path}
      >
        {busy ? "Scanning…" : "Re-scan from start"}
      </button>
    {/snippet}

    {#if location?.path}
      <p class="font-mono text-xs break-all text-text">{location.path}</p>
      {#if location.candidates.length > 1}
        <p class="mt-1 text-xs text-muted">
          {location.candidates.length} candidate installs found
        </p>
      {/if}
    {:else}
      <p class="text-sm text-warn-text">
        No Game.log found. Set the path in Settings.
      </p>
    {/if}

    {#if error}
      <p class="mt-2 border border-danger bg-danger/20 px-2 py-1 text-sm text-danger-text">
        {error}
      </p>
    {/if}

    <p class="mt-2 text-xs text-muted">
      A re-scan only reads and reports. Nothing is written to your database.
    </p>
  </Panel>

  {#if report}
    <Panel title="Scan result">
      <dl class="grid grid-cols-2 gap-x-6 gap-y-1 text-sm sm:grid-cols-4">
        {#each [["Lines", n(report.totalLines)], ["Class-matched", n(report.classMatched)], ["Buys", n(report.buys)], ["Sells", n(report.sells)], ["Failures", n(report.errors)], ["Detected", n(report.trades.length)], ["Discarded", n(report.discarded.length)], ["Locations seen", n(report.locationIds.length)]] as [label, value] (label)}
          <div>
            <dt class="text-xs tracking-widest text-muted uppercase">{label}</dt>
            <dd class="font-display text-accent">{value}</dd>
          </div>
        {/each}
      </dl>

      <!-- Drift detectors. These are the signals that a game patch changed the
           log format, so they are stated even when they are zero. -->
      <div class="mt-3 flex flex-wrap gap-3 border-t border-border pt-3 text-xs">
        <span class={report.malformed.length ? "text-danger-text" : "text-muted"}>
          Unparseable: {report.malformed.length}
        </span>
        <span class={report.parityMismatches ? "text-warn-text" : "text-muted"}>
          Parity mismatches: {report.parityMismatches}
        </span>
        <span class={report.priceDrift ? "text-warn-text" : "text-muted"}>
          Price drift &gt;0.1%: {report.priceDrift}
        </span>
        <span class="text-muted">
          Unmatched failures: {report.unmatchedErrors.length}
        </span>
      </div>
    </Panel>

    <Panel title="Detected trades">
      <div class="overflow-x-auto">
        <table class="w-full text-left text-sm">
          <thead class="rsi-scanline">
            <tr class="text-xs tracking-widest text-muted uppercase">
              <th class="py-1 pr-3">Kind</th>
              <th class="py-1 pr-3">Logged</th>
              <th class="py-1 pr-3">Item (GUID)</th>
              <th class="py-1 pr-3 text-right">SCU</th>
              <th class="py-1 pr-3 text-right">aUEC/SCU</th>
              <th class="py-1 pr-3 text-right">Total</th>
              <th class="py-1 pr-3">Shop</th>
              <th class="py-1">Flags</th>
            </tr>
          </thead>
          <tbody class="divide-y divide-border">
            {#each report.trades as t (t.id)}
              <tr>
                <td class="py-1 pr-3">
                  <span
                    class="px-1.5 py-0.5 font-display text-[10px] tracking-widest uppercase
                           {t.kind === 'buy'
                      ? 'bg-accent-dim text-bg'
                      : 'border border-accent text-accent'}"
                  >
                    {t.kind}
                  </span>
                </td>
                <td class="py-1 pr-3 font-mono text-xs text-muted">
                  {t.logTimestamp.slice(11, 23)}
                </td>
                <td class="py-1 pr-3 font-mono text-xs break-all">
                  {t.resourceGuid.slice(0, 8)}…
                </td>
                <td class="py-1 pr-3 text-right">{n(t.quantityScu)}</td>
                <td class="py-1 pr-3 text-right">{price(t.unitPricePerScu)}</td>
                <td class="py-1 pr-3 text-right">{n(t.totalAuec)}</td>
                <td class="py-1 pr-3 text-xs text-muted">{t.shopName ?? "—"}</td>
                <td class="py-1 text-xs">
                  {#if t.parity.kind === "mismatch"}
                    <span class="text-warn-text" title="boxSize x unitAmount disagrees with quantity">
                      parity {t.parity.expectedScu}≠{t.parity.actualScu}
                    </span>
                  {/if}
                  {#if t.locationIdHint}
                    <span class="text-muted">loc {t.locationIdHint}</span>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </Panel>

    {#if report.discarded.length}
      <Panel title="Discarded — a failure line followed these">
        <ul class="flex flex-col gap-2">
          {#each report.discarded as d (d.trade.id)}
            <li class="border-l-2 border-danger pl-2 text-sm">
              <span class="font-display text-xs tracking-widest uppercase">
                {d.trade.kind}
              </span>
              <span class="text-muted">
                {n(d.trade.quantityScu)} SCU · {d.error.result} after {d.deltaMs}ms
              </span>
              <div class="mt-0.5 font-mono text-[11px] break-all text-muted">
                {d.trade.resourceGuid}
              </div>
            </li>
          {/each}
        </ul>
        <p class="mt-2 text-xs text-muted">
          Correlation is by proximity, not by transaction id — the failure line
          carries no resourceGUID. Check these before trusting a discard.
        </p>
      </Panel>
    {/if}
  {/if}
</div>

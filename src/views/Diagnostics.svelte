<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import Panel from "../components/Panel.svelte";
  import {
    detectLogPath,
    rescanLog,
    isAppError,
    planWatchStart,
    startWatcher,
    stopWatcher,
    EV,
    type ScanReport,
    type LogLocation,
    type ColdStart,
    type WatchStatus,
    type DetectedTrade,
    type DiscardedTrade,
  } from "../lib/ipc";
  import { app, refreshWatcher } from "../lib/status.svelte";

  let location = $state<LogLocation | null>(null);
  let report = $state<ScanReport | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  // --- the watcher -------------------------------------------------------
  // Status comes from the shared store (subscribed once in App.svelte), so
  // this panel and the footer can never disagree.
  const status = $derived(app.watcher);
  let plan = $state<ColdStart | null>(null);
  let starting = $state(false);
  /** Newest first. Nothing is written anywhere yet — Phase 3 adds the queue. */
  let feed = $state<
    Array<
      | { k: "detected"; at: string; t: DetectedTrade }
      | { k: "discarded"; at: string; d: DiscardedTrade }
      | { k: "malformed"; at: string; raw: string }
      | { k: "rotated"; at: string }
    >
  >([]);

  const clock = () => new Date().toLocaleTimeString("en-GB");

  function push(item: (typeof feed)[number]) {
    feed = [item, ...feed].slice(0, 50);
  }

  async function refreshPlan() {
    try {
      plan = await planWatchStart();
    } catch (e) {
      plan = null;
      error = isAppError(e) ? e.message : String(e);
    }
  }

  async function start(offset?: number) {
    starting = true;
    error = null;
    try {
      app.watcher = await startWatcher(offset);
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    } finally {
      starting = false;
    }
  }

  async function stop() {
    try {
      await stopWatcher();
      await refreshWatcher();
      await refreshPlan();
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    }
  }

  $effect(() => {
    const subs = [
      listen<WatchStatus>(EV.rotated, () =>
        push({ k: "rotated", at: clock() }),
      ),
      listen<DetectedTrade>(EV.detected, (e) =>
        push({ k: "detected", at: clock(), t: e.payload }),
      ),
      listen<DiscardedTrade>(EV.discarded, (e) =>
        push({ k: "discarded", at: clock(), d: e.payload }),
      ),
      listen<{ raw?: string }>(EV.malformed, (e) =>
        push({ k: "malformed", at: clock(), raw: e.payload?.raw ?? "—" }),
      ),
    ];
    return () => subs.forEach((p) => p.then((un) => un()));
  });

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

  (async () => {
    await detect();
    await refreshWatcher();
    if (!app.watcher?.running) await refreshPlan();
  })();
</script>

<div class="flex flex-col gap-4">
  <Panel title="Watcher">
    {#snippet actions()}
      {#if status?.running}
        <button
          class="border border-accent px-3 py-1 font-display text-xs tracking-widest text-accent uppercase"
          onclick={stop}
        >
          Stop
        </button>
      {:else if plan?.kind !== "firstRun"}
        <button
          class="bg-accent px-3 py-1 font-display text-xs tracking-widest text-bg uppercase disabled:opacity-40"
          onclick={() => start()}
          disabled={starting || !location?.path}
        >
          {starting ? "Starting…" : "Start watching"}
        </button>
      {/if}
    {/snippet}

    {#if status?.running}
      <div class="flex flex-wrap items-baseline gap-x-6 gap-y-1 text-sm">
        <span class="font-display text-accent">
          <span class="mr-1 inline-block size-2 rounded-full bg-accent"></span>
          {status.logPresent ? "Watching" : "Waiting for Game.log"}
        </span>
        <span class="text-muted">
          {n(status.linesSeen)} lines · {n(status.classMatched)} shop lines
        </span>
        <span class="text-muted">
          {n(status.promoted)} detected · {n(status.discarded)} discarded
        </span>
        {#if status.rotations}
          <span class="text-muted">{n(status.rotations)} rotation(s)</span>
        {/if}
      </div>

      <dl class="mt-3 grid grid-cols-2 gap-x-6 gap-y-1 border-t border-border pt-3 text-sm sm:grid-cols-4">
        <div>
          <dt class="text-xs tracking-widest text-muted uppercase">Read to</dt>
          <dd class="font-display text-accent">{n(status.readOffset)}</dd>
        </div>
        <div>
          <dt class="text-xs tracking-widest text-muted uppercase">Saved to</dt>
          <dd class="font-display text-accent">{n(status.safeOffset)}</dd>
        </div>
        <div>
          <dt class="text-xs tracking-widest text-muted uppercase">In grace</dt>
          <dd class="font-display {status.pending ? 'text-warn-text' : 'text-accent'}">
            {n(status.pending)}
          </dd>
        </div>
        <div>
          <dt class="text-xs tracking-widest text-muted uppercase">Unparseable</dt>
          <dd class="font-display {status.malformed ? 'text-danger-text' : 'text-accent'}">
            {n(status.malformed)}
          </dd>
        </div>
      </dl>

      <p class="mt-2 text-xs text-muted">
        <!-- This gap is the point, not a bug: the saved offset deliberately
             lags while a detection is inside its {status.graceWindowMs}ms
             window, so a crash re-reads the line instead of losing the trade. -->
        Saved position lags read position while a detection is inside its
        {n(status.graceWindowMs)}ms grace window — that is what makes a crash
        re-read the line rather than lose the trade.
      </p>
    {:else if plan?.kind === "firstRun"}
      <p class="text-sm text-text">
        This is the first time watching. The current session's log already
        contains
        <span class="font-display text-accent">{n(plan.trades)}</span>
        trade{plan.trades === 1 ? "" : "s"}.
      </p>
      <p class="mt-1 text-xs text-muted">
        Nothing has been imported. Choose — and either way nothing is written
        to your database without your approval.
      </p>
      <div class="mt-3 flex flex-wrap gap-2">
        <button
          class="bg-accent px-3 py-1 font-display text-xs tracking-widest text-bg uppercase"
          onclick={() => start(plan?.kind === "firstRun" ? plan.endOffset : undefined)}
        >
          Start from now
        </button>
        <button
          class="border border-accent px-3 py-1 font-display text-xs tracking-widest text-accent uppercase"
          onclick={() => start(0)}
          disabled={plan.trades === 0}
        >
          Import {n(plan.trades)} for review
        </button>
      </div>
    {:else if plan?.kind === "backfill"}
      <p class="text-sm text-warn-text">
        A session happened while the client was closed
        {plan.reason === "replaced"
          ? "— the game rotated the log and started a new one"
          : plan.reason === "truncated"
            ? "— the log was truncated in place"
            : "— the configured path changed"}.
      </p>
      <p class="mt-1 text-xs text-muted">
        Starting will read the file from the beginning and offer anything not
        already seen.
      </p>
      <button
        class="mt-3 bg-accent px-3 py-1 font-display text-xs tracking-widest text-bg uppercase disabled:opacity-40"
        onclick={() => start(0)}
        disabled={starting}
      >
        {starting ? "Starting…" : "Start and backfill"}
      </button>
    {:else if plan?.kind === "noLog"}
      <p class="text-sm text-muted">
        No Game.log yet. Start Star Citizen, or set the path in Settings.
      </p>
    {:else if plan?.kind === "resume"}
      <p class="text-sm text-muted">
        Not watching. Will resume at byte {n(plan.offset)}.
      </p>
    {:else}
      <p class="text-sm text-muted">Not watching.</p>
    {/if}
  </Panel>

  {#if feed.length}
    <Panel title="Live feed">
      <ul class="flex flex-col gap-1 text-sm">
        {#each feed as f, i (f.at + i)}
          <li class="flex gap-2">
            <span class="font-mono text-xs text-muted">{f.at}</span>
            {#if f.k === "detected"}
              <span class="font-display text-[10px] tracking-widest uppercase
                           {f.t.kind === 'buy' ? 'text-accent' : 'text-accent'}">
                {f.t.kind}
              </span>
              <span class="text-text">
                {n(f.t.quantityScu)} SCU @ {price(f.t.unitPricePerScu)}
              </span>
              <span class="font-mono text-xs text-muted">
                {f.t.resourceGuid.slice(0, 8)}…
              </span>
              {#if f.t.locationIdHint}
                <span class="text-xs text-muted">loc {f.t.locationIdHint}</span>
              {/if}
            {:else if f.k === "discarded"}
              <span class="text-danger-text">
                discarded {f.d.trade.kind} — {f.d.error.result} after
                {n(f.d.deltaMs)}ms
              </span>
            {:else if f.k === "malformed"}
              <span class="text-danger-text">unparseable shop line</span>
              <span class="truncate font-mono text-xs text-muted">{f.raw}</span>
            {:else}
              <span class="text-warn-text">Game.log rotated</span>
            {/if}
          </li>
        {/each}
      </ul>
      <p class="mt-2 text-xs text-muted">
        Detections only. Nothing is written to your database — the review queue
        lands in the next phase.
      </p>
    </Panel>
  {/if}

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

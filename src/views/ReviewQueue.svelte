<!--
  The home view: everything waiting for a decision.

  It holds no queue state of its own. Every action calls a command and this
  refetches — the queue lives in Rust and Svelte renders a projection of it.
  So there is no reconciliation logic and no "unsaved edits" anywhere.
-->
<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import Panel from "../components/Panel.svelte";
  import ReviewCard from "../components/ReviewCard.svelte";
  import {
    getQueue,
    getRefdata,
    refreshRefdata,
    drainQueue,
    isAppError,
    QEV,
    type ItemView,
    type RefData,
  } from "../lib/ipc";
  import { app } from "../lib/status.svelte";

  let items = $state<ItemView[]>([]);
  let refdata = $state<RefData>({
    commodities: {},
    locations: [],
    fetchedAt: null,
    sourceUrl: null,
  });
  let loading = $state(true);
  let error = $state<string | null>(null);
  let refreshing = $state(false);

  async function load() {
    error = null;
    try {
      [items, refdata] = await Promise.all([getQueue(), getRefdata()]);
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  async function refresh() {
    refreshing = true;
    error = null;
    try {
      refdata = await refreshRefdata();
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    } finally {
      refreshing = false;
    }
  }

  // Newest first: the trade you just made is the one you want to confirm.
  const pending = $derived(
    items
      .filter((i) => i.state === "pendingReview")
      .sort((a, b) => b.detected.logTimestamp.localeCompare(a.detected.logTimestamp)),
  );
  const failed = $derived(items.filter((i) => i.state === "failed"));
  const inFlight = $derived(
    items.filter((i) => i.state === "approved" || i.state === "writing"),
  );

  $effect(() => {
    const subs = [
      listen(QEV.queueChanged, load),
      listen(QEV.written, load),
      listen(QEV.writeFailed, load),
      listen(QEV.refdataUpdated, load),
    ];
    return () => subs.forEach((p) => p.then((un) => un()));
  });

  load();
</script>

<div class="flex flex-col gap-4">
  {#if error}
    <p class="border border-danger bg-danger/20 px-2 py-1 text-sm text-danger-text">
      {error}
    </p>
  {/if}

  {#if failed.length}
    <section class="flex flex-col gap-3">
      <h2 class="font-display text-xs tracking-[0.2em] text-danger-text uppercase">
        Failed to write — {failed.length}
      </h2>
      {#each failed as item (item.id)}
        <ReviewCard {item} {refdata} onchanged={load} />
      {/each}
    </section>
  {/if}

  {#if inFlight.length}
    <Panel title="Writing">
      <p class="text-sm text-muted">
        {inFlight.length} item{inFlight.length === 1 ? "" : "s"} approved and
        being written.
        <button class="text-accent underline" onclick={() => drainQueue().then(load)}>
          Write them now
        </button>
      </p>
    </Panel>
  {/if}

  <section class="flex flex-col gap-3">
    {#if pending.length}
      <h2 class="font-display text-xs tracking-[0.2em] text-muted uppercase">
        Awaiting review — {pending.length}
      </h2>
      {#each pending as item, i (item.id)}
        <!-- The first card's Location field takes focus: it is the one thing
             the log cannot tell us, and the only reason a human is here. -->
        <ReviewCard {item} {refdata} autofocus={i === 0} onchanged={load} />
      {/each}
    {:else if !loading}
      <Panel title="Nothing to review">
        {#if !app.watcher?.running}
          <p class="text-sm text-text">
            The watcher is not running, so no trades are being detected.
          </p>
          <p class="mt-1 text-xs text-muted">
            Start it in Diagnostics. Nothing is written to your database without
            your approval.
          </p>
        {:else}
          <p class="text-sm text-text">
            Watching for trades. Buy or sell at a commodity kiosk and it will
            appear here about {((app.watcher?.graceWindowMs ?? 3000) / 1000).toFixed(
              0,
            )} seconds later.
          </p>
          <p class="mt-1 text-xs text-muted">
            The log records what and how much, but never where — so each trade
            needs you to confirm the location before it is written.
          </p>
        {/if}
      </Panel>
    {/if}
  </section>

  <!-- Reference data state belongs here rather than buried in Settings: an
       empty commodity map is why a card would ask you to name an item. -->
  <Panel title="Reference data">
    {#snippet actions()}
      <button
        class="border border-accent px-3 py-1 font-display text-xs tracking-widest text-accent uppercase disabled:opacity-40"
        onclick={refresh}
        disabled={refreshing}
      >
        {refreshing ? "Fetching…" : "Refresh"}
      </button>
    {/snippet}
    <p class="text-sm {Object.keys(refdata.commodities).length ? 'text-muted' : 'text-warn-text'}">
      {Object.keys(refdata.commodities).length} commodities ·
      {refdata.locations.length} locations
      {#if refdata.fetchedAt}
        · fetched {refdata.fetchedAt.slice(0, 16).replace("T", " ")} UTC
      {:else}
        · never fetched
      {/if}
    </p>
  </Panel>
</div>

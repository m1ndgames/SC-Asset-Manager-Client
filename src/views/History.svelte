<!--
  Everything that has left the queue: written, rejected, discarded, failed.

  Like the Review view this holds no state of its own — `get_history` returns
  the records newest first and this renders them. The totals are computed here
  rather than in Rust only because they are a view concern: nothing is written
  from this screen.

  One honesty rule runs through the whole view: **every aUEC figure here is
  gross.** Auto-load and auto-unload fees are charged by the station and appear
  nowhere in Game.log, so a realised profit is lower by however much the user
  paid to have cargo moved. Showing a confident "profit" without saying so
  would be the most misleading thing this app could do.
-->
<script lang="ts">
  import { save } from "@tauri-apps/plugin-dialog";
  import Panel from "../components/Panel.svelte";
  import {
    getHistory,
    exportHistoryCsv,
    restoreItem,
    isAppError,
    QEV,
    type ItemView,
    type ItemState,
  } from "../lib/ipc";
  import { listen } from "@tauri-apps/api/event";

  let items = $state<ItemView[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let note = $state<string | null>(null);
  let filter = $state<"all" | "written" | "notWritten">("all");
  let query = $state("");

  // Restoring. A discard came from the game's own failure line, so that one
  // asks twice; a rejection was just the user changing their mind.
  let restoring = $state<string | null>(null);
  let confirmDiscard = $state<string | null>(null);

  /**
   * Whether a row can go back for review.
   *
   * Written and merged rows are refused in Rust too — this only decides
   * whether to offer the button, and the real guard is `Queue::restore`.
   */
  function canRestore(i: ItemView): boolean {
    if (i.mergeApplied) return false;
    return i.state === "rejected" || i.state === "discarded";
  }

  async function restore(i: ItemView) {
    if (i.state === "discarded" && confirmDiscard !== i.id) {
      confirmDiscard = i.id;
      return;
    }
    restoring = i.id;
    error = null;
    note = null;
    try {
      await restoreItem(i.id);
      note = "Moved back to the Review Queue.";
      await load();
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    } finally {
      restoring = null;
      confirmDiscard = null;
    }
  }

  async function load() {
    error = null;
    try {
      items = await getHistory();
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const subs = [listen(QEV.queueChanged, load), listen(QEV.written, load)];
    return () => subs.forEach((p) => p.then((un) => un()));
  });

  const shown = $derived(
    items
      .filter((i) =>
        filter === "all"
          ? true
          : filter === "written"
            ? i.state === "written"
            : i.state !== "written",
      )
      .filter((i) => {
        const q = query.trim().toLowerCase();
        if (!q) return true;
        const name = (i.resolvedItemName ?? i.edits.itemName ?? "").toLowerCase();
        return (
          name.includes(q) ||
          (i.edits.location ?? "").toLowerCase().includes(q) ||
          i.id.toLowerCase().includes(q)
        );
      }),
  );

  // Written records only: a rejected trade never happened, so counting it
  // would misstate what the user actually traded.
  const written = $derived(items.filter((i) => i.state === "written"));
  const buys = $derived(written.filter((i) => i.detected.kind === "buy"));
  const sells = $derived(written.filter((i) => i.detected.kind === "sell"));
  const spent = $derived(buys.reduce((n, i) => n + i.totalAuec, 0));
  const earned = $derived(sells.reduce((n, i) => n + i.totalAuec, 0));

  const auec = (v: number) => v.toLocaleString("en-US", { maximumFractionDigits: 0 });
  const qty = (v: number) => v.toLocaleString("en-US", { maximumFractionDigits: 6 });
  const price = (v: number) =>
    v.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 6 });
  const when = (s: string) => s.replace("T", " ").replace("Z", "").slice(0, 19);

  const stateClass: Record<ItemState, string> = {
    written: "text-accent",
    failed: "text-danger-text",
    rejected: "text-muted",
    discarded: "text-muted",
    pendingReview: "text-warn-text",
    approved: "text-warn-text",
    writing: "text-warn-text",
  };

  async function exportCsv() {
    note = null;
    error = null;
    try {
      const path = await save({
        title: "Export history",
        defaultPath: "sc-asset-manager-history.csv",
        filters: [{ name: "CSV", extensions: ["csv"] }],
      });
      if (!path) return;
      const rows = await exportHistoryCsv(path);
      note = `Exported ${rows} record${rows === 1 ? "" : "s"} to ${path}`;
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    }
  }

  load();
</script>

<div class="flex flex-col gap-4">
  {#if error}
    <p class="border border-danger bg-danger/20 px-2 py-1 text-sm text-danger-text">
      {error}
    </p>
  {/if}
  {#if note}
    <p class="border border-accent bg-accent/10 px-2 py-1 text-sm text-text">{note}</p>
  {/if}

  <Panel title="Totals">
    {#snippet actions()}
      <button
        class="border border-accent px-3 py-1 font-display text-xs tracking-widest text-accent uppercase disabled:opacity-40"
        onclick={exportCsv}
        disabled={!items.length}
      >
        Export CSV
      </button>
    {/snippet}

    <div class="grid gap-4 sm:grid-cols-4">
      <div>
        <dt class="text-xs tracking-widest text-muted uppercase">Bought</dt>
        <dd class="font-display text-lg text-accent">{auec(spent)}</dd>
        <dd class="text-xs text-muted">{buys.length} record{buys.length === 1 ? "" : "s"}</dd>
      </div>
      <div>
        <dt class="text-xs tracking-widest text-muted uppercase">Sold</dt>
        <dd class="font-display text-lg text-accent">{auec(earned)}</dd>
        <dd class="text-xs text-muted">{sells.length} record{sells.length === 1 ? "" : "s"}</dd>
      </div>
      <div>
        <dt class="text-xs tracking-widest text-muted uppercase">Difference</dt>
        <dd class="font-display text-lg {earned - spent >= 0 ? 'text-accent' : 'text-danger-text'}">
          {auec(earned - spent)}
        </dd>
        <dd class="text-xs text-muted">gross, see below</dd>
      </div>
      <div>
        <dt class="text-xs tracking-widest text-muted uppercase">Records</dt>
        <dd class="font-display text-lg text-text">{items.length}</dd>
        <dd class="text-xs text-muted">{written.length} written</dd>
      </div>
    </div>

    <p class="mt-3 border-l-2 border-border pl-2 text-xs text-muted">
      <span class="text-warn-text">Gross.</span> Station auto-load and auto-unload
      fees never appear in Game.log, so what you actually banked is lower. The
      difference above is also not a profit figure: it compares everything bought
      against everything sold, including cargo you are still holding.
    </p>
  </Panel>

  <Panel title="Records">
    {#snippet actions()}
      <div class="flex items-center gap-2">
        <input
          class="border border-border bg-bg px-2 py-1 text-xs text-text placeholder:text-muted"
          placeholder="Filter item, place, id"
          bind:value={query}
        />
        {#each [["all", "All"], ["written", "Written"], ["notWritten", "Other"]] as const as [v, labelText]}
          <button
            class="border px-2 py-1 font-display text-[10px] tracking-widest uppercase
                   {filter === v
              ? 'border-accent text-accent'
              : 'border-border text-muted hover:text-text'}"
            onclick={() => (filter = v)}
          >
            {labelText}
          </button>
        {/each}
      </div>
    {/snippet}

    {#if loading}
      <p class="text-sm text-muted">Loading…</p>
    {:else if !items.length}
      <p class="text-sm text-text">Nothing has left the queue yet.</p>
      <p class="mt-1 text-xs text-muted">
        Approved, rejected and discarded trades all land here.
      </p>
    {:else if !shown.length}
      <p class="text-sm text-muted">No records match that filter.</p>
    {:else}
      <div class="overflow-x-auto">
        <table class="w-full border-collapse text-sm">
          <thead>
            <tr class="scanline border-b border-border text-left">
              {#each ["When", "", "Item", "Amount", "aUEC/SCU", "Total", "Place", "State", ""] as h}
                <th class="px-2 py-1 font-display text-[10px] tracking-[0.2em] text-muted uppercase">
                  {h}
                </th>
              {/each}
            </tr>
          </thead>
          <tbody>
            {#each shown as i (i.id)}
              <tr class="border-b border-border/40 hover:bg-surface/40">
                <td class="px-2 py-1 font-mono text-xs text-muted whitespace-nowrap">
                  {when(i.detected.logTimestamp)}
                </td>
                <td class="px-2 py-1">
                  <span
                    class="px-1 py-0.5 font-display text-[9px] tracking-widest uppercase
                           {i.detected.kind === 'buy'
                      ? 'bg-accent text-bg'
                      : 'border border-accent text-accent'}"
                  >
                    {i.detected.kind}
                  </span>
                </td>
                <td class="px-2 py-1 text-text">
                  {i.resolvedItemName ?? i.edits.itemName ?? "—"}
                </td>
                <td class="px-2 py-1 whitespace-nowrap text-text">
                  {qty(i.effectiveAmountScu)}
                  <span class="text-xs text-muted">SCU</span>
                </td>
                <td class="px-2 py-1 whitespace-nowrap text-text">
                  {price(i.effectiveUnitPricePerScu)}
                </td>
                <td class="px-2 py-1 whitespace-nowrap text-text">{auec(i.totalAuec)}</td>
                <td class="px-2 py-1 text-muted">{i.edits.location ?? "—"}</td>
                <td class="px-2 py-1 whitespace-nowrap">
                  <span class="font-display text-xs tracking-widest uppercase {stateClass[i.state]}">
                    {i.state}
                  </span>
                  {#if i.error}
                    <span class="block text-xs text-danger-text">{i.error}</span>
                  {/if}
                </td>
                <td class="px-2 py-1 whitespace-nowrap text-right">
                  {#if canRestore(i)}
                    <button
                      class="border px-2 py-0.5 font-display text-[10px] tracking-widest uppercase
                             disabled:opacity-40
                             {confirmDiscard === i.id
                        ? 'border-warn-text text-warn-text'
                        : 'border-border text-muted hover:text-text'}"
                      onclick={() => restore(i)}
                      disabled={restoring !== null}
                      title={i.state === "discarded"
                        ? "The game reported this trade failed. Only restore it if you are sure it actually went through."
                        : "Put this trade back in the review queue."}
                    >
                      {#if restoring === i.id}
                        Restoring…
                      {:else if confirmDiscard === i.id}
                        Really restore?
                      {:else}
                        Restore
                      {/if}
                    </button>
                  {:else if i.mergeApplied}
                    <span
                      class="font-display text-[10px] tracking-widest text-muted uppercase"
                      title="This buy was merged into an existing asset. That is the one write this client cannot safely repeat, so it cannot be restored — change it in SC-Asset-Manager instead."
                    >
                      merged
                    </span>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </Panel>
</div>

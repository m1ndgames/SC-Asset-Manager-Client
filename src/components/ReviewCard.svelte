<!--
  One detected trade, awaiting a human.

  The Location field is the primary interaction of the entire application. The
  game log records what was bought, how much, and at what price, but nothing in
  it names the place — verified against a full session log, where even the
  station the trade happened at appears nowhere. That gap is why this app has a
  human in the loop at all, so the field is autofocused and nothing can be
  approved without it.

  Everything else on the card is either read-only fact from the log, or an
  override the user may set. The component holds no queue state: it calls a
  command and the parent re-renders from the result.
-->
<script lang="ts">
  import {
    updateItem,
    approveItem,
    rejectItem,
    retryItem,
    previewSell,
    previewMerge,
    isAppError,
    type ItemView,
    type RefData,
    type CargoUnit,
    type ItemPatch,
    type SellPreview,
    type MergePlan,
  } from "../lib/ipc";
  import { remember, suggestionFor, recentLocations } from "../lib/suggestions.svelte";

  let {
    item,
    refdata,
    autofocus = false,
    onchanged,
  }: {
    item: ItemView;
    refdata: RefData;
    autofocus?: boolean;
    onchanged: () => void;
  } = $props();

  let busy = $state(false);
  let error = $state<string | null>(null);

  // What approving this sell would do. `null` means the check could not run,
  // which is deliberately distinct from "matched nothing": one is unknown, the
  // other is an orphan trade, and showing the wrong one would mislead.
  let preview = $state<SellPreview | null>(null);
  let previewing = $state(false);

  // The buy equivalent. Same null-means-unknown rule.
  let merge = $state<MergePlan | null>(null);

  $effect(() => {
    if (item.detected.kind !== "buy") return;
    // Re-checked when the amount, the item or the opt-out changes: all three
    // change what the merge would do, or whether there is one at all.
    const key = `${item.id}|${item.effectiveAmountScu}|${item.effectiveUnitPricePerScu}|${item.edits.location}|${item.edits.separateAsset}`;
    let cancelled = false;
    previewMerge(item.id)
      .then((m) => {
        if (!cancelled) merge = m;
      })
      .catch(() => {
        if (!cancelled) merge = null;
      });
    void key;
    return () => {
      cancelled = true;
    };
  });

  // Re-checked whenever the amount or the resolved item changes, since both
  // change which asset is hit and whether it is over-sold.
  $effect(() => {
    if (item.detected.kind !== "sell") return;
    const key = `${item.id}|${item.effectiveAmountScu}|${item.effectiveItemName}`;
    let cancelled = false;
    previewing = true;
    previewSell(item.id)
      .then((p) => {
        if (!cancelled) preview = p;
      })
      .catch(() => {
        if (!cancelled) preview = null;
      })
      .finally(() => {
        if (!cancelled) previewing = false;
      });
    void key;
    return () => {
      cancelled = true;
    };
  });

  const isBuy = $derived(item.detected.kind === "buy");
  const shopId = $derived(item.detected.shopId);
  const suggestion = $derived(suggestionFor(shopId));
  const recent = $derived(recentLocations());

  const blocked = (kind: string) => item.blockers.some((b) => b.kind === kind);

  // 6 dp and never rounded: the unit price comes from shopPricePerCentiSCU and
  // the total from the log's own `price`, never one from the other.
  const price = (v: number) =>
    v.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 6 });
  const qty = (v: number) =>
    v.toLocaleString("en-US", { maximumFractionDigits: 6 });
  const auec = (v: number) => v.toLocaleString("en-US", { maximumFractionDigits: 2 });

  async function patch(p: ItemPatch) {
    error = null;
    try {
      await updateItem(item.id, p);
      onchanged();
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    }
  }

  /** Amount and price are entered in the display unit and stored per SCU. */
  const unitFactor: Record<CargoUnit, number> = {
    SCU: 1,
    cSCU: 100,
    "μSCU": 1_000_000,
  };

  function setDisplayAmount(raw: string) {
    const v = Number(raw);
    if (!Number.isFinite(v)) return;
    patch({ amountScu: v / unitFactor[item.displayUnit] });
  }

  function setDisplayPrice(raw: string) {
    const v = Number(raw);
    if (!Number.isFinite(v)) return;
    // Price scales inversely to amount, so the product stays put.
    patch({ unitPricePerScu: v * unitFactor[item.displayUnit] });
  }

  async function setLocation(loc: string) {
    await patch({ location: loc.trim() || null });
    if (loc.trim()) remember(shopId, loc);
  }

  async function approve() {
    busy = true;
    error = null;
    try {
      const report = await approveItem(item.id);
      if (report.stopped) error = report.stopped;
      else if (report.failed > 0) error = "The write failed — see the card below.";
      onchanged();
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function reject() {
    busy = true;
    try {
      await rejectItem(item.id);
      onchanged();
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function retry() {
    busy = true;
    error = null;
    try {
      const report = await retryItem(item.id);
      if (report.stopped) error = report.stopped;
      onchanged();
    } catch (e) {
      error = isAppError(e) ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  const units: CargoUnit[] = ["SCU", "cSCU", "μSCU"];
</script>

<article class="rsi-panel border border-border bg-surface p-4">
  <!-- Read-only: what the log said. -->
  <header class="flex flex-wrap items-baseline gap-x-3 gap-y-1">
    <span
      class="px-1.5 py-0.5 font-display text-[10px] tracking-widest uppercase
             {isBuy ? 'bg-accent text-bg' : 'border border-accent text-accent'}"
    >
      {item.detected.kind}
    </span>

    <h3 class="font-display text-base text-text">
      {item.effectiveItemName ?? "Unidentified item"}
    </h3>

    <span class="font-mono text-xs text-muted">
      {item.detected.logTimestamp.replace("T", " ").replace("Z", " UTC")}
    </span>

    {#if item.state === "failed"}
      <span class="ml-auto font-display text-xs tracking-widest text-danger-text uppercase">
        Write failed · attempt {item.attempts}
      </span>
    {:else if item.state !== "pendingReview"}
      <span class="ml-auto font-display text-xs tracking-widest text-muted uppercase">
        {item.state}
      </span>
    {/if}
  </header>

  <p class="mt-0.5 font-mono text-[11px] break-all text-muted">
    {item.detected.resourceGuid}
    {#if item.detected.shopName}· {item.detected.shopName}{/if}
    · shop {item.detected.shopId}
  </p>

  <!-- Figures, with the raw log text beneath so the derivation is checkable. -->
  <dl class="mt-3 grid grid-cols-2 gap-x-6 gap-y-2 border-y border-border py-3 sm:grid-cols-4">
    <div>
      <dt class="text-xs tracking-widest text-muted uppercase">Quantity</dt>
      <dd class="font-display text-accent">
        {qty(item.effectiveAmountScu)} <span class="text-xs text-muted">SCU</span>
      </dd>
      <dd class="font-mono text-[11px] text-muted">
        log: {item.detected.quantityRaw}{isBuy ? " cSCU" : ""}
      </dd>
    </div>
    <div>
      <dt class="text-xs tracking-widest text-muted uppercase">aUEC / SCU</dt>
      <dd class="font-display text-accent">{price(item.effectiveUnitPricePerScu)}</dd>
      <dd class="font-mono text-[11px] text-muted">
        {isBuy ? "shopPricePerCentiSCU × 100" : "amount ÷ quantity"}
      </dd>
    </div>
    <div>
      <dt class="text-xs tracking-widest text-muted uppercase">Total</dt>
      <dd class="font-display text-accent">{auec(item.totalAuec)}</dd>
      <dd class="font-mono text-[11px] text-muted">from the log, not derived</dd>
    </div>
    <div>
      <dt class="text-xs tracking-widest text-muted uppercase">Checks</dt>
      <dd class="text-xs">
        {#if item.detected.parity.kind === "mismatch"}
          <span
            class="text-warn-text"
            title="Σ(boxSize × unitAmount) over every Cargo Box Data group disagrees with quantity. quantity stays authoritative."
          >
            parity {qty(item.detected.parity.expectedScu)} ≠ {qty(item.detected.parity.actualScu)}
          </span>
        {:else if item.detected.parity.kind === "ok"}
          <span class="text-muted">parity ok</span>
        {:else}
          <span class="text-muted">no cargo box data</span>
        {/if}
        {#if item.detected.priceCrossCheck !== null && item.detected.priceCrossCheck > 0.001}
          <br />
          <span class="text-warn-text">
            price drift {(item.detected.priceCrossCheck * 100).toFixed(3)}%
          </span>
        {/if}
      </dd>
    </div>
  </dl>

  {#if item.state === "pendingReview" || item.state === "failed"}
    <!-- The one field the log cannot give us. -->
    <div class="mt-3">
      <label class="block text-xs tracking-widest uppercase
                    {blocked('locationMissing') ? 'text-warn-text' : 'text-muted'}"
             for="loc-{item.id}">
        Location {#if blocked("locationMissing")}· required{/if}
      </label>
      <!--
        Autofocus is normally an accessibility problem because it moves focus
        somewhere the user did not ask for. It is the right call here and only
        here: this is a single-purpose window whose entire job is to collect
        this one field, the card is the first thing on screen, and the
        alternative is a user tabbing past read-only log figures on every
        trade. PLAN.md §9 specifies it deliberately.
      -->
      <!-- svelte-ignore a11y_autofocus -->
      <input
        id="loc-{item.id}"
        list="locations-{item.id}"
        class="mt-1 w-full border bg-bg px-2 py-1 text-text
               {blocked('locationMissing') ? 'border-warn' : 'border-border'}"
        placeholder="Where did this happen?"
        value={item.edits.location ?? ""}
        {autofocus}
        onchange={(e) => setLocation(e.currentTarget.value)}
      />
      <datalist id="locations-{item.id}">
        {#each refdata.locations as loc (loc)}
          <option value={loc}></option>
        {/each}
      </datalist>

      <!-- Suggestions are offered, never applied. A wrong auto-filled location
           is worse than an empty one. -->
      {#if suggestion && item.edits.location !== suggestion}
        <p class="mt-1 text-xs text-muted">
          This kiosk was
          <button
            class="border border-accent px-1.5 text-accent"
            onclick={() => setLocation(suggestion)}
          >
            {suggestion}
          </button>
          earlier this session
        </p>
      {:else if recent.length && !item.edits.location}
        <p class="mt-1 flex flex-wrap items-baseline gap-1 text-xs text-muted">
          Used this session:
          {#each recent.slice(0, 4) as loc (loc)}
            <button
              class="border border-border px-1.5 text-text hover:border-accent hover:text-accent"
              onclick={() => setLocation(loc)}
            >
              {loc}
            </button>
          {/each}
        </p>
      {/if}
      {#if !refdata.locations.length}
        <p class="mt-1 text-xs text-warn-text">
          No location list — reference data has not been fetched. You can still
          type a name; check the URL in Settings.
        </p>
      {/if}
    </div>

    <div class="mt-3 grid gap-3 sm:grid-cols-3">
      <!-- Amber and required when the GUID did not resolve. -->
      <div>
        <label class="block text-xs tracking-widest uppercase
                      {blocked('itemUnresolved') ? 'text-warn-text' : 'text-muted'}"
               for="item-{item.id}">
          Item {#if blocked("itemUnresolved")}· not recognised{/if}
        </label>
        <input
          id="item-{item.id}"
          list="commodities-{item.id}"
          class="mt-1 w-full border bg-bg px-2 py-1 text-text
                 {blocked('itemUnresolved') ? 'border-warn' : 'border-border'}"
          placeholder={item.resolvedItemName ?? "Name this commodity"}
          value={item.edits.itemName ?? ""}
          onchange={(e) => patch({ itemName: e.currentTarget.value.trim() || null })}
        />
        <datalist id="commodities-{item.id}">
          {#each Object.values(refdata.commodities) as c (c.name)}
            <option value={c.name}></option>
          {/each}
        </datalist>
      </div>

      <div>
        <label class="block text-xs tracking-widest text-muted uppercase" for="amt-{item.id}">
          Amount ({item.displayUnit})
        </label>
        <input
          id="amt-{item.id}"
          type="number"
          step="any"
          class="mt-1 w-full border bg-bg px-2 py-1 text-text
                 {blocked('amountInvalid') ? 'border-warn' : 'border-border'}"
          value={item.displayAmount}
          onchange={(e) => setDisplayAmount(e.currentTarget.value)}
        />
      </div>

      <div>
        <label class="block text-xs tracking-widest text-muted uppercase" for="prc-{item.id}">
          aUEC per {item.displayUnit}
        </label>
        <input
          id="prc-{item.id}"
          type="number"
          step="any"
          class="mt-1 w-full border bg-bg px-2 py-1 text-text
                 {blocked('priceInvalid') ? 'border-warn' : 'border-border'}"
          value={item.displayUnitPrice}
          onchange={(e) => setDisplayPrice(e.currentTarget.value)}
        />
      </div>
    </div>

    <!-- The toggle rescales amount and price together, always. Letting them
         land on different denominators is the easiest contract violation
         available, so the UI makes it unreachable. -->
    <div class="mt-3 flex items-center gap-2">
      <span class="text-xs tracking-widest text-muted uppercase">Unit</span>
      {#each units as u (u)}
        <button
          class="px-2 py-0.5 font-display text-xs tracking-widest uppercase
                 {item.displayUnit === u
            ? 'bg-accent text-bg'
            : 'border border-border text-muted hover:text-text'}"
          onclick={() => patch({ displayUnit: u })}
        >
          {u}
        </button>
      {/each}
      <span class="ml-2 text-xs text-muted">
        written as {qty(item.effectiveAmountScu)} SCU @ {price(item.effectiveUnitPricePerScu)}
      </span>
    </div>

    <!-- A merge is the one operation in this app that cannot be undone by
         repeating it, so what it will do is spelled out in full before the
         user commits. -->
    {#if isBuy && merge}
      <div class="mt-3 border border-accent/40 bg-accent/5 px-2 py-2">
        <p class="font-display text-[10px] tracking-[0.2em] text-muted uppercase">
          Merges into an existing asset
        </p>
        <p class="mt-1 font-mono text-sm text-text">
          {qty(merge.oldAmount)} → <span class="text-accent">{qty(merge.newAmount)}</span>
          <span class="text-xs text-muted">{merge.unit}</span>
          · {price(merge.oldBuyPrice)} →
          <span class="text-accent">{price(merge.newBuyPrice)}</span>
          <span class="text-xs text-muted">aUEC/{merge.unit} (quantity-weighted)</span>
        </p>
        <p class="mt-1 text-xs text-muted">
          Adding {qty(merge.addedAmount)}
          {merge.unit} at {price(merge.addedBuyPrice)} to the existing lot at {merge.location}.
          No new row is created.
        </p>
        {#if merge.alsoMatching.length}
          <p class="mt-1 text-xs text-warn-text">
            {merge.alsoMatching.length} other asset{merge.alsoMatching.length === 1
              ? ""
              : "s"} already exist here. The oldest is used; the rest stay as they are.
          </p>
        {/if}
        <p class="mt-2 text-xs text-muted">
          Unlike everything else this app writes, a merge cannot simply be
          repeated — it adds. It is recorded before it is attempted so an
          interrupted write is never applied twice.
        </p>
      </div>
    {/if}

    {#if isBuy}
      <label class="mt-2 flex cursor-pointer items-start gap-2">
        <input
          type="checkbox"
          class="mt-0.5 accent-accent"
          checked={item.edits.separateAsset ?? false}
          onchange={(e) =>
            patch({ separateAsset: (e.currentTarget as HTMLInputElement).checked })}
        />
        <span class="text-xs text-muted">
          Keep this buy as its own asset row
          {#if merge}
            — instead of merging it into {merge.targetId.slice(0, 8)}…
          {:else}
            — it has nothing to merge into anyway
          {/if}
        </span>
      </label>
    {/if}

    <!-- A sell mutates something that already exists, so the card has to say
         what it will hit before the user commits to it. -->
    {#if !isBuy}
      <div class="mt-3 border border-border/70 bg-bg/40 px-2 py-2">
        <p class="font-display text-[10px] tracking-[0.2em] text-muted uppercase">
          Sells from
        </p>

        {#if previewing}
          <p class="mt-1 text-sm text-muted">Checking your assets…</p>
        {:else if preview === null}
          <p class="mt-1 text-sm text-warn-text">
            Could not check your assets — the backend is unconfigured or
            unreachable. You can still approve; the write does its own lookup.
          </p>
        {:else if preview.matchedAsset}
          {@const a = preview.matchedAsset}
          <p class="mt-1 text-sm text-text">
            <span class="font-display text-accent">{qty(a.amount)}</span>
            <span class="text-xs text-muted">{a.unit ?? "SCU"}</span>
            bought at
            <span class="text-accent">{price(a.buyPrice)}</span>
            <span class="text-xs text-muted">aUEC/{a.unit ?? "SCU"}</span>
            in <span class="text-accent">{a.location}</span>
          </p>
          <p class="mt-0.5 text-xs text-muted">
            Matched on item, not location — an asset records where it was
            <em>bought</em>, and cargo moves.
          </p>

          <p class="mt-2 text-sm text-text">
            {#if preview.deletesAsset}
              Sells out completely, so the asset is removed.
            {:else}
              Leaves
              <span class="font-display text-accent">
                {qty(preview.remainingAfter ?? 0)}
              </span>
              <span class="text-xs text-muted">{preview.unit}</span>
            {/if}
            {#if preview.profit !== null}
              · profit
              <span class="font-display text-accent">{auec(preview.profit)}</span>
              <span class="text-xs text-muted">aUEC</span>
            {/if}
          </p>

          {#if preview.profit !== null}
            <p class="mt-0.5 text-xs text-muted">
              Gross. Auto-load fees are charged by the station but never appear
              in the log, so your realised profit is lower.
            </p>
          {/if}

          {#if preview.overSoldBy > 0}
            <p class="mt-2 border-l-2 border-warn bg-warn/10 px-2 py-1 text-sm text-warn-text">
              You are selling more than this asset holds. It will be clamped to
              {qty(preview.amountSold)}
              {preview.unit} — {qty(preview.overSoldBy)}
              {preview.unit} of the sale is not covered by any record. Your
              database disagrees with what you actually shipped.
            </p>
          {/if}

          {#if preview.otherCandidates.length}
            <p class="mt-2 text-xs text-muted">
              {preview.otherCandidates.length} other {item.effectiveItemName ??
                "item"} asset{preview.otherCandidates.length === 1 ? "" : "s"} exist.
              The oldest is sold first and the rest are untouched.
            </p>
          {/if}
        {:else}
          <p class="mt-1 text-sm text-warn-text">
            No matching asset — this client never saw the buy.
          </p>
          <p class="mt-0.5 text-xs text-muted">
            It will be written as an orphan trade: the sale is recorded, but
            with no buy price, so the Profit column shows “—”.
          </p>
        {/if}
      </div>
    {/if}

    {#if item.error}
      <p class="mt-3 border-l-2 border-danger bg-danger/10 px-2 py-1 text-sm text-danger-text">
        {item.error}
      </p>
    {/if}
    {#if error}
      <p class="mt-3 border-l-2 border-danger bg-danger/10 px-2 py-1 text-sm text-danger-text">
        {error}
      </p>
    {/if}

    <div class="mt-4 flex items-center gap-2">
      {#if item.state === "failed"}
        <button
          class="bg-accent px-3 py-1 font-display text-xs tracking-widest text-bg uppercase disabled:opacity-40"
          onclick={retry}
          disabled={busy}
        >
          {busy ? "Retrying…" : "Retry write"}
        </button>
      {:else}
        <button
          class="bg-accent px-3 py-1 font-display text-xs tracking-widest text-bg uppercase disabled:opacity-40"
          onclick={approve}
          disabled={busy || !item.canApprove}
          title={item.canApprove ? "Write this to your database" : "Fill in what is missing first"}
        >
          {busy ? "Writing…" : isBuy ? "Approve & write" : "Approve & sell"}
        </button>
      {/if}

      <button
        class="border border-border px-3 py-1 font-display text-xs tracking-widest text-muted uppercase hover:border-danger hover:text-danger-text disabled:opacity-40"
        onclick={reject}
        disabled={busy}
      >
        Reject
      </button>

      {#if !item.canApprove}
        <span class="text-xs text-warn-text">
          {#if blocked("locationMissing")}
            Set the location to approve.
          {:else if blocked("itemUnresolved")}
            Name the commodity to approve.
          {:else}
            Check the amount and price.
          {/if}
        </span>
      {/if}
    </div>
  {/if}
</article>

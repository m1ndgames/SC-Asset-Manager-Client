<script lang="ts">
  import type { Snippet } from "svelte";

  let {
    title = "",
    children,
    actions,
  }: {
    title?: string;
    children: Snippet;
    actions?: Snippet;
  } = $props();
</script>

<section class="rsi-panel border border-border bg-surface">
  {#if title || actions}
    <header
      class="rsi-scanline flex items-center justify-between border-b border-border px-3 py-2"
    >
      <h2
        class="font-display text-xs tracking-widest text-muted uppercase"
      >
        {title}
      </h2>
      {#if actions}
        <div class="flex items-center gap-2">{@render actions()}</div>
      {/if}
    </header>
  {/if}
  <div class="p-3">
    {@render children()}
  </div>
</section>

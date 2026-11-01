<script lang="ts">
  import type { View } from "./lib/types";
  import ReviewQueue from "./views/ReviewQueue.svelte";
  import Settings from "./views/Settings.svelte";
  import History from "./views/History.svelte";
  import Diagnostics from "./views/Diagnostics.svelte";

  // One window, four views, no router. PLAN.md §2.
  let view = $state<View>("review");

  const tabs: { id: View; label: string }[] = [
    { id: "review", label: "Review" },
    { id: "settings", label: "Settings" },
    { id: "history", label: "History" },
    { id: "diagnostics", label: "Diagnostics" },
  ];
</script>

<div class="flex h-full flex-col">
  <header class="border-b border-border bg-surface">
    <div class="flex items-baseline gap-3 px-4 pt-3">
      <h1 class="font-display text-sm tracking-[0.2em] text-accent uppercase">
        SC Asset Manager
      </h1>
      <span class="text-xs tracking-widest text-muted uppercase">
        Log Watcher
      </span>
    </div>

    <nav class="flex gap-1 px-4 pt-3">
      {#each tabs as tab (tab.id)}
        <button
          class="rsi-tab border-x border-t px-3 py-1 font-display text-xs tracking-widest uppercase transition-colors
                 {view === tab.id
            ? 'border-border-bright bg-bg text-accent'
            : 'border-transparent text-muted hover:text-text'}"
          onclick={() => (view = tab.id)}
          aria-current={view === tab.id ? "page" : undefined}
        >
          {tab.label}
        </button>
      {/each}
    </nav>
  </header>

  <main class="flex-1 overflow-y-auto p-4">
    {#if view === "review"}
      <ReviewQueue />
    {:else if view === "settings"}
      <Settings />
    {:else if view === "history"}
      <History />
    {:else}
      <Diagnostics />
    {/if}
  </main>

  <footer
    class="flex items-center gap-4 border-t border-border bg-surface px-4 py-1.5
           text-xs tracking-widest text-muted uppercase"
  >
    <span>Watcher: idle</span>
    <span class="text-border-bright">|</span>
    <span>Backend: not configured</span>
  </footer>
</div>

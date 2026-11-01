<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import Panel from "../components/Panel.svelte";
  import {
    getConfig,
    setConfig,
    configFilePath,
    parseFirebaseConfig,
    testBackend,
    detectLogPath,
    isAppError,
    type ConfigDto,
    type HealthReport,
  } from "../lib/ipc";

  let cfg = $state<ConfigDto | null>(null);
  let cfgPath = $state("");

  // The password is write-only from here: the backend never sends one back, so
  // an empty box means "leave whatever is stored alone", not "clear it".
  let password = $state("");
  let pastedFirebase = $state("");

  let saving = $state(false);
  let testing = $state(false);
  let message = $state<{ tone: "ok" | "warn" | "bad"; text: string } | null>(
    null,
  );
  let health = $state<HealthReport | null>(null);

  async function load() {
    cfg = await getConfig();
    cfgPath = await configFilePath();
  }

  function say(tone: "ok" | "warn" | "bad", text: string) {
    message = { tone, text };
  }

  function errText(e: unknown): string {
    if (isAppError(e)) return e.message;
    if (typeof e === "object" && e !== null && "message" in e)
      return String((e as { message: unknown }).message);
    return String(e);
  }

  async function save() {
    if (!cfg) return;
    saving = true;
    message = null;
    try {
      cfg = await setConfig({
        nickname: cfg.nickname,
        appUrl: cfg.appUrl,
        logPath: cfg.logPath,
        pollIntervalMs: cfg.pollIntervalMs,
        graceWindowMs: cfg.graceWindowMs,
        backendKind: cfg.backendKind,
        email: cfg.email,
        // Only send a password when one was typed.
        ...(password ? { password } : {}),
        firebase: cfg.firebase,
        pocketbase: cfg.pocketbase,
      });
      password = "";
      say("ok", "Saved.");
    } catch (e) {
      say("bad", errText(e));
    } finally {
      saving = false;
    }
  }

  async function applyPastedConfig() {
    if (!cfg || !pastedFirebase.trim()) return;
    try {
      cfg.firebase = await parseFirebaseConfig(pastedFirebase);
      pastedFirebase = "";
      say("ok", `Read config for project "${cfg.firebase.projectId}". Now Save.`);
    } catch (e) {
      say("bad", errText(e));
    }
  }

  async function test() {
    testing = true;
    message = null;
    health = null;
    try {
      health = await testBackend();
      const notes = health.notes.length;
      say(
        notes ? "warn" : "ok",
        notes ? `Connected, with ${notes} thing to check.` : "Connected.",
      );
    } catch (e) {
      say("bad", errText(e));
    } finally {
      testing = false;
    }
  }

  async function browseForLog() {
    const picked = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "Game log", extensions: ["log"] }],
    });
    if (typeof picked === "string" && cfg) cfg.logPath = picked;
  }

  async function detect() {
    const loc = await detectLogPath();
    if (!cfg) return;
    if (loc.candidates.length) {
      cfg.logPath = loc.candidates[0];
      say("ok", "Found an install.");
    } else {
      say("warn", "No Star Citizen install found. Browse to Game.log instead.");
    }
  }

  const label = "block text-xs uppercase tracking-widest text-muted mb-1";
  const input =
    "w-full border border-border bg-bg px-2 py-1 text-sm text-text outline-none focus:border-accent";

  load();
</script>

{#if cfg}
  <div class="flex flex-col gap-4">
    {#if message}
      <p
        class="border px-3 py-2 text-sm
               {message.tone === 'ok'
          ? 'border-accent text-accent'
          : message.tone === 'warn'
            ? 'border-warn-text text-warn-text'
            : 'border-danger text-danger-text'}"
      >
        {message.text}
      </p>
    {/if}

    <!-- ------------------------------------------------------------------ -->
    <Panel title="Backend">
      {#snippet actions()}
        <button
          class="border border-border px-3 py-1 font-display text-xs tracking-widest text-text uppercase disabled:opacity-40"
          onclick={test}
          disabled={testing}
        >
          {testing ? "Testing…" : "Test connection"}
        </button>
      {/snippet}

      <div class="flex gap-1">
        {#each [["firebase", "Firebase"], ["pocketbase", "PocketBase"]] as [id, name] (id)}
          <button
            class="border px-3 py-1 font-display text-xs tracking-widest uppercase
                   {cfg.backendKind === id
              ? 'border-accent bg-accent text-bg'
              : 'border-border text-muted hover:text-text'}"
            onclick={() => cfg && (cfg.backendKind = id as typeof cfg.backendKind)}
          >
            {name}
          </button>
        {/each}
      </div>

      {#if cfg.backendKind === "firebase"}
        <div class="mt-3">
          <label class={label} for="fb-paste">
            Firebase web config — paste the same JSON the web app uses
          </label>
          <textarea
            id="fb-paste"
            class="{input} h-24 font-mono text-xs"
            placeholder={'{ "apiKey": "…", "projectId": "…", … }'}
            bind:value={pastedFirebase}
          ></textarea>
          <button
            class="mt-1 border border-border px-3 py-1 font-display text-xs tracking-widest text-text uppercase disabled:opacity-40"
            onclick={applyPastedConfig}
            disabled={!pastedFirebase.trim()}
          >
            Read config
          </button>
        </div>

        <dl class="mt-3 grid grid-cols-2 gap-x-6 gap-y-1 text-sm">
          <div>
            <dt class="text-xs tracking-widest text-muted uppercase">Project</dt>
            <dd class="font-mono text-xs {cfg.firebase.projectId ? 'text-accent' : 'text-warn-text'}">
              {cfg.firebase.projectId || "not set"}
            </dd>
          </div>
          <div>
            <dt class="text-xs tracking-widest text-muted uppercase">API key</dt>
            <dd class="font-mono text-xs {cfg.firebase.apiKey ? 'text-muted' : 'text-warn-text'}">
              {cfg.firebase.apiKey ? "set" : "not set"}
            </dd>
          </div>
        </dl>
      {:else}
        <div class="mt-3">
          <label class={label} for="pb-url">PocketBase URL</label>
          <input
            id="pb-url"
            class={input}
            bind:value={cfg.pocketbase.url}
            placeholder="https://pb.example.org"
          />
          <p class="mt-1 text-xs text-warn-text">
            PocketBase support is not built yet — it lands in Phase 6.
          </p>
        </div>
      {/if}

      <div class="mt-3 grid gap-3 sm:grid-cols-2">
        <div>
          <label class={label} for="email">Account email</label>
          <input id="email" class={input} bind:value={cfg.email} autocomplete="off" />
        </div>
        <div>
          <label class={label} for="pw">
            Password {cfg.passwordSet ? "— stored; type to replace" : ""}
          </label>
          <input
            id="pw"
            class={input}
            type="password"
            bind:value={password}
            placeholder={cfg.passwordSet ? "••••••••" : "required"}
            autocomplete="off"
          />
        </div>
      </div>

      <p class="mt-3 border-l-2 border-warn-text pl-2 text-xs text-muted">
        The password is stored in cleartext in <code>{cfgPath}</code>. It is
        never sent back to this window, never logged and never put in an error
        message — but anyone with that file can read it. Use an ordinary group
        member account, not an admin one.
      </p>
    </Panel>

    <!-- ------------------------------------------------------------------ -->
    {#if health}
      <Panel title="Connection">
        <dl class="grid grid-cols-2 gap-x-6 gap-y-1 text-sm sm:grid-cols-4">
          <div>
            <dt class="text-xs tracking-widest text-muted uppercase">Signed in</dt>
            <dd class="font-display text-accent">{health.email}</dd>
          </div>
          <div>
            <dt class="text-xs tracking-widest text-muted uppercase">Assets</dt>
            <dd class="font-display text-accent">{health.assetCount ?? "—"}</dd>
          </div>
          <div>
            <dt class="text-xs tracking-widest text-muted uppercase">Trades</dt>
            <dd class="font-display text-accent">{health.tradeCount ?? "—"}</dd>
          </div>
          <div>
            <dt class="text-xs tracking-widest text-muted uppercase">Nickname</dt>
            <dd class="font-display {health.configuredNickname ? 'text-accent' : 'text-warn-text'}">
              {health.configuredNickname || "unset"}
            </dd>
          </div>
        </dl>
        {#each health.notes as note (note)}
          <p class="mt-2 border-l-2 border-warn-text pl-2 text-sm text-warn-text">
            {note}
          </p>
        {/each}
      </Panel>
    {/if}

    <!-- ------------------------------------------------------------------ -->
    <Panel title="Identity">
      <div class="grid gap-3 sm:grid-cols-2">
        <div>
          <label class={label} for="nick">
            Nickname — must match SC-Asset-Manager exactly
          </label>
          <input id="nick" class={input} bind:value={cfg.nickname} />
        </div>
        <div>
          <label class={label} for="url">SC-Asset-Manager URL</label>
          <input id="url" class={input} bind:value={cfg.appUrl} />
        </div>
      </div>
      <p class="mt-2 text-xs text-muted">
        One character's difference in the nickname makes every record this
        client writes unsellable and un-pushable for a user-role member.
      </p>
    </Panel>

    <!-- ------------------------------------------------------------------ -->
    <Panel title="Game.log">
      {#snippet actions()}
        <button
          class="border border-border px-3 py-1 font-display text-xs tracking-widest text-text uppercase"
          onclick={detect}>Detect</button
        >
        <button
          class="border border-border px-3 py-1 font-display text-xs tracking-widest text-text uppercase"
          onclick={browseForLog}>Browse</button
        >
      {/snippet}
      <input
        class="{input} font-mono text-xs"
        bind:value={cfg.logPath}
        placeholder="auto-detect"
      />

      <div class="mt-3 grid gap-3 sm:grid-cols-2">
        <div>
          <label class={label} for="poll">Poll interval (ms)</label>
          <input
            id="poll"
            class={input}
            type="number"
            min="250"
            max="60000"
            bind:value={cfg.pollIntervalMs}
          />
        </div>
        <div>
          <label class={label} for="grace">Grace window (ms)</label>
          <input
            id="grace"
            class={input}
            type="number"
            min="0"
            max="60000"
            bind:value={cfg.graceWindowMs}
          />
          <p class="mt-1 text-xs text-muted">
            How long a detected trade waits for a failure line before entering
            the queue.
          </p>
        </div>
      </div>
    </Panel>

    <div class="flex items-center gap-3">
      <button
        class="bg-accent px-4 py-1.5 font-display text-xs tracking-widest text-bg uppercase disabled:opacity-40"
        onclick={save}
        disabled={saving}
      >
        {saving ? "Saving…" : "Save"}
      </button>
      <span class="font-mono text-xs text-muted">{cfgPath}</span>
    </div>
  </div>
{:else}
  <p class="text-sm text-muted">Loading…</p>
{/if}

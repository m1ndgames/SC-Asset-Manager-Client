/**
 * The one place that knows whether the watcher is running and whether the
 * backend is usable.
 *
 * It exists because the footer and the Diagnostics panel both need it, and two
 * independent subscriptions would eventually disagree — showing "watching" in
 * one place and "idle" in the other is worse than showing neither. Subscribed
 * once, from `App.svelte`, for the lifetime of the window.
 */
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  EV,
  getConfig,
  watcherStatus,
  type ConfigDto,
  type WatchStatus,
} from "./ipc";

/** `null` means "not known yet", which is distinct from "not running". */
export const app = $state<{
  watcher: WatchStatus | null;
  config: ConfigDto | null;
  /** Set when even reading the status failed — worth saying out loud. */
  error: string | null;
}>({
  watcher: null,
  config: null,
  error: null,
});

let started = false;

/** Idempotent: calling it twice must not double-subscribe. */
export async function initStatus(): Promise<UnlistenFn> {
  if (started) return () => {};
  started = true;

  const subs: Promise<UnlistenFn>[] = [
    listen<WatchStatus>(EV.status, (e) => {
      app.watcher = e.payload;
    }),
  ];

  await Promise.all([refreshConfig(), refreshWatcher()]);

  const unlisten = await Promise.all(subs);
  return () => {
    unlisten.forEach((un) => un());
    started = false;
  };
}

export async function refreshConfig() {
  try {
    app.config = await getConfig();
  } catch (e) {
    app.error = String(e);
  }
}

export async function refreshWatcher() {
  try {
    app.watcher = await watcherStatus();
  } catch (e) {
    app.error = String(e);
  }
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

export type Tone = "ok" | "warn" | "off";

export interface Chip {
  label: string;
  tone: Tone;
  title?: string;
}

export function watcherChip(): Chip {
  const w = app.watcher;
  if (!w || !w.running) {
    return {
      label: "Watcher: idle",
      tone: "off",
      title: "Not reading Game.log. Start it in Diagnostics.",
    };
  }
  if (!w.logPresent) {
    return {
      label: "Watcher: waiting for Game.log",
      tone: "warn",
      title: "Watching, but no file at that path yet — the game is not running.",
    };
  }
  if (w.pending > 0) {
    // A detection is mid-grace-window right now.
    return {
      label: `Watcher: ${w.pending} in grace`,
      tone: "ok",
      title: `Held for ${w.graceWindowMs}ms in case a failure line follows.`,
    };
  }
  return {
    label: `Watcher: watching · ${w.promoted} detected`,
    tone: "ok",
    title: `${w.linesSeen.toLocaleString("en-US")} lines read.`,
  };
}

export function backendChip(): Chip {
  const c = app.config;
  if (!c) return { label: "Backend: —", tone: "off" };

  const name = c.backendKind === "firebase" ? "Firebase" : "PocketBase";
  if (!c.backendReady) {
    // Say which part is missing: "not configured" sends people hunting.
    const missing: string[] = [];
    if (!c.email.trim()) missing.push("email");
    if (!c.passwordSet) missing.push("password");
    if (c.backendKind === "firebase") {
      if (!c.firebase.apiKey?.trim()) missing.push("apiKey");
      if (!c.firebase.projectId?.trim()) missing.push("projectId");
    } else if (!c.pocketbase.url?.trim()) {
      missing.push("url");
    }
    return {
      label: `Backend: ${name} incomplete`,
      tone: "warn",
      title: missing.length
        ? `Missing in Settings: ${missing.join(", ")}`
        : "Not fully configured — see Settings.",
    };
  }

  // Deliberately not "connected": nothing here has made a request. Claiming a
  // live connection the app has not verified is the kind of status that gets
  // believed and then disbelieved.
  return {
    label: `Backend: ${name} configured`,
    tone: "ok",
    title: `${c.firebase.projectId || c.pocketbase.url} as ${c.email}. Use Test connection in Settings to verify.`,
  };
}

export function nicknameChip(): Chip | null {
  const nick = app.config?.nickname?.trim();
  if (!nick) {
    return {
      label: "Nickname: unset",
      tone: "warn",
      title:
        "Records written without a nickname cannot be sold by a user-role member in the web app.",
    };
  }
  return { label: nick, tone: "ok", title: "Writes are attributed to this name." };
}

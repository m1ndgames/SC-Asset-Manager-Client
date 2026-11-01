/**
 * The only way the frontend talks to anything.
 *
 * All network and filesystem I/O happens in Rust — the webview makes zero
 * outbound requests, which is what keeps the CSP tight and credentials out of
 * the renderer entirely. If you find yourself reaching for `fetch` here, the
 * work belongs in a command instead.
 */
import { invoke } from "@tauri-apps/api/core";
import type { ParityCheck, TradeKind } from "./types";

/** Typed, discriminable error from a command. Mirrors Rust's `AppError`. */
export interface AppError {
  kind: "logNotFound" | "logUnreadable" | "config";
  message: string;
}

export function isAppError(e: unknown): e is AppError {
  return typeof e === "object" && e !== null && "kind" in e && "message" in e;
}

export interface CargoBoxGroup {
  boxSize: number;
  unitAmount: number;
}

export interface TradeLine {
  kind: TradeKind;
  id: string;
  logTimestamp: string;
  resourceGuid: string;
  shopId: string;
  shopName: string | null;
  kioskId: string | null;
  quantityRaw: string;
  quantityScu: number;
  unitPricePerScu: number;
  totalAuec: number;
  boxGroups: CargoBoxGroup[];
  transactionMode: string | null;
  parity: ParityCheck;
  priceCrossCheck: number | null;
  fields: Record<string, string>;
}

export interface DetectedTrade extends TradeLine {
  locationIdHint: string | null;
}

export interface TxErrorLine {
  logTimestamp: string;
  result: string;
  txType: string | null;
  playerId: string | null;
}

export interface DiscardedTrade {
  trade: TradeLine;
  error: TxErrorLine;
  deltaMs: number;
}

export interface ScanReport {
  totalLines: number;
  classMatched: number;
  buys: number;
  sells: number;
  errors: number;
  trades: DetectedTrade[];
  discarded: DiscardedTrade[];
  malformed: unknown[];
  unmatchedErrors: TxErrorLine[];
  locationIds: string[];
  shopNames: string[];
  parityMismatches: number;
  priceDrift: number;
}

export interface LogLocation {
  path: string | null;
  candidates: string[];
}

export const detectLogPath = (): Promise<LogLocation> =>
  invoke("detect_log_path");

export const rescanLog = (
  path?: string,
  holdMs?: number,
): Promise<ScanReport> => invoke("rescan_log", { path, holdMs });

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

export type BackendKind = "firebase" | "pocketbase";

export interface FirebaseConfig {
  apiKey: string;
  authDomain: string;
  projectId: string;
  storageBucket: string;
  messagingSenderId: string;
  appId: string;
  measurementId?: string;
}

export interface PocketbaseConfig {
  url: string;
}

/** Note the absence of `password` — it never crosses into the webview. */
export interface ConfigDto {
  schema: number;
  nickname: string;
  appUrl: string;
  logPath: string;
  pollIntervalMs: number;
  graceWindowMs: number;
  backendKind: BackendKind;
  email: string;
  passwordSet: boolean;
  /** Computed in Rust by `Config::backend_ready`, so the rule lives in one place. */
  backendReady: boolean;
  firebase: FirebaseConfig;
  pocketbase: PocketbaseConfig;
}

/** Omit a field to leave it unchanged. Omit `password` to keep the stored one. */
export interface ConfigPatch {
  nickname?: string;
  appUrl?: string;
  logPath?: string;
  pollIntervalMs?: number;
  graceWindowMs?: number;
  backendKind?: BackendKind;
  email?: string;
  password?: string;
  firebase?: FirebaseConfig;
  pocketbase?: PocketbaseConfig;
}

export interface HealthReport {
  ok: boolean;
  backend: BackendKind;
  email: string;
  userId: string;
  assetCount: number | null;
  tradeCount: number | null;
  configuredNickname: string;
  knownNicknames: string[];
  notes: string[];
}

export interface BackendError {
  kind: "notConfigured" | "auth" | "network" | "protocol" | "other";
  message: string;
}

export const getConfig = (): Promise<ConfigDto> => invoke("get_config");

export const setConfig = (patch: ConfigPatch): Promise<ConfigDto> =>
  invoke("set_config", { patch });

export const configFilePath = (): Promise<string> => invoke("config_file_path");

export const parseFirebaseConfig = (text: string): Promise<FirebaseConfig> =>
  invoke("parse_firebase_config", { text });

export const testBackend = (): Promise<HealthReport> => invoke("test_backend");

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

/** What starting the watcher would do. Mirrors Rust's `ColdStart`. */
export type ColdStart =
  | { kind: "resume"; offset: number }
  | { kind: "backfill"; reason: "replaced" | "truncated" | "differentPath" }
  | { kind: "firstRun"; trades: number; endOffset: number }
  | { kind: "noLog" };

export interface WatchStatus {
  path: string;
  running: boolean;
  logPresent: boolean;
  /** Where reading has got to. */
  readOffset: number;
  /**
   * Where resolution has got to — the offset that is actually persisted. It
   * sits behind `readOffset` exactly while a detection is inside its grace
   * window, which is the property that stops a crash losing a trade.
   */
  safeOffset: number;
  pending: number;
  graceWindowMs: number;
  linesSeen: number;
  classMatched: number;
  promoted: number;
  discarded: number;
  malformed: number;
  rotations: number;
  lastLogTimestamp: string | null;
}

/** Event names emitted by the watcher thread. */
export const EV = {
  status: "watcher://status",
  rotated: "watcher://log-rotated",
  detected: "tx://detected",
  discarded: "tx://discarded",
  malformed: "tx://malformed",
} as const;

export const planWatchStart = (): Promise<ColdStart> =>
  invoke("plan_watch_start");

/**
 * Start watching. `offset` is the user's answer to a `firstRun` plan; omit it
 * to take the plan's own default, which is start-from-now — never a silent
 * import of a session nobody asked to import.
 */
export const startWatcher = (offset?: number): Promise<WatchStatus> =>
  invoke("start_watcher", { offset });

export const stopWatcher = (): Promise<void> => invoke("stop_watcher");

export const watcherStatus = (): Promise<WatchStatus | null> =>
  invoke("watcher_status");

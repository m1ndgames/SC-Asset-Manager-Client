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

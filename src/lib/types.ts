/**
 * Mirrors of SC-Asset-Manager's src/lib/types.ts, kept field-for-field so the
 * two cannot drift. INTEGRATIONS.md §4/§5 is the contract.
 *
 * NOTE: 'μSCU' starts with U+03BC GREEK SMALL LETTER MU, *not* U+00B5 MICRO
 * SIGN. The difference is invisible in a diff. There is a Rust test asserting
 * it; do not "clean up" this literal.
 */
export type CargoUnit = "SCU" | "cSCU" | "μSCU";

export interface FirebaseConfig {
  apiKey: string;
  authDomain: string;
  projectId: string;
  storageBucket: string;
  messagingSenderId: string;
  appId: string;
}

export interface Asset {
  id: string;
  item: string;
  amount: number;
  unit?: CargoUnit;
  buyPrice: number;
  location: string;
  quality?: number;
  createdAt: string;
  loggedBy?: string;
  uexBuyId?: number;
  source?: string;
}

export interface Trade {
  id: string;
  assetId: string;
  item: string;
  amountSold: number;
  unit?: CargoUnit;
  buyUnit?: CargoUnit;
  buyPrice?: number;
  buyLocation?: string;
  sellPrice: number;
  sellLocation: string;
  soldAt: string;
  loggedBy?: string;
  uexSellId?: number;
  source?: string;
}

/** Which way a parsed log line went. */
export type TradeKind = "buy" | "sell";

/**
 * How much to trust boxSize x unitAmount == quantity for this line.
 *
 * This is NOT a boolean. PLAN.md §3.5 treated the identity as universal
 * ("verified against all seven real lines"); across 91 real trade lines it
 * fails on 10, all structurally:
 *   - buys:                      21/21 hold  -> a real invariant
 *   - ResourceContainer sells:   48/49 hold  -> advisory
 *   - Location sells (TDD):       9/21 fail  -> meaningless, suppressed
 * At a TDD kiosk you sell an arbitrary amount from station inventory and
 * "Cargo Box Data" describes a single representative box. Warning on those
 * would train the user to ignore amber and destroy the drift signal.
 */
export type ParityCheck =
  | { kind: "ok" }
  | { kind: "mismatch"; expectedScu: number; actualScu: number }
  | { kind: "notApplicable"; reason: string };

export type View = "review" | "settings" | "history" | "diagnostics";

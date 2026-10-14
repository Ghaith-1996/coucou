// The desktop assistant's contracts, version 2 — the TypeScript side of
// src-tauri/src/assistant/contracts.rs. tests/fixtures/assistant-contracts.json
// keeps the two in step.
//
// Rust decides and runs assistant actions; the page only shows them. Nothing
// here can answer an agent's permission request: those go through the
// approval_* commands, from a click on the agent's own card, and the
// assistant's code never calls them (tests/assistant-contracts.test.mjs).

/** Version 2 adds `pill-set` to the frozen v1 action registry. */
export const CONTRACT_VERSION = 2;

/** The event the island listens to for voice capture. */
export const VOICE_EVENT = "assistant-voice";

export type ActionKind = "chat-draft" | "pill-set" | "insert-text" | "launch-app" | "ask-region";

/** The closed registry. `available`: implemented in this version. */
export const ACTION_KINDS: readonly { kind: ActionKind; available: boolean; idempotent: boolean }[] = [
  { kind: "chat-draft", available: true, idempotent: true },
  { kind: "pill-set", available: true, idempotent: true },
  { kind: "insert-text", available: true, idempotent: false },
  { kind: "launch-app", available: true, idempotent: false },
  { kind: "ask-region", available: false, idempotent: false },
];

export interface AssistantAction {
  v: number;
  domain: "desktop-assistant";
  id: string;
  kind: ActionKind;
  /** Checked against the kind's closed schema by Rust, never trusted as is. */
  args: unknown;
  intentId: string;
  /** Unix milliseconds. */
  expiresAtMs: number;
}

export type Decision = "allow" | "ask" | "deny";

export type ErrorCode =
  | "version" | "invalid-args" | "unavailable" | "expired"
  | "grant-mismatch" | "cancelled" | "unverified" | "failed";

declare const nonBlankEvidence: unique symbol;
type NonBlankEvidence = string & { readonly [nonBlankEvidence]: true };

/** Success evidence must pass succeededReceipt's runtime validation. */
export type ActionReceipt = {
  actionId: string;
  errorCode?: ErrorCode;
} & (
  | { readonly outcome: "succeeded"; readonly evidenceRef: NonBlankEvidence }
  | { readonly outcome: "failed" | "cancelled" | "unknown"; readonly evidenceRef?: string }
);

/** Build a success only after observing an effect; keep the wire shape unchanged. */
export function succeededReceipt(actionId: string, evidenceRef: string): Extract<ActionReceipt, { outcome: "succeeded" }> {
  // Unicode White_Space matches Rust's str::trim, including U+0085.
  if (typeof evidenceRef !== "string" || /^\p{White_Space}*$/u.test(evidenceRef)) {
    throw new TypeError("a successful receipt requires nonblank evidenceRef");
  }
  return { actionId, outcome: "succeeded", evidenceRef: evidenceRef as NonBlankEvidence };
}

/**
 * Rust's grant for an action shown for confirmation (policy.rs `Grant`). The
 * page only hands it back on the user's click; Rust recomputes the binding,
 * so a changed one is worthless.
 */
export interface AppGrant {
  readonly actionId: string;
  readonly digest: string;
  readonly expiresAtMs: number;
}

/** Why another app's field can't take dictated words (dictation.rs `Refusal`). Nothing was written. */
export type InsertRefusal = "focus-moved" | "gone" | "protected" | "read-only" | "unsupported" | "busy" | "value-changed";

/** assistant_insert_offer: "Insert into <label>", or why the field can't take it.
 * `copyAllowed` requires native evidence from this check of a non-protected field.
 */
export type InsertOfferReply =
  | { status: "ready"; label: string; grant: AppGrant }
  | { status: "refused"; reason: InsertRefusal; copyAllowed: boolean };

/** assistant_insert_text: written (or maybe), kept on the card, or put in the chat instead. */
export type InsertTextReply =
  | { status: "done"; receipt: ActionReceipt }
  | { status: "kept"; reason: InsertRefusal; copyAllowed: boolean }
  | { status: "failed"; receipt: ActionReceipt; draft: { actionId: string; text: string } | null };

export type VoiceState = "idle" | "listening" | "processing" | "preview" | "speaking" | "error";

export const VOICE_STATES: readonly VoiceState[] = [
  "idle", "listening", "processing", "preview", "speaking", "error",
];

export type VoiceError = "no-microphone" | "microphone-denied" | "silence" | "provider";

export const VOICE_ERRORS: readonly VoiceError[] = [
  "no-microphone", "microphone-denied", "silence", "provider",
];

export interface VoiceEvent {
  v: number;
  /** Every event of one capture carries the same id; others are stale. */
  captureId: string;
  state: VoiceState;
  /** Input level 0…1 while listening. */
  level?: number;
  elapsedMs?: number;
  transcript?: string;
  /** The transcript came from the simulated provider, not real speech. */
  simulated?: boolean;
  error?: VoiceError;
}

/** A voice event as it arrived, or null when it isn't one this version knows. */
export function parseVoiceEvent(raw: unknown): VoiceEvent | null {
  if (typeof raw !== "object" || raw === null) return null;
  const e = raw as Record<string, unknown>;
  if (e.v !== CONTRACT_VERSION) return null;
  if (typeof e.captureId !== "string" || e.captureId === "") return null;
  if (!VOICE_STATES.includes(e.state as VoiceState)) return null;
  const optional = (key: string, ok: (value: unknown) => boolean) => e[key] === undefined || ok(e[key]);
  const isNumber = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value);
  if (!optional("level", (x) => isNumber(x) && x >= 0 && x <= 1)) return null;
  if (!optional("elapsedMs", (x) => isNumber(x) && Number.isSafeInteger(x) && x >= 0)) return null;
  if (!optional("transcript", (x) => typeof x === "string")) return null;
  if (!optional("simulated", (x) => typeof x === "boolean")) return null;
  if (!optional("error", (x) => VOICE_ERRORS.includes(x as VoiceError))) return null;
  return {
    v: CONTRACT_VERSION,
    captureId: e.captureId,
    state: e.state as VoiceState,
    ...(e.level !== undefined && { level: e.level as number }),
    ...(e.elapsedMs !== undefined && { elapsedMs: e.elapsedMs as number }),
    ...(e.transcript !== undefined && { transcript: e.transcript as string }),
    ...(e.simulated !== undefined && { simulated: e.simulated as boolean }),
    ...(e.error !== undefined && { error: e.error as VoiceError }),
  };
}

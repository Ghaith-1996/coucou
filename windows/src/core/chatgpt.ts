// The chat on the user's ChatGPT plan — the island's side of chatgpt.rs: the
// two models it always offers, the reasoning efforts each one takes, and the
// effort remembered per model. Pure data and helpers, testable without a
// webview. tests/fixtures/chatgpt-efforts.json holds the same table for
// Rust's tests, so the two never disagree.

import type { Settings } from "./state";
import { N_ } from "../i18n/i18n";

export const CHATGPT = "chatgpt";
export const LUNA = "gpt-6-luna";
export const SOL = "gpt-6.1-sol";
export const DEFAULT_EFFORT = "medium";

export type Effort = "none" | "low" | "medium" | "high" | "xhigh" | "max";

const ALL: readonly Effort[] = ["none", "low", "medium", "high", "xhigh", "max"];

/** What the effort picker shows for each value. */
export const EFFORT_LABELS: Record<Effort, string> = {
  none: N_("None"),
  low: N_("Low"),
  medium: N_("Medium"),
  high: N_("High"),
  xhigh: N_("Extra High"),
  max: N_("Max"),
};

/** The `reasoning.effort` values a model takes (GPT-6.1 Sol has no None). */
export function effortsFor(model: string): readonly Effort[] {
  if (model === LUNA) return ALL;
  if (model === SOL) return ALL.slice(1);
  return ALL.slice(1, 4);
}

/** The effort saved for `model`, or Medium when there is none or it doesn't apply. */
export function effortFor(settings: Settings, model: string): Effort {
  const list = effortsFor(model);
  const saved = settings.chatEfforts?.[model] as Effort | undefined;
  if (saved && list.includes(saved)) return saved;
  return list.includes(DEFAULT_EFFORT) ? DEFAULT_EFFORT : list[0];
}

/** `settings` with `effort` remembered for `model`; an effort it doesn't take is refused. */
export function withEffort(settings: Settings, model: string, effort: string): Settings | null {
  if (!effortsFor(model).includes(effort as Effort)) return null;
  return { ...settings, chatEfforts: { ...(settings.chatEfforts ?? {}), [model]: effort } };
}

/** How a model is named under its answer, when the catalog's name isn't known. */
export function modelLabel(model: string): string {
  if (model === LUNA) return "GPT-6 Luna";
  if (model === SOL) return "GPT-6.1 Sol";
  return model;
}

/** chatgpt.rs `Status`: never a token. */
export interface ChatGptStatus {
  connected: boolean;
  /** Either saved credential remains, even if only the sign-in hint does. */
  hasSavedCredentials: boolean;
  email: string | null;
  planEnabled: boolean;
  needsReconnect: boolean;
  showWelcome: boolean;
  manageUsageUrl: string;
}

/** chatgpt.rs `DisconnectResult`: independent outcomes, never credentials. */
export interface ChatGptDisconnectResult {
  /** Null means there was no refresh token to revoke. */
  revocationConfirmed: boolean | null;
  localCleanupSucceeded: boolean;
}

/** chatgpt.rs `CatalogModel`. */
export interface ChatGptModel {
  id: string;
  label: string;
  available: boolean;
  efforts: Effort[];
}

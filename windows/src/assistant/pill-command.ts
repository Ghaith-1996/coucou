// What a spoken pill command would change — VoiceActionRunner.swift's pill
// cases, as a plan the voice card shows before anything happens. Applying it
// is a `pill-set` action that Rust's policy decides (voice.ts).
//
// The Mac acts at once and asks a follow-up question when the island is full;
// here the card shows the change and one click (or Enter) makes it, and a
// command that can't be done says why instead of asking.

import { MAX_DECLARED, mainPillChoices, pillDefinition, sanitizeDeclared, type HostOs } from "../core/pills";
import { t } from "../i18n/i18n";
import type { Intent } from "./intents";

export interface Declared {
  mainPill: string;
  activeIntegrations: string[];
}

export type PillPlan =
  /** `label` says what the button does; `next` is the whole result. */
  | { ok: true; label: string; next: Declared }
  | { ok: false; reason: string };

const PILL_KINDS = new Set(["pillAdd", "pillAddMultiple", "pillRemove", "pillRemoveMultiple", "pillSetMain", "pillReplace", "pillOnly"]);

const name = (id: string) => pillDefinition(id)?.name ?? id;
const names = (ids: string[]) => ids.map(name).join(", ");

/**
 * The plan for `intents` from `current`, or null when they aren't (all) pill
 * commands: then there is nothing to offer but the chat.
 */
export function planPills(intents: Intent[], current: Declared, os: HostOs): PillPlan | null {
  if (intents.length === 0 || !intents.every((i) => PILL_KINDS.has(i.kind))) return null;
  const mains = new Set(mainPillChoices(os).map((p) => p.id));
  let main = current.mainPill;
  let active = [...current.activeIntegrations];
  const labels: string[] = [];
  const full = () => t("Already {count} pills: remove one first.", { count: MAX_DECLARED });

  const add = (id: string): boolean => {
    if (id === main || active.includes(id)) return false;
    if (active.length >= MAX_DECLARED) return false;
    active.push(id);
    return true;
  };

  // Match the Mac's multi-action flow: free requested slots before checking
  // additions against capacity, while preserving order within each group.
  const removes = (intent: Intent) => intent.kind === "pillRemove" || intent.kind === "pillRemoveMultiple";
  const ordered = [...intents.filter(removes), ...intents.filter((intent) => !removes(intent))];
  for (const intent of ordered) {
    switch (intent.kind) {
      case "pillAdd": {
        if (intent.id === main || active.includes(intent.id)) return { ok: false, reason: t("{name} is already in the island.", { name: name(intent.id) }) };
        if (!add(intent.id)) return { ok: false, reason: full() };
        labels.push(t("Add {names}", { names: name(intent.id) }));
        break;
      }
      case "pillAddMultiple": {
        const added = intent.ids.filter((x) => add(x));
        if (added.length === 0) {
          return { ok: false, reason: active.length >= MAX_DECLARED ? full() : t("{name} is already in the island.", { name: names(intent.ids) }) };
        }
        labels.push(t("Add {names}", { names: names(added) }));
        break;
      }
      case "pillRemove": {
        if (!active.includes(intent.id)) return { ok: false, reason: t("{name} isn't in the island.", { name: name(intent.id) }) };
        active = active.filter((x) => x !== intent.id);
        labels.push(t("Remove {names}", { names: name(intent.id) }));
        break;
      }
      case "pillRemoveMultiple": {
        const removed = intent.ids.filter((x) => active.includes(x));
        if (removed.length === 0) return { ok: false, reason: t("{name} isn't in the island.", { name: names(intent.ids) }) };
        active = active.filter((x) => !removed.includes(x));
        labels.push(t("Remove {names}", { names: names(removed) }));
        break;
      }
      case "pillSetMain": {
        if (!mains.has(intent.id)) return { ok: false, reason: t("{name} can't be the main pill.", { name: name(intent.id) }) };
        if (intent.id === main) return { ok: false, reason: t("{name} is already in the island.", { name: name(intent.id) }) };
        main = intent.id;
        active = active.filter((x) => x !== main);
        labels.push(t("Make {name} the main pill", { name: name(intent.id) }));
        break;
      }
      case "pillReplace": {
        if (intent.old === main) {
          if (!mains.has(intent.new)) return { ok: false, reason: t("{name} can't be the main pill.", { name: name(intent.new) }) };
          main = intent.new;
          active = active.filter((x) => x !== main);
        } else {
          active = active.filter((x) => x !== intent.old);
          if (intent.new !== main && !active.includes(intent.new)) {
            if (active.length >= MAX_DECLARED) return { ok: false, reason: full() };
            active.push(intent.new);
          }
        }
        labels.push(t("Replace {old} with {new}", { old: name(intent.old), new: name(intent.new) }));
        break;
      }
      case "pillOnly": {
        const kept = [...new Set(intent.ids.filter((x) => x !== main))];
        if (kept.length > MAX_DECLARED) return { ok: false, reason: full() };
        active = kept;
        labels.push(t("Keep only {names}", { names: names(active) }));
        break;
      }
    }
  }

  const next = sanitizeDeclared({ mainPill: main, activeIntegrations: active }, os);
  const before = sanitizeDeclared(current, os);
  if (next.mainPill === before.mainPill && sameList(next.activeIntegrations, before.activeIntegrations)) {
    return { ok: false, reason: t("Already active") };
  }
  return { ok: true, label: labels.join(" · "), next };
}

export function sameList(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

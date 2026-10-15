// Voice capture on the island — what the `voiceCapture` shortcut does, and
// what the voice card (voice-view.ts) shows and calls.
//
// Rust owns the capture (src-tauri/src/assistant/voice) and says where it is
// with `assistant-voice` events. The island holds two rules of its own:
//   * An agent's waiting permission or question comes first. The shortcut
//     opens that card instead of the microphone, and a card that arrives while
//     listening cancels the capture (its audio is dropped).
//   * The microphone is only open while its card is on screen. Leaving the
//     card, closing the island or hiding it cancels the capture. The card
//     keeps the island pinned meanwhile, so the auto-close never cuts a
//     capture short (the island's 15 s default is under the 30 s limit).
//
// A transcript that is a pill command (intents.ts, the Mac's parser) is shown
// as that change on the card, next to "Use in chat"; doing it is a `pill-set`
// action Rust's policy decides, applied the way Settings applies it.
//
// "Open <app>" asks Rust for the app the Start menu has under that name. Rust
// keeps the `launch-app` action and gives back the app's name and a grant;
// the card shows "Open <that name>", and the user's click hands the grant
// back, Rust checks it and opens the app. Nothing opens without that click.
//
// Dictation: when the capture started in another app's text field, Rust
// remembered that field. A plain transcript (no command) is then offered as
// "Insert into <app>" if the field can take it. The click hands Rust's grant
// back; Rust checks the field again and writes the words there, never
// sending anything. If the field moved or went away, nothing is written and
// the transcript stays on the card; if writing fails, the words land in the
// chat instead.

import { Bridge, onEvent } from "../core/bridge";
import { AudioMode } from "./audio-mode";
import type { BotEmoteName, IslandViewName } from "../core/layout";
import { Sound } from "../core/sound";
import { HOST_OS, availablePills, sanitizeDeclared } from "../core/pills";
import { State } from "../core/state";
import {
  CONTRACT_VERSION, VOICE_EVENT, parseVoiceEvent, succeededReceipt,
  type AppGrant, type InsertRefusal, type VoiceEvent,
} from "./contracts";
import { writeClipboard } from "../views/markdown";
import { parseTranscript } from "./intents";
import { planPills, sameList, type Declared, type PillPlan } from "./pill-command";

/** What voice needs from the island. */
export interface VoiceHost {
  alert(view: IslandViewName): void;
  collapse(): void;
  emote(name: BotEmoteName): void;
  takeKeyboard(): void;
  setPinned(on: boolean): void;
  /** Opens the chat with `text` in its field, unsent. True when it is there. */
  showChatDraft(text: string): boolean;
  /** Shows these pills and saves them, as Settings does. True when they are what it shows. */
  applyPills(next: Declared): boolean;
}

/** How long an error stays on the card before it goes by itself. */
const ERROR_SHOWN_MS = 4000;
/** Matches Rust's PREVIEW_TTL: a transcript stops waiting after two minutes. */
const PREVIEW_TTL_MS = 120_000;

let host: VoiceHost | null = null;
/** True once the card was seen on screen for the current capture. */
let shown = false;
let errorTimer: ReturnType<typeof setTimeout> | null = null;
let previewTimer: ReturnType<typeof setTimeout> | null = null;
/** Survives Rust's normal idle-before-reply event, but not a new capture or Cancel. */
let generation = 0;
/** The card pinned the island; `userPinned` is what it was before. */
let pinnedByVoice = false;
let userPinned = false;
/** Only events belonging to a capture this page started may change its card. */
let captureId: string | null = null;
/** Cancel requests return before the native microphone closes. */
let releasing: string | null = null;
/** Rust may emit before the command reply identifies the new capture. */
let starting: {
  events: Map<string, VoiceEvent>;
  cancelled: boolean;
  stop: boolean;
  view: typeof State.view;
  mode: typeof State.mode;
} | null = null;

export const Voice = {
  /** The latest event of the current capture; null when there is none. */
  event: null as VoiceEvent | null,
  /** Derive the preview from current settings, including initially blocked plans.
   * Render/keyboard listeners see the same up-to-date plan regardless of their
   * subscription order; reading it never executes an action or renews the TTL.
   */
  get command(): PillPlan | null {
    const event = this.event;
    if (event?.state !== "preview" || event.simulated) return null;
    return planPills(
      parseTranscript(event.transcript ?? "", availablePills(HOST_OS)),
      sanitizeDeclared(State.settings, HOST_OS), HOST_OS,
    );
  },
  /** "Open <app>": the name said, then the app Rust found and its grant. */
  app: null as AppOffer | null,
  /** Dictation: the field the capture started in, if it can take the words. */
  insert: null as InsertOffer | null,
  /** Lock actions through the fresh field check and the one clipboard attempt. */
  copying: false,
  /** A failed check or clipboard attempt left the transcript untouched. */
  copyFailed: false,
  /** A pending Use must not race a Copy check or an insertion. */
  using: false,

  /** Includes native startup and cancellation until microphone release. */
  get busy(): boolean {
    return starting !== null || releasing !== null || this.active;
  },

  get active(): boolean {
    const s = this.event?.state;
    return s === "listening" || s === "processing" || s === "preview";
  },
};

export type AppOffer =
  | { name: string; status: "finding" | "missing" }
  | { name: string; status: "ready" | "opening"; label: string; grant: AppGrant };

export type InsertOffer =
  | { status: "finding" }
  | { status: "retrying"; reason: InsertRefusal }
  /** `reason`: the last Insert was refused before writing; it may be tried again. */
  | { status: "ready"; label: string; grant: AppGrant; reason?: InsertRefusal }
  | { status: "inserting"; label: string; grant: AppGrant }
  | { status: "refused"; reason: InsertRefusal; copyAllowed: boolean };

/** Only these refusals can change while the same remembered field stays valid. */
export function canRetryInsert(reason: InsertRefusal): boolean {
  return reason === "focus-moved" || reason === "busy" || reason === "value-changed";
}

/** Copy requires positive evidence from the current native refusal, never
 * a stale offer, a refusal name, or a still-pending check/write.
 */
export function canCopyInsert(offer: InsertOffer | null): boolean {
  return offer?.status === "refused" && offer.copyAllowed === true
    && offer.reason !== "protected" && offer.reason !== "gone" && offer.reason !== "busy" && offer.reason !== "value-changed";
}

function waitingCard(): IslandViewName | null {
  const req = State.pendingApproval;
  return req ? (req.questions ? "question" : "approval") : null;
}

/** The shortcut: start, or stop the capture that is listening. */
export async function voiceShortcut() {
  if (!host) return;
  const card = waitingCard();
  if (card) {
    if (starting) starting.cancelled = true;
    // An agent is waiting on the user: that card, not the microphone.
    host.alert(card);
    host.emote("annoyed");
    Sound.play("error");
    return;
  }
  if (AudioMode.busy || releasing) return;
  if (starting) {
    starting.stop = true;
    return;
  }
  const current = Voice.event;
  if (current?.state === "listening") {
    void Bridge.voiceStop(current.captureId);
    return;
  }
  if (current?.state === "processing") return;
  generation++;
  end(false);
  const pending = {
    events: new Map<string, VoiceEvent>(), cancelled: false, stop: false,
    view: State.view, mode: State.mode,
  };
  starting = pending;
  const id = await Bridge.voiceToggle();
  if (starting !== pending) return;
  starting = null;
  const event = id ? pending.events.get(id) : undefined;
  if (!id) {
    State.notify();
    return;
  }
  // A waiting agent or Cancel may have taken priority while Rust was opening
  // the microphone. Retire this capture without ever showing its card.
  if (pending.cancelled || waitingCard()) {
    cancelCapture(id, event);
    State.notify();
    return;
  }
  captureId = id;
  // Keep the newest real state, especially an immediate error or idle. Only
  // synthesize listening when Rust has not sent an event for this capture.
  accept(event ?? { v: CONTRACT_VERSION, captureId: id, state: "listening", level: 0, elapsedMs: 0 });
  if (pending.stop && Voice.event?.state === "listening") void Bridge.voiceStop(id);
}

export function voiceStop() {
  if (Voice.event?.state === "listening") void Bridge.voiceStop(Voice.event.captureId);
}

export function voiceCancel() {
  generation++;
  if (starting) starting.cancelled = true;
  const current = Voice.event;
  if (!current) return;
  if (current.state === "error") {
    end();
    return;
  }
  cancelCapture(current.captureId);
  end(!waitingCard());
}

/** Reserve cancellation until native idle/error confirms the microphone closed. */
function cancelCapture(id: string, event?: VoiceEvent) {
  if (event?.state === "idle" || event?.state === "error") return;
  releasing = id;
  void Bridge.voiceCancel(id);
}

/** Use: Rust's policy turns the transcript into a chat draft; the chat opens with it. */
export async function voiceUse() {
  const current = Voice.event;
  // Rust reserves this preview until the write finishes. A concurrent Use
  // would be refused and could otherwise dismiss a still-pending insertion.
  if (current?.state !== "preview" || Voice.copying || Voice.using || Voice.insert?.status === "inserting" || !host) return;
  const requestGeneration = generation;
  Voice.using = true;
  State.notify();
  try {
    const draft = await Bridge.voiceUse(current.captureId);
    if (requestGeneration !== generation) {
      void Bridge.actionReceipt({ actionId: draft.actionId, outcome: "cancelled", errorCode: "cancelled" });
      return;
    }
    end(false);
    const there = host.showChatDraft(draft.text);
    void Bridge.actionReceipt(
      there
        ? succeededReceipt(draft.actionId, "chat-field:draft-set")
        : { actionId: draft.actionId, outcome: "failed", errorCode: "unverified" },
    );
  } catch {
    if (requestGeneration !== generation) return;
    // Expired or refused: nothing was done.
    end();
    Sound.play("error");
  } finally {
    if (requestGeneration === generation) {
      Voice.using = false;
      State.notify();
    }
  }
}

/** Do it: the pill command on the card becomes a `pill-set` Rust decides. */
export async function voiceDo() {
  const current = Voice.event;
  if (current?.state !== "preview" || current.simulated || Voice.copying || Voice.using || !host) return;
  // The preview may have been open while Settings changed. Apply the spoken
  // intent to today's declaration, never the whole set saved in the preview.
  const before = sanitizeDeclared(State.settings, HOST_OS);
  const command = planPills(parseTranscript(current.transcript ?? "", availablePills(HOST_OS)), before, HOST_OS);
  if (!command?.ok) {
    State.notify();
    return;
  }
  const requestGeneration = generation;
  try {
    const actionId = await Bridge.voiceAct(current.captureId, "pill-set", command.next);
    if (generation !== requestGeneration) {
      void Bridge.actionReceipt({ actionId, outcome: "cancelled", errorCode: "cancelled" });
      return;
    }
    const now = sanitizeDeclared(State.settings, HOST_OS);
    if (now.mainPill !== before.mainPill || !sameList(now.activeIntegrations, before.activeIntegrations)) {
      // Rust approved these exact arguments. A newer declaration means they
      // are stale again; leave that change intact and report no effect.
      end(false);
      void Bridge.actionReceipt({ actionId, outcome: "cancelled", errorCode: "cancelled" });
      return;
    }
    end(false);
    const shown = host.applyPills(command.next);
    host.alert("overview");
    Sound.play(shown ? "approve" : "error");
    void Bridge.actionReceipt(
      shown
        ? succeededReceipt(actionId, "settings:pills-shown")
        : { actionId, outcome: "failed", errorCode: "unverified" },
    );
  } catch {
    if (generation !== requestGeneration) return;
    end();
    Sound.play("error");
  }
}

/** Asks Rust for the app the transcript named; the card offers it, or says it isn't there. */
async function findApp(id: string, name: string) {
  const requestGeneration = generation;
  const stale = () => requestGeneration !== generation || Voice.event?.captureId !== id || Voice.app?.name !== name;
  try {
    const found = await Bridge.appPropose(id, name);
    if (stale()) return;
    Voice.app = found ? { name, status: "ready", label: found.label, grant: found.grant } : { name, status: "missing" };
  } catch {
    if (stale()) return;
    Voice.app = { name, status: "missing" };
  }
  State.notify();
}

/** The user's click on "Open <app>": Rust checks the grant and opens it. */
export async function voiceOpen() {
  const current = Voice.event;
  const app = Voice.app;
  if (current?.state !== "preview" || Voice.copying || Voice.using || app?.status !== "ready" || !host) return;
  Voice.app = { ...app, status: "opening" };
  State.notify();
  const requestGeneration = generation;
  try {
    const receipt = await Bridge.appLaunch(current.captureId, app.grant);
    if (requestGeneration !== generation) return;
    end();
    // `unknown`: it may still be opening, or not at all. Never tried again.
    Sound.play(receipt.outcome === "succeeded" ? "approve" : "error");
    if (receipt.outcome !== "succeeded") host.emote("annoyed");
  } catch {
    if (requestGeneration !== generation) return;
    end();
    Sound.play("error");
  }
}

/** Asks Rust whether the field the capture started in can take the transcript. */
async function findField(id: string) {
  const requestGeneration = generation;
  const stale = () => requestGeneration !== generation || Voice.event?.captureId !== id
    || (Voice.insert?.status !== "finding" && Voice.insert?.status !== "retrying");
  try {
    const offer = await Bridge.insertOffer(id);
    if (stale()) return;
    Voice.insert = !offer ? null
      : offer.status === "ready" ? { status: "ready", label: offer.label, grant: offer.grant }
      : { status: "refused", reason: offer.reason, copyAllowed: offer.copyAllowed === true };
  } catch {
    if (stale()) return;
    Voice.insert = null;
  }
  State.notify();
}

/** Retry asks for a fresh offer; only a separate "Insert into <app>" confirms it. */
export async function voiceInsert() {
  const current = Voice.event;
  const offer = Voice.insert;
  if (current?.state !== "preview" || Voice.copying || Voice.using || !host) return;
  if (offer?.status === "refused" && canRetryInsert(offer.reason)) {
    Voice.copyFailed = false;
    Voice.insert = { status: "retrying", reason: offer.reason };
    State.notify();
    // No grant was offered yet. Recheck the same capture without writing or
    // renewing its preview timer; a named offer needs its own confirmation.
    await findField(current.captureId);
    return;
  }
  if (offer?.status !== "ready") return;
  Voice.insert = { status: "inserting", label: offer.label, grant: offer.grant };
  State.notify();
  const requestGeneration = generation;
  try {
    const result = await Bridge.insertText(current.captureId, offer.grant);
    if (requestGeneration !== generation) return;
    if (result.status === "kept") {
      if (Voice.event?.state !== "preview" || Voice.event.captureId !== current.captureId) return;
      // Nothing was written. Only transient refusals retain the named grant;
      // a protected or unusable field gets the same safe fallback as an offer.
      Voice.insert = canRetryInsert(result.reason)
        ? { status: "ready", label: offer.label, grant: offer.grant, reason: result.reason }
        : { status: "refused", reason: result.reason, copyAllowed: result.copyAllowed === true };
      Sound.play("error");
      State.notify();
      return;
    }
    if (result.status === "done") {
      end();
      // `unknown`: it may be there or not. Never written twice.
      Sound.play(result.receipt.outcome === "succeeded" ? "approve" : "error");
      if (result.receipt.outcome !== "succeeded") host.emote("annoyed");
      return;
    }
    end(false);
    Sound.play("error");
    host.emote("annoyed");
    // The words aren't lost: they wait in the chat field, unsent.
    const draft = result.draft;
    if (!draft) {
      host.collapse();
      return;
    }
    const there = host.showChatDraft(draft.text);
    void Bridge.actionReceipt(
      there
        ? succeededReceipt(draft.actionId, "chat-field:draft-set")
        : { actionId: draft.actionId, outcome: "failed", errorCode: "unverified" },
    );
  } catch {
    if (requestGeneration !== generation) return;
    end();
    Sound.play("error");
  }
}

/** Copy: the transcript on the clipboard, for a field Coucou can't write to. */
export async function voiceCopy() {
  const current = Voice.event;
  if (current?.state !== "preview" || current.simulated || Voice.copying || Voice.using || !canCopyInsert(Voice.insert)) return;
  const requestGeneration = generation;
  const owns = () => requestGeneration === generation && Voice.event === current && Voice.copying;
  Voice.copying = true;
  Voice.copyFailed = false;
  State.notify();
  try {
    const result = await Bridge.insertCopyCheck(current.captureId);
    if (!owns()) return;
    if (result?.status !== "allowed") {
      Voice.insert = { status: "refused", reason: result?.status === "refused" ? result.reason : "busy", copyAllowed: false };
      Sound.play("error");
      return;
    }
    // The native check and another process's clipboard cannot be atomic.
    // Use fresh evidence immediately, with no asynchronous fallback attempt.
    await writeClipboard(current.transcript ?? "", { fallback: false });
    if (owns()) Sound.play("approve");
  } catch {
    if (!owns()) return;
    Voice.copyFailed = true;
    Voice.insert = { status: "refused", reason: "busy", copyAllowed: false };
    Sound.play("error");
  } finally {
    if (requestGeneration === generation) {
      Voice.copying = false;
      State.notify();
    }
  }
}

/** Forgets the capture; `close` folds the card away. */
function end(close = true) {
  if (errorTimer) clearTimeout(errorTimer);
  if (previewTimer) clearTimeout(previewTimer);
  errorTimer = null;
  previewTimer = null;
  captureId = null;
  Voice.event = null;
  Voice.app = null;
  Voice.insert = null;
  Voice.copying = false;
  Voice.copyFailed = false;
  Voice.using = false;
  shown = false;
  unpin();
  if (close && State.view === "voice" && host) host.collapse();
  State.notify();
}

/** Gives the island back its own pin, unless an agent's card holds it now. */
function unpin() {
  if (!pinnedByVoice) return;
  pinnedByVoice = false;
  if (!userPinned && !State.pendingApproval) host?.setPinned(false);
}

function accept(event: VoiceEvent) {
  if (event.captureId === releasing) {
    if (event.state === "idle" || event.state === "error") {
      releasing = null;
      State.notify();
    }
    return;
  }
  if (starting) {
    // Release events retire this capture even if a previously queued preview
    // or level reaches the page later, including during cancelled startup.
    const prior = starting.events.get(event.captureId)?.state;
    if (prior !== "idle" && (event.state === "idle" || event.state === "error" || (prior !== "error" && prior !== "preview"))) {
      starting.events.set(event.captureId, event);
    }
    return;
  }
  if (event.captureId !== captureId) return;
  const current = Voice.event;

  if (event.state === "idle") {
    end();
    return;
  }
  const fresh = current?.captureId !== event.captureId;
  Voice.event = event;
  if (fresh) {
    shown = false;
    Sound.play("tick");
    host?.alert("voice");
    if (!pinnedByVoice) {
      userPinned = State.isPinned;
      pinnedByVoice = true;
      host?.setPinned(true);
    }
    host?.takeKeyboard();
  }
  if (event.state !== "preview" && previewTimer) {
    clearTimeout(previewTimer);
    previewTimer = null;
  }
  if (event.state === "preview" && current?.state !== "preview") {
    Sound.play("finish");
    // Only a real transcript can be a command: a simulated one is a placeholder.
    const intents = event.simulated ? [] : parseTranscript(event.transcript ?? "", availablePills(HOST_OS));
    const open = intents.length === 1 && intents[0].kind === "openApp" ? intents[0].name : null;
    Voice.app = open ? { name: open, status: "finding" } : null;
    if (open) void findApp(event.captureId, open);
    // Plain words, not a command: they may go into the field the capture
    // started in.
    Voice.insert = !event.simulated && intents.length === 0 ? { status: "finding" } : null;
    if (Voice.insert) void findField(event.captureId);
    previewTimer = setTimeout(() => {
      if (Voice.event?.captureId === event.captureId && Voice.event.state === "preview") voiceCancel();
    }, PREVIEW_TTL_MS);
  }
  if (event.state === "error") {
    Sound.play("error");
    host?.emote("annoyed");
    if (errorTimer) clearTimeout(errorTimer);
    const id = event.captureId;
    errorTimer = setTimeout(() => {
      if (Voice.event?.captureId === id && Voice.event.state === "error") end();
    }, ERROR_SHOWN_MS);
  }
  State.notify();
}

/** The two island rules, checked on every state change. */
function guard() {
  if (starting && (State.view !== starting.view || State.mode !== starting.mode)) {
    starting.cancelled = true;
  }
  if (waitingCard()) {
    generation++;
    if (starting) starting.cancelled = true;
    if (Voice.active) voiceCancel();
    return;
  }
  const current = Voice.event;
  if (!current || !Voice.active) return;
  const onScreen = State.view === "voice" && State.mode === "expanded";
  if (onScreen) {
    shown = true;
    return;
  }
  // Until the card has been seen once, the island may still be opening.
  if (shown) voiceCancel();
}

export function registerVoice(islandHost: VoiceHost) {
  host = islandHost;
  void onEvent<unknown>(VOICE_EVENT, (raw) => {
    const event = parseVoiceEvent(raw);
    if (event) accept(event);
  });
  State.subscribe(guard);
}

/** Tests only: back to no capture and no island. */
export function resetVoiceForTests() {
  generation++;
  if (errorTimer) clearTimeout(errorTimer);
  if (previewTimer) clearTimeout(previewTimer);
  errorTimer = null;
  previewTimer = null;
  captureId = null;
  Voice.event = null;
  Voice.app = null;
  Voice.insert = null;
  Voice.copying = false;
  Voice.copyFailed = false;
  Voice.using = false;
  shown = false;
  pinnedByVoice = false;
  userPinned = false;
  starting = null;
  releasing = null;
  host = null;
}

/** Tests only: an event as if Rust had sent it. */
export function acceptForTests(raw: unknown) {
  const event = parseVoiceEvent(raw);
  if (event) accept(event);
}

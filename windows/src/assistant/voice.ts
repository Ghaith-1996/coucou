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

import { Bridge, onEvent } from "../core/bridge";
import type { BotEmoteName, IslandViewName } from "../core/layout";
import { Sound } from "../core/sound";
import { HOST_OS, availablePills, sanitizeDeclared } from "../core/pills";
import { State } from "../core/state";
import { VOICE_EVENT, parseVoiceEvent, succeededReceipt, type AppGrant, type VoiceEvent } from "./contracts";
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
  /** What the transcript asks of the pills, when it is a pill command. */
  command: null as PillPlan | null,
  /** "Open <app>": the name said, then the app Rust found and its grant. */
  app: null as AppOffer | null,

  get active(): boolean {
    const s = this.event?.state;
    return s === "listening" || s === "processing" || s === "preview";
  },
};

export type AppOffer =
  | { name: string; status: "finding" | "missing" }
  | { name: string; status: "ready" | "opening"; label: string; grant: AppGrant };

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
  let id: string | null;
  try {
    id = await Bridge.voiceToggle();
  } finally {
    starting = null;
  }
  if (!id) return;
  // A waiting agent or Cancel may have taken priority while Rust was opening
  // the microphone. Retire this capture without ever showing its card.
  if (pending.cancelled || waitingCard()) {
    void Bridge.voiceCancel(id);
    return;
  }
  captureId = id;
  // Keep the newest real state, especially an immediate error or idle. Only
  // synthesize listening when Rust has not sent an event for this capture.
  accept(pending.events.get(id) ?? { v: 1, captureId: id, state: "listening", level: 0, elapsedMs: 0 });
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
  void Bridge.voiceCancel(current.captureId);
  end(!waitingCard());
}

/** Use: Rust's policy turns the transcript into a chat draft; the chat opens with it. */
export async function voiceUse() {
  const current = Voice.event;
  if (current?.state !== "preview" || !host) return;
  const requestGeneration = generation;
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
  }
}

/** Do it: the pill command on the card becomes a `pill-set` Rust decides. */
export async function voiceDo() {
  const current = Voice.event;
  if (current?.state !== "preview" || current.simulated || !host) return;
  // The preview may have been open while Settings changed. Apply the spoken
  // intent to today's declaration, never the whole set saved in the preview.
  const before = sanitizeDeclared(State.settings, HOST_OS);
  const command = planPills(parseTranscript(current.transcript ?? "", availablePills(HOST_OS)), before, HOST_OS);
  Voice.command = command;
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
  if (current?.state !== "preview" || app?.status !== "ready" || !host) return;
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

/** Forgets the capture; `close` folds the card away. */
function end(close = true) {
  if (errorTimer) clearTimeout(errorTimer);
  if (previewTimer) clearTimeout(previewTimer);
  errorTimer = null;
  previewTimer = null;
  captureId = null;
  Voice.event = null;
  Voice.command = null;
  Voice.app = null;
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
  if (starting) {
    starting.events.set(event.captureId, event);
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
    Voice.command = event.simulated || open ? null : planPills(intents, State.settings, HOST_OS);
    Voice.app = open ? { name: open, status: "finding" } : null;
    if (open) void findApp(event.captureId, open);
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
  Voice.command = null;
  Voice.app = null;
  shown = false;
  pinnedByVoice = false;
  userPinned = false;
  starting = null;
  host = null;
}

/** Tests only: an event as if Rust had sent it. */
export function acceptForTests(raw: unknown) {
  const event = parseVoiceEvent(raw);
  if (event) accept(event);
}

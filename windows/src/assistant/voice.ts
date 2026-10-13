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

import { Bridge, onEvent } from "../core/bridge";
import type { BotEmoteName, IslandViewName } from "../core/layout";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import { VOICE_EVENT, parseVoiceEvent, type VoiceEvent } from "./contracts";

/** What voice needs from the island. */
export interface VoiceHost {
  alert(view: IslandViewName): void;
  collapse(): void;
  emote(name: BotEmoteName): void;
  takeKeyboard(): void;
  setPinned(on: boolean): void;
  /** Opens the chat with `text` in its field, unsent. True when it is there. */
  showChatDraft(text: string): boolean;
}

/** How long an error stays on the card before it goes by itself. */
const ERROR_SHOWN_MS = 4000;

let host: VoiceHost | null = null;
/** True once the card was seen on screen for the current capture. */
let shown = false;
let errorTimer: ReturnType<typeof setTimeout> | null = null;
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

  get active(): boolean {
    const s = this.event?.state;
    return s === "listening" || s === "processing" || s === "preview";
  },
};

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
  try {
    const draft = await Bridge.voiceUse(current.captureId);
    end(false);
    const there = host.showChatDraft(draft.text);
    void Bridge.actionReceipt(
      there
        ? { actionId: draft.actionId, outcome: "succeeded", evidenceRef: "chat-field:draft-set" }
        : { actionId: draft.actionId, outcome: "failed", errorCode: "unverified" },
    );
  } catch {
    // Expired or refused: nothing was done.
    end();
    Sound.play("error");
  }
}

/** Forgets the capture; `close` folds the card away. */
function end(close = true) {
  if (errorTimer) clearTimeout(errorTimer);
  errorTimer = null;
  captureId = null;
  Voice.event = null;
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
  if (event.state === "preview" && current?.state !== "preview") Sound.play("finish");
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
  if (errorTimer) clearTimeout(errorTimer);
  errorTimer = null;
  captureId = null;
  Voice.event = null;
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

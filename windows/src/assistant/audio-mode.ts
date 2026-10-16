// Audio mode: a spoken conversation with the chat, hands-free.
//
//   listening → (the user speaks, then pauses) → thinking → speaking → listening…
//
// Listening is a hands-free capture (Rust ends it after speech and a pause,
// or with `silence` when nothing is said, which ends audio mode). What was
// said goes to the chat as the user's message: turning audio mode on is the
// user's choice to talk to the chat, and that is all it ever reaches. No
// command, app, field or agent permission is acted on from here.
//
// The answer is read aloud as it streams in, a sentence or two at a time
// (speakable.ts), by Windows' voices (speech.rs). The microphone is closed
// while Coucou speaks, so it never hears itself; tapping the status while it
// speaks interrupts it and listens again. End, Escape, leaving the chat, an
// agent's permission card or nothing said for a while ends audio mode.

import { Bridge, onEvent } from "../core/bridge";
import { audioRelease, retryCaptureRelease } from "./capture-release";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import { VOICE_EVENT, parseVoiceEvent, type VoiceEvent } from "./contracts";
import { nextChunk, speechChunks } from "./speakable";
import { Voice } from "./voice";

export type AudioState = "off" | "listening" | "transcribing" | "thinking" | "speaking";

/** What audio mode needs from the island. */
export interface AudioHost {
  /** Keeps the island open (and gives its pin back). */
  setPinned(on: boolean): void;
  takeKeyboard(): void;
}

/** The chat: sends `text` as the user's message; resolves with the answer, or null. */
export type Ask = (text: string, requestId: string) => Promise<string | null>;
let askChat: Ask | null = null;
let canAsk = () => true;
let cancelAsk = () => {};

/** The chat view hands audio mode its way of asking. */
export function setAudioAsk(ask: Ask, ready = () => true, cancel = () => {}) {
  askChat = ask;
  canAsk = ready;
  cancelAsk = cancel;
}

/** A clip to play: a WAV data URL. Tests replace it. */
export interface Player {
  play(audio: string): Promise<void>;
  stop(): void;
}

/**
 * The app's content security policy plays media from `blob:` only, never a
 * `data:` URL: a clip from Rust (`data:audio/…;base64,…`) becomes a blob URL,
 * released once the clip is done. Anything else is used as it is.
 */
export function playableUrl(audio: string): { url: string; release: () => void } {
  const match = /^data:([^;,]+);base64,(.*)$/s.exec(audio);
  if (!match) return { url: audio, release: () => {} };
  const binary = atob(match[2]);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  const url = URL.createObjectURL(new Blob([bytes], { type: match[1] }));
  return { url, release: () => URL.revokeObjectURL(url) };
}

class ElementPlayer implements Player {
  private current: HTMLAudioElement | null = null;
  private finish: (() => void) | null = null;

  play(audio: string): Promise<void> {
    this.stop();
    return new Promise((resolve, reject) => {
      const clip = playableUrl(audio);
      const el = new Audio(clip.url);
      this.current = el;
      let settled = false;
      const done = (ok: boolean) => {
        if (settled) return;
        settled = true;
        clip.release();
        if (this.current === el) {
          this.current = null;
          this.finish = null;
        }
        if (ok) resolve();
        else {
          el.pause();
          reject(new Error("Speech playback failed"));
        }
      };
      // Explicit stop is cancellation, not a media failure. Settling once also
      // ignores errors delivered by a stopped element during a newer clip.
      this.finish = () => done(true);
      el.addEventListener("ended", () => done(true));
      el.addEventListener("error", () => done(false));
      el.play().catch(() => done(false));
    });
  }

  stop() {
    this.current?.pause();
    this.current = null;
    const finish = this.finish;
    this.finish = null;
    finish?.();
  }
}

export const AudioMode = {
  state: "off" as AudioState,
  /** UI can be off while a cancelled native capture still owns the microphone. */
  get busy(): boolean {
    return this.state !== "off" || starting !== null || audioRelease.busy;
  },
  /** The microphone level while listening, 0…1. */
  level: 0,
  /** Why audio mode ended by itself, shown once. */
  note: "",
};

let host: AudioHost | null = null;
let player: Player = new ElementPlayer();
let captureId: string | null = null;
/** Native events can arrive before the command reply identifies our capture. */
let starting: { events: Map<string, VoiceEvent> } | null = null;
/** Keep a spoken request reserved until its chat finally block has completed. */
let answering: { promise: Promise<string | null>; cancel: () => void } | null = null;
/** Bumped by every start, stop and interrupt: stale work checks it. */
let turn = 0;
let pinned = false;
let userPinned = false;
/** The streamed reply of the current turn, and how much of it was queued. */
let replyText = "";
/** Correlation only: native binds the actual answering provider to this turn. */
let replyRequestId: string | null = null;
let queuedTo = 0;
let replyDone = false;
let speaking: Promise<void> | null = null;
const queue: string[] = [];

function set(state: AudioState) {
  AudioMode.state = state;
  State.notify();
}

/** The chat's audio button: on, or off when it is on. */
export function toggleAudioMode() {
  if (AudioMode.state === "off") void startAudioMode();
  else stopAudioMode();
}

export async function startAudioMode() {
  if (retryCaptureRelease(true)) return;
  if (AudioMode.busy || !host || !askChat || State.pendingApproval || Voice.busy || answering || !canAsk()) return;
  turn++;
  AudioMode.note = "";
  if (!pinned) {
    userPinned = State.isPinned;
    pinned = true;
    host.setPinned(true);
  }
  host.takeKeyboard();
  Sound.play("tick");
  await listen(turn);
}

export function stopAudioMode(note = "") {
  if (AudioMode.state === "off") { retryCaptureRelease(true); return; }
  turn++;
  const id = captureId;
  captureId = null;
  // Keep a pending start reserved until its reply identifies what to cancel.
  answering?.cancel();
  replyDone = true;
  if (id) cancelCapture(id);
  clearPlayback();
  AudioMode.level = 0;
  AudioMode.note = note;
  if (pinned) {
    pinned = false;
    if (!userPinned && !State.pendingApproval) host?.setPinned(false);
  }
  set("off");
}

/** Retire playback without letting an old synthesis completion own a new turn. */
function endSpeech() {
  const requestId = replyRequestId;
  replyRequestId = null;
  if (requestId) void Bridge.speechEnd(requestId);
}

function clearPlayback() {
  endSpeech();
  queue.length = 0;
  player.stop();
  speaking = null;
}

/** Stop the streamed answer before opening the microphone for the next turn. */
export function interruptAudio() {
  if (AudioMode.state !== "speaking") return;
  const at = ++turn;
  const pending = answering;
  replyDone = true; // Ignore deltas from the interrupted request while it settles.
  clearPlayback();
  if (pending) {
    set("thinking");
    pending.cancel();
    void pending.promise.catch(() => null).then(() => listen(at));
  } else {
    void listen(at);
  }
}

/** Reserve cancellation until native idle/error confirms the microphone closed. */
function cancelCapture(id: string, event?: VoiceEvent) {
  audioRelease.begin(id, event);
}

async function listen(at: number) {
  if (at !== turn) return;
  if (!canAsk() || Voice.busy) {
    stopAudioMode();
    return;
  }
  const pending = { events: new Map<string, VoiceEvent>() };
  starting = pending;
  AudioMode.level = 0;
  set("listening");
  if (at !== turn) {
    // A synchronous subscriber stopped us before any native command was sent.
    if (starting === pending) starting = null;
    State.notify();
    return;
  }
  const id = await Bridge.voiceHandsFree();
  if (starting !== pending) return;
  starting = null;
  const event = id ? pending.events.get(id) : undefined;
  if (at !== turn) {
    if (id) cancelCapture(id, event);
    State.notify();
    return;
  }
  if (!id) {
    stopAudioMode("error");
    return;
  }
  captureId = id;
  if (event) void onVoice(event);
}

async function onVoice(event: VoiceEvent) {
  if (audioRelease.accept(event)) return;
  if (starting) {
    const prior = starting.events.get(event.captureId)?.state;
    // A terminal event cannot be undone by a queued level/processing event.
    if (prior !== "idle" && (event.state === "idle" || event.state === "error" || (prior !== "error" && prior !== "preview"))) {
      starting.events.set(event.captureId, event);
    }
    return;
  }
  if (AudioMode.state === "off" || event.captureId !== captureId) return;
  const at = turn;
  switch (event.state) {
    case "listening":
      AudioMode.level = event.level ?? 0;
      State.notify();
      return;
    case "processing":
      set("transcribing");
      return;
    case "error":
      captureId = null;
      Sound.play("error");
      stopAudioMode(event.error ?? "error");
      return;
    case "idle":
      captureId = null; // This event already acknowledges native release.
      // Cancelled from elsewhere (the screen locked, the app quitting).
      if (AudioMode.state === "listening" || AudioMode.state === "transcribing") stopAudioMode();
      return;
    case "preview": {
      captureId = null;
      if (event.simulated) {
        cancelCapture(event.captureId);
        stopAudioMode("model");
        return;
      }
      let text: string;
      try {
        text = await Bridge.voiceConverse(event.captureId);
      } catch {
        if (at === turn) stopAudioMode("error");
        return;
      }
      if (at !== turn || !askChat) return;
      await converse(at, text);
      return;
    }
  }
}

async function converse(at: number, text: string) {
  set("thinking");
  if (at !== turn) return;
  replyText = "";
  queuedTo = 0;
  replyDone = false;
  queue.length = 0;
  const requestId = crypto.randomUUID();
  replyRequestId = requestId;
  const pending = { promise: askChat!(text, requestId), cancel: cancelAsk };
  answering = pending;
  let answer: string | null;
  try {
    answer = await pending.promise;
  } catch {
    if (at === turn) stopAudioMode("error");
    return;
  } finally {
    if (answering === pending) answering = null;
  }
  if (at !== turn) return;
  if (answer == null) {
    // A failed later round may already have streamed speech. Retire the turn
    // before stopping playback, so a pending synthesis (or its error) cannot
    // cross into the next capture. The chat already explains the failure.
    const next = ++turn;
    replyDone = true;
    clearPlayback();
    await listen(next);
    return;
  }
  replyText = answer;
  replyDone = true;
  enqueue(at);
  await drained(at);
  if (at === turn) {
    endSpeech();
    await listen(at);
  }
}

/** Queues every complete chunk of the reply so far, and starts speaking. */
function enqueue(at: number) {
  for (let c = nextChunk(replyText, queuedTo, replyDone); c; c = nextChunk(replyText, queuedTo, replyDone)) {
    // Both providers truncate oversized input. Queue every bounded piece
    // before advancing the raw-markdown cursor, so no spoken tail is lost.
    queue.push(...speechChunks(c.text));
    queuedTo = c.end;
  }
  if (!speaking && queue.length) {
    const task = speak(at).finally(() => {
      if (speaking === task) speaking = null;
    });
    speaking = task;
  }
}

async function speak(at: number) {
  const requestId = replyRequestId;
  if (!requestId) return;
  // The next clip is asked for while the current one plays.
  let next: Promise<string | null> | null = null;
  const failed = (why: unknown) => {
    if (at !== turn) return;
    // Only the reason (a native error or the player's), never the text.
    void Bridge.log(`audio: speech failed (${why instanceof Error ? why.message : String(why)})`);
    Sound.play("error");
    stopAudioMode("speech");
  };
  const fetch = async (text: string): Promise<string | null> => {
    try {
      const clip = await Bridge.speechSay(text, requestId);
      if (!clip.audio) throw new Error("Speech synthesis returned no audio");
      return clip.audio;
    } catch (why) {
      // Attach the handler when prefetch starts: it can fail before playback
      // finishes, or after this turn is cancelled. Only this turn may end mode.
      failed(why);
      return null;
    }
  };
  try {
    while (at === turn && (queue.length || next)) {
      const audio = await (next ?? fetch(queue.shift()!));
      if (at !== turn || !audio) return;
      next = queue.length ? fetch(queue.shift()!) : null;
      set("speaking");
      if (at !== turn) return; // A state subscriber may synchronously stop us.
      await player.play(audio);
    }
  } catch (why) {
    failed(why);
  }
}

/** Resolves once everything queued for this turn has been spoken. */
async function drained(at: number) {
  while (at === turn && (speaking || queue.length)) {
    if (!speaking && queue.length) enqueue(at);
    await (speaking ?? Promise.resolve());
  }
}

function onDelta(text: string) {
  if (AudioMode.state !== "thinking" && AudioMode.state !== "speaking") return;
  if (!text || replyDone) return;
  replyText = text;
  enqueue(turn);
}

/** Leaving the chat or an agent's card ends audio mode. */
function guard() {
  if (AudioMode.state === "off") return;
  if (State.pendingApproval) {
    stopAudioMode();
    return;
  }
  if (State.view !== "prompt" || State.mode !== "expanded") stopAudioMode();
}

export function registerAudioMode(island: AudioHost) {
  host = island;
  void onEvent<unknown>(VOICE_EVENT, (raw) => {
    const event = parseVoiceEvent(raw);
    if (event) void onVoice(event);
  });
  void onEvent<string>("chat-delta", onDelta);
  State.subscribe(guard);
  // Capture runs before the focused chat field stops keydown propagation.
  if (typeof window.addEventListener === "function") {
    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && AudioMode.state !== "off") {
        e.preventDefault();
        stopAudioMode();
      }
    }, true);
  }
}

/** Tests only. */
export function resetAudioModeForTests(testPlayer?: Player) {
  turn++;
  captureId = null;
  starting = null;
  audioRelease.reset();
  answering = null;
  queue.length = 0;
  speaking = null;
  replyText = "";
  replyRequestId = null;
  queuedTo = 0;
  replyDone = false;
  pinned = false;
  userPinned = false;
  AudioMode.state = "off";
  AudioMode.level = 0;
  AudioMode.note = "";
  if (testPlayer) player = testPlayer;
}

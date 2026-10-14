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
import { Sound } from "../core/sound";
import { State } from "../core/state";
import { VOICE_EVENT, parseVoiceEvent, type VoiceEvent } from "./contracts";
import { nextChunk } from "./speakable";

export type AudioState = "off" | "listening" | "transcribing" | "thinking" | "speaking";

/** What audio mode needs from the island. */
export interface AudioHost {
  /** Keeps the island open (and gives its pin back). */
  setPinned(on: boolean): void;
  takeKeyboard(): void;
}

/** The chat: sends `text` as the user's message; resolves with the answer, or null. */
export type Ask = (text: string) => Promise<string | null>;
let askChat: Ask | null = null;

/** The chat view hands audio mode its way of asking. */
export function setAudioAsk(ask: Ask) {
  askChat = ask;
}

/** A clip to play: a WAV data URL. Tests replace it. */
export interface Player {
  play(audio: string): Promise<void>;
  stop(): void;
}

class ElementPlayer implements Player {
  private current: HTMLAudioElement | null = null;
  private finish: (() => void) | null = null;

  play(audio: string): Promise<void> {
    this.stop();
    return new Promise((resolve) => {
      const el = new Audio(audio);
      this.current = el;
      this.finish = resolve;
      const done = () => {
        if (this.current === el) {
          this.current = null;
          this.finish = null;
        }
        resolve();
      };
      el.addEventListener("ended", done);
      el.addEventListener("error", done);
      el.play().catch(done);
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
  /** The microphone level while listening, 0…1. */
  level: 0,
  /** Why audio mode ended by itself, shown once. */
  note: "",
};

let host: AudioHost | null = null;
let player: Player = new ElementPlayer();
let captureId: string | null = null;
/** Bumped by every start, stop and interrupt: stale work checks it. */
let turn = 0;
let pinned = false;
let userPinned = false;
/** The streamed reply of the current turn, and how much of it was queued. */
let replyText = "";
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
  if (AudioMode.state !== "off" || !host || !askChat || State.pendingApproval) return;
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
  if (AudioMode.state === "off") return;
  turn++;
  const id = captureId;
  captureId = null;
  if (id) void Bridge.voiceCancel(id);
  queue.length = 0;
  player.stop();
  AudioMode.level = 0;
  AudioMode.note = note;
  if (pinned) {
    pinned = false;
    if (!userPinned && !State.pendingApproval) host?.setPinned(false);
  }
  set("off");
}

/** Tapping the status while Coucou speaks: it stops and listens again. */
export function interruptAudio() {
  if (AudioMode.state !== "speaking") return;
  turn++;
  queue.length = 0;
  player.stop();
  void listen(turn);
}

async function listen(at: number) {
  if (at !== turn) return;
  set("listening");
  AudioMode.level = 0;
  const id = await Bridge.voiceHandsFree();
  if (at !== turn) {
    if (id) void Bridge.voiceCancel(id);
    return;
  }
  if (!id) {
    stopAudioMode();
    return;
  }
  captureId = id;
}

async function onVoice(event: VoiceEvent) {
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
      stopAudioMode(event.error === "silence" ? "silence" : "error");
      return;
    case "idle":
      // Cancelled from elsewhere (the screen locked, the app quitting).
      if (AudioMode.state === "listening" || AudioMode.state === "transcribing") stopAudioMode();
      return;
    case "preview": {
      captureId = null;
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
  replyText = "";
  queuedTo = 0;
  replyDone = false;
  queue.length = 0;
  const answer = await askChat!(text);
  if (at !== turn) return;
  if (answer == null) {
    // Stopped or failed: the chat says why; back to listening.
    await listen(at);
    return;
  }
  replyText = answer;
  replyDone = true;
  enqueue(at);
  await drained(at);
  if (at === turn) await listen(at);
}

/** Queues every complete chunk of the reply so far, and starts speaking. */
function enqueue(at: number) {
  for (let c = nextChunk(replyText, queuedTo, replyDone); c; c = nextChunk(replyText, queuedTo, replyDone)) {
    queue.push(c.text);
    queuedTo = c.end;
  }
  if (!speaking && queue.length) speaking = speak(at).finally(() => (speaking = null));
}

async function speak(at: number) {
  // The next clip is asked for while the current one plays.
  let next: Promise<string | null> | null = null;
  const fetch = (text: string) => Bridge.speechSay(text).then((c) => c.audio).catch(() => null);
  while (at === turn && (queue.length || next)) {
    const audio = await (next ?? fetch(queue.shift()!));
    next = queue.length ? fetch(queue.shift()!) : null;
    if (at !== turn) return;
    if (!audio) continue;
    set("speaking");
    await player.play(audio);
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
  // Escape anywhere on the island ends it (the chat field handles its own).
  if (typeof window.addEventListener === "function") {
    window.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && AudioMode.state !== "off") {
        e.preventDefault();
        stopAudioMode();
      }
    });
  }
}

/** Tests only. */
export function resetAudioModeForTests(testPlayer?: Player) {
  turn++;
  captureId = null;
  queue.length = 0;
  speaking = null;
  replyText = "";
  queuedTo = 0;
  replyDone = false;
  pinned = false;
  userPinned = false;
  AudioMode.state = "off";
  AudioMode.level = 0;
  AudioMode.note = "";
  if (testPlayer) player = testPlayer;
}

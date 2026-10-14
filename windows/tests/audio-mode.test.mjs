// Audio mode (src/assistant/audio-mode.ts): listen hands-free, send what was
// said to the chat, read the answer aloud as it streams, listen again; and
// every way it ends.

import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { calls, emit, internals, sent } from "./tauri.mjs";
import { State } from "../src/core/state.ts";
import { CONTRACT_VERSION } from "../src/assistant/contracts.ts";
import {
  AudioMode, interruptAudio, registerAudioMode, resetAudioModeForTests, setAudioAsk, startAudioMode, stopAudioMode,
} from "../src/assistant/audio-mode.ts";

const answers = new Map();
const realInvoke = internals.invoke;
let captures = 0;
internals.invoke = async (cmd, args) => {
  await realInvoke(cmd, args);
  if (cmd === "assistant_voice_hands_free") return `cap-${++captures}`;
  if (cmd === "speech_say") return { audio: `data:audio/wav;base64,${args.text}`, language: "en", matched: true };
  if (!answers.has(cmd)) return null;
  const answer = answers.get(cmd);
  if (answer instanceof Error) throw answer;
  return answer;
};

const played = [];
let playing = null;
const player = {
  play(audio) {
    played.push(audio.slice("data:audio/wav;base64,".length));
    return new Promise((resolve) => (playing = resolve));
  },
  stop() {
    const done = playing;
    playing = null;
    done?.();
  },
};
const finishClip = async () => {
  const done = playing;
  playing = null;
  done?.();
  await flush();
};

const island = { pins: [], setPinned(on) { this.pins.push(on); State.isPinned = on; }, takeKeyboard() {} };
const asked = [];
let reply = null;
setAudioAsk((text) => {
  asked.push(text);
  return new Promise((resolve) => (reply = resolve));
});

const ev = (captureId, state, extra = {}) => ({ v: CONTRACT_VERSION, captureId, state, ...extra });
const flush = async () => {
  for (let i = 0; i < 6; i++) await new Promise((r) => setTimeout(r, 0));
};

let registered = false;
beforeEach(() => {
  resetAudioModeForTests(player);
  if (!registered) {
    registerAudioMode(island);
    registered = true;
  }
  calls.length = 0;
  answers.clear();
  played.length = 0;
  asked.length = 0;
  captures = 0;
  playing = null;
  reply = null;
  island.pins = [];
  State.pendingApproval = null;
  State.isPinned = false;
  State.view = "prompt";
  State.mode = "expanded";
});
afterEach(() => resetAudioModeForTests(player));

async function heard(text, id = "cap-1") {
  answers.set("assistant_voice_converse", text);
  emit("assistant-voice", ev(id, "processing", { elapsedMs: 900 }));
  emit("assistant-voice", ev(id, "preview", { transcript: text }));
  await flush();
}

test("a spoken turn goes to the chat, the answer is read aloud as it streams, then it listens again", async () => {
  await startAudioMode();
  assert.equal(AudioMode.state, "listening");
  assert.deepEqual(island.pins, [true], "the island stays open");
  await heard("What is the capital of Japan?");
  assert.deepEqual(sent("assistant_voice_converse"), [{ captureId: "cap-1" }]);
  assert.deepEqual(asked, ["What is the capital of Japan?"]);
  assert.equal(AudioMode.state, "thinking");

  emit("chat-delta", "The capital of Japan is Tokyo. It has");
  await flush();
  assert.deepEqual(played, ["The capital of Japan is Tokyo."], "the first sentence before the answer is done");
  assert.equal(AudioMode.state, "speaking");

  reply("The capital of Japan is Tokyo. It has about fourteen million people.");
  await finishClip();
  assert.deepEqual(played, ["The capital of Japan is Tokyo.", "It has about fourteen million people."]);
  await finishClip();
  assert.equal(AudioMode.state, "listening", "listening again");
  assert.equal(sent("assistant_voice_hands_free").length, 2);
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0, "never the agents' permissions");
});

test("nothing said ends audio mode and says why", async () => {
  await startAudioMode();
  emit("assistant-voice", ev("cap-1", "error", { error: "silence" }));
  await flush();
  assert.equal(AudioMode.state, "off");
  assert.equal(AudioMode.note, "silence");
  assert.deepEqual(island.pins, [true, false], "the pin is given back");
});

test("an agent's permission card ends audio mode and closes the microphone", async () => {
  await startAudioMode();
  State.pendingApproval = { tool: "Bash" };
  State.notify();
  assert.equal(AudioMode.state, "off");
  assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-1" }]);
  State.pendingApproval = null;
});

test("leaving the chat ends audio mode", async () => {
  await startAudioMode();
  State.view = "overview";
  State.notify();
  assert.equal(AudioMode.state, "off");
});

test("tapping while it speaks interrupts and listens again", async () => {
  await startAudioMode();
  await heard("Tell me a story");
  reply("Once upon a time there was a tiny robot. It lived on a desk and loved tea.");
  await flush();
  assert.equal(AudioMode.state, "speaking");
  interruptAudio();
  await flush();
  assert.equal(AudioMode.state, "listening");
  assert.equal(played.length, 1, "the rest is not read");
  assert.equal(sent("assistant_voice_hands_free").length, 2);
});

test("End stops speaking and listening at once", async () => {
  await startAudioMode();
  await heard("Hi there");
  reply("Hello! How can I help you today with anything at all?");
  await flush();
  stopAudioMode();
  await flush();
  assert.equal(AudioMode.state, "off");
  assert.equal(playing, null, "the voice stopped");
  assert.equal(sent("assistant_voice_hands_free").length, 1, "no new capture");
});

test("a failed or stopped answer goes back to listening", async () => {
  await startAudioMode();
  await heard("Hello");
  reply(null);
  await flush();
  assert.equal(AudioMode.state, "listening");
  assert.deepEqual(played, []);
});

test("a transcript the chat refuses ends audio mode", async () => {
  await startAudioMode();
  answers.set("assistant_voice_converse", new Error("unavailable"));
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "x", simulated: true }));
  await flush();
  assert.equal(AudioMode.state, "off");
  assert.deepEqual(asked, []);
});

test("events of another capture are ignored", async () => {
  await startAudioMode();
  emit("assistant-voice", ev("cap-9", "preview", { transcript: "not mine" }));
  await flush();
  assert.equal(AudioMode.state, "listening");
  assert.deepEqual(asked, []);
});

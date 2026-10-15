// Exercise the real ElementPlayer through the audio controller. Only native IPC
// and HTMLAudioElement are faked; playback errors must not look like `ended`.
import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { emit, internals, calls, sent } from "./tauri.mjs";
import { State } from "../src/core/state.ts";
import { CONTRACT_VERSION } from "../src/assistant/contracts.ts";
import { AudioMode, registerAudioMode, resetAudioModeForTests, setAudioAsk, startAudioMode, stopAudioMode } from "../src/assistant/audio-mode.ts";
const elements = [];
class FakeAudio {
  events = new Map();
  playback = Promise.withResolvers();
  paused = false;
  constructor(audio) { this.audio = audio; elements.push(this); }
  addEventListener(type, listener) { this.events.set(type, listener); }
  play() { return this.playback.promise; }
  pause() { this.paused = true; }
  fire(type) { this.events.get(type)?.(); }
}
globalThis.Audio = FakeAudio;
const nativeInvoke = internals.invoke;
let synthesize;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  if (cmd === "assistant_voice_hands_free") return "capture";
  if (cmd === "assistant_voice_converse") return "spoken question";
  if (cmd === "speech_say") return synthesize(args.text);
  return result;
};
const flush = async () => { for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0)); };
registerAudioMode({ setPinned(on) { State.isPinned = on; }, takeKeyboard() {} });
beforeEach(() => {
  resetAudioModeForTests(); elements.length = 0; calls.length = 0;
  State.pendingApproval = null; State.isPinned = false; State.view = "prompt"; State.mode = "expanded";
  setAudioAsk(async () => "The answer is a single spoken sentence.");
  synthesize = () => ({ audio: "wav" });
});
afterEach(() => { stopAudioMode(); resetAudioModeForTests(); });
async function hear() {
  emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "capture", state: "preview", transcript: "spoken question" });
  await flush();
}
for (const failure of ["play rejection", "media error"]) test(`${failure} ends audio with a speech error instead of reopening capture`, async () => {
  await startAudioMode(); await hear(); assert.equal(elements.length, 1);
  if (failure === "play rejection") elements[0].playback.reject(new Error("playback blocked"));
  else elements[0].fire("error");
  await flush();
  assert.equal(AudioMode.state, "off"); assert.equal(AudioMode.note, "speech");
  assert.equal(sent("assistant_voice_hands_free").length, 1); assert.equal(State.isPinned, false);
});
test("explicit stop and stale media failures do not end a newer clip", async () => {
  await startAudioMode(); await hear(); const old = elements[0];
  stopAudioMode(); assert.equal(old.paused, true); await startAudioMode(); await hear();
  old.fire("error"); old.playback.reject(new Error("old playback failed after stop")); await flush();
  assert.equal(AudioMode.state, "speaking"); assert.equal(AudioMode.note, "");
  assert.equal(elements[1].paused, false); assert.equal(sent("assistant_voice_hands_free").length, 2);
  elements[1].fire("ended"); await flush(); assert.equal(AudioMode.state, "listening");
});
test("a failed playback also safely drains a subsequently rejected prefetch", async (t) => {
  const prefetch = Promise.withResolvers(); t.after(() => prefetch.resolve({ audio: "unused" }));
  let clips = 0; synthesize = () => ++clips === 1 ? { audio: "first" } : prefetch.promise;
  setAudioAsk(async () => "The first sentence cannot play correctly. The second sentence is prefetched meanwhile.");
  await startAudioMode(); await hear(); assert.equal(clips, 2);
  elements[0].fire("error"); await flush();
  assert.equal(AudioMode.state, "off"); assert.equal(AudioMode.note, "speech");
  prefetch.reject(new Error("prefetch failed after playback")); await flush();
  assert.equal(AudioMode.state, "off"); assert.equal(sent("assistant_voice_hands_free").length, 1);
});

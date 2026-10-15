// A native cancellation reply is only a request: the matching terminal event
// arrives after the microphone closes. Exercise both real frontend controllers.
import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { calls, emit, internals, sent } from "./tauri.mjs";
import { State } from "../src/core/state.ts";
import { CONTRACT_VERSION } from "../src/assistant/contracts.ts";
import { AudioMode, registerAudioMode, resetAudioModeForTests, setAudioAsk, startAudioMode, stopAudioMode } from "../src/assistant/audio-mode.ts";
import { Voice, registerVoice, resetVoiceForTests, voiceCancel, voiceShortcut } from "../src/assistant/voice.ts";

const commands = { audio: "assistant_voice_hands_free", voice: "assistant_voice_toggle" };
const starts = { audio: startAudioMode, voice: voiceShortcut };
const stops = { audio: stopAudioMode, voice: voiceCancel };
const busy = { audio: () => AudioMode.busy, voice: () => Voice.busy };
const listeners = new Map();
const nativeInvoke = internals.invoke;
let pending, sequence, cancel;
internals.invoke = async (cmd, args) => {
  if (cmd === "plugin:event|listen") {
    listeners.set(args.event, [...(listeners.get(args.event) ?? []), args.handler]);
    return nativeInvoke(cmd, { ...args, handler: (event) => {
      for (const listener of listeners.get(args.event) ?? []) listener(event);
    } });
  }
  const result = await nativeInvoke(cmd, args);
  if (Object.values(commands).includes(cmd)) return pending?.command === cmd ? pending.promise : `capture-${++sequence}`;
  if (cmd === "assistant_voice_cancel") return cancel(args);
  return result;
};
const flush = async () => { for (let i = 0; i < 4; i++) await new Promise((resolve) => setTimeout(resolve, 0)); };
const event = (captureId, state) => emit("assistant-voice", { v: CONTRACT_VERSION, captureId, state, ...(state === "error" && { error: "provider" }) });
const host = {
  setPinned(on) { State.isPinned = on; }, takeKeyboard() {}, emote() {},
  alert(view) { State.view = view; State.mode = "expanded"; State.notify(); },
  collapse() {}, showChatDraft() { return true; }, applyPills() { return false; },
};
const player = { play: async () => {}, stop() {} };
beforeEach(() => {
  listeners.clear(); pending = null; sequence = 0; cancel = () => false;
  resetVoiceForTests(); resetAudioModeForTests(player);
  registerVoice(host); registerAudioMode(host); setAudioAsk(async () => null);
  State.pendingApproval = null; State.isPinned = false; State.view = "prompt"; State.mode = "expanded";
  calls.length = 0;
});
afterEach(() => { resetVoiceForTests(); resetAudioModeForTests(player); });
const start = (owner) => { State.view = "prompt"; State.mode = "expanded"; return starts[owner](); };
const captureCalls = () => calls.filter(([cmd]) => Object.values(commands).includes(cmd)).length;
const activeId = (owner) => owner === "voice" ? Voice.event?.captureId : AudioMode.state;

for (const owner of ["audio", "voice"]) for (const target of ["audio", "voice"]) for (const phase of ["startup", "listening", "processing"]) {
  test(`${owner} ${phase} cancellation reserves capture ownership before ${target} can start`, async () => {
    if (phase === "startup") pending = { ...Promise.withResolvers(), command: commands[owner] };
    const opening = start(owner);
    if (phase !== "startup") await opening;
    const oldId = phase === "startup" ? "opening-capture" : "capture-1";
    if (phase === "processing") event(oldId, "processing");
    stops[owner](); await flush();
    const attempt = start(target); await flush();
    assert.equal(captureCalls(), 1, "neither mode may invoke another native start");
    await attempt;
    assert.equal(busy[owner](), true, "hiding the UI does not release the native capture");
    if (pending) { const openingCapture = pending; pending = null; openingCapture.resolve(oldId); await opening; }
    await flush();
    assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: oldId }]);
    // The cancellation command has completed, but the capture thread has not.
    for (const state of ["listening", "processing", "preview"]) event(oldId, state);
    event("unrelated", "idle");
    await start(target);
    assert.equal(captureCalls(), 1, "only the matching release event ends the reservation");
    assert.equal(AudioMode.state, "off"); assert.equal(Voice.event, null);
    event(oldId, "idle");
    await start(target);
    assert.equal(captureCalls(), 2, "a fresh capture is available after native release");
    const current = activeId(target);
    for (const state of ["listening", "processing", "preview", "idle", "error"]) event(oldId, state);
    assert.equal(activeId(target), current, "old callbacks cannot change the new owner");
    assert.equal(sent("assistant_voice_cancel").length, 1, "late work cannot cancel the new capture");
  });
}

for (const owner of ["audio", "voice"]) for (const terminal of ["idle", "error"]) {
  test(`cancelled ${owner} startup retains early ${terminal} until its ID is known`, async () => {
    pending = { ...Promise.withResolvers(), command: commands[owner] };
    const opening = start(owner); stops[owner](); await flush();
    // A queued preview must not hide a later release, and a late level cannot
    // undo it. An unrelated terminal event must not release this startup.
    event("opening-capture", "preview"); event("opening-capture", terminal); event("opening-capture", "listening");
    event("unrelated", "idle");
    const attempt = start(owner); await flush();
    assert.equal(captureCalls(), 1, "no new start until the old command identifies its capture");
    await attempt;
    const openingCapture = pending; pending = null; openingCapture.resolve("opening-capture"); await opening;
    assert.equal(busy[owner](), false, "the buffered terminal event already acknowledged release");
    assert.deepEqual(sent("assistant_voice_cancel"), [], "do not wait for another idle from an already released capture");
    await start(owner);
    assert.equal(captureCalls(), 2);
  });
}

for (const owner of ["audio", "voice"]) test(`a cancelled ${owner} startup returning no capture releases its reservation`, async () => {
  pending = { ...Promise.withResolvers(), command: commands[owner] };
  const opening = start(owner); stops[owner](); await flush();
  const openingCapture = pending; pending = null; openingCapture.resolve(null); await opening;
  assert.equal(busy[owner](), false); await start(owner); assert.equal(captureCalls(), 2);
});

test("synchronous audio cancellation before native start leaves no phantom reservation", async () => {
  const unsubscribe = State.subscribe(() => { if (AudioMode.state === "listening") stopAudioMode(); });
  try { await start("audio"); } finally { unsubscribe(); }
  assert.equal(captureCalls(), 0); assert.equal(AudioMode.busy, false);
  await start("voice"); assert.equal(captureCalls(), 1);
});

for (const owner of ["audio", "voice"]) test(`a matching native error releases cancelled ${owner} ownership`, async () => {
  await start(owner); stops[owner](); await flush();
  assert.equal(busy[owner](), true);
  event("capture-1", "error");
  assert.equal(busy[owner](), false);
  await start(owner);
  assert.equal(captureCalls(), 2);
  event("capture-1", "idle");
  assert.equal(busy[owner](), true, "late idle cannot release the newer capture");
});

for (const target of ["audio", "voice"]) test(`dismissing a voice error card allows ${target} to start without another idle`, async () => {
  await start("voice"); event("capture-1", "error"); voiceCancel();
  assert.equal(Voice.busy, false);
  assert.deepEqual(sent("assistant_voice_cancel"), []);
  await start(target); assert.equal(captureCalls(), 2);
});

for (const owner of ["audio", "voice"]) for (const target of ["audio", "voice"]) {
  test(`${owner} retries a failed cancel and releases only after checked acknowledgement for ${target}`, async () => {
    let attempts = 0;
    const acknowledgement = Promise.withResolvers();
    cancel = () => { if (++attempts === 1) throw new Error("lost cancel reply"); return acknowledgement.promise; };
    await start(owner); stops[owner](); await flush();
    assert.equal(attempts, 2, "failed transport is retried with the same capture id");
    assert.equal(busy[owner](), true);
    await start(target); assert.equal(captureCalls(), 1);
    acknowledgement.resolve(true); await flush();
    assert.equal(busy[owner](), false, "checked native release reconciles a missing terminal event");
    await start(target); assert.equal(captureCalls(), 2);
    assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "capture-1" }, { captureId: "capture-1" }]);
  });
}

test("Preview cancellation failure retries even though no later native event is coming", async () => {
  let attempts = 0;
  cancel = () => { if (++attempts === 1) throw new Error("IPC failed before cancellation"); return true; };
  await start("voice");
  emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "capture-1", state: "preview", transcript: "unused preview" });
  voiceCancel(); await flush();
  assert.equal(attempts, 2);
  assert.equal(Voice.busy, false);
  await start("audio"); assert.equal(captureCalls(), 2);
});

for (const owner of ["audio", "voice"]) test(`${owner} retains ownership after bounded failures and a later user attempt can retry`, async () => {
  cancel = () => { throw new Error("native temporarily unavailable"); };
  await start(owner); stops[owner](); await flush();
  assert.equal(sent("assistant_voice_cancel").length, 3, "automatic retries are bounded");
  assert.equal(busy[owner](), true);
  cancel = () => true;
  await start(owner); await flush();
  assert.equal(captureCalls(), 1, "retrying release does not start a capture prematurely");
  assert.equal(busy[owner](), false);
  await start(owner); assert.equal(captureCalls(), 2);
});

for (const owner of ["audio", "voice"]) for (const failure of [false, true]) test(`late ${owner} cancel ${failure ? "failure" : "acknowledgement"} cannot retire a newer release`, async () => {
  const old = Promise.withResolvers();
  cancel = () => old.promise;
  await start(owner); stops[owner](); await flush();
  event("capture-1", "idle");
  await start(owner);
  cancel = () => false;
  stops[owner](); await flush();
  if (failure) old.reject(new Error("late failure")); else old.resolve(true);
  await flush();
  assert.equal(busy[owner](), true);
  assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "capture-1" }, { captureId: "capture-2" }]);
  event("capture-2", "idle");
  assert.equal(busy[owner](), false);
});

test("a null cancel result is not confused with a release acknowledgement", async () => {
  let attempts = 0;
  cancel = () => ++attempts === 1 ? null : true;
  await start("voice"); voiceCancel(); await flush();
  assert.equal(attempts, 2);
  assert.equal(Voice.busy, false);
});

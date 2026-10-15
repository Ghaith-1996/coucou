// Real controllers; only native IPC, playback and DOM are simulated.
import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";
installFakeDom();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { userAgent: "Windows" } });
const keys = [];
window.addEventListener = (type, fn, options) => { if (type === "keydown") keys.push({ fn, capture: options === true || options?.capture }); };
const { buildPrompt, newConversation } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
const { CONTRACT_VERSION } = await import("../src/assistant/contracts.ts");
const { Voice, registerVoice, resetVoiceForTests, voiceShortcut } = await import("../src/assistant/voice.ts");
const { AudioMode, interruptAudio, registerAudioMode, resetAudioModeForTests, startAudioMode, stopAudioMode } = await import("../src/assistant/audio-mode.ts");
const { Bridge } = await import("../src/core/bridge.ts");
let answers, view, unsubscribe, playing;
const played = [];
const nativeInvoke = internals.invoke;
const listeners = new Map();
internals.invoke = async (cmd, args) => {
  // Tauri delivers a stream to both the chat and audio controller. Keep that
  // fan-out here; the basic fixture otherwise keeps only the last listener.
  if (cmd === "plugin:event|listen") {
    listeners.set(args.event, [...(listeners.get(args.event) ?? []), args.handler]);
    return nativeInvoke(cmd, { ...args, handler: (event) => {
      for (const listener of listeners.get(args.event) ?? []) listener(event);
    } });
  }
  const result = await nativeInvoke(cmd, args);
  return cmd in answers ? (typeof answers[cmd] === "function" ? answers[cmd](args) : answers[cmd]) : result;
};
const flush = async () => { for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0)); };
const ev = (state, extra = {}, captureId = "audio-1") => ({ v: CONTRACT_VERSION, captureId, state, ...extra });
const voiceEvent = (...args) => emit("assistant-voice", ev(...args));
const player = { play(audio) { played.push(audio); return new Promise((r) => { playing = r; }); }, stop() { playing?.(); playing = null; } };
const host = { setPinned(on) { State.isPinned = on; }, takeKeyboard() {}, alert(view) { State.view = view; State.mode = "expanded"; State.notify(); }, collapse() {}, emote() {}, showChatDraft() { return true; }, applyPills() { return false; } };
beforeEach(async () => {
  listeners.clear(); keys.length = 0;
  answers = { assistant_voice_hands_free: "audio-1", assistant_voice_toggle: "regular-1", speech_say: { audio: "wav" } };
  resetAudioModeForTests(player); resetVoiceForTests(); registerVoice(host); registerAudioMode(host);
  await Bridge.chatReset(); calls.length = 0;
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.chatHistory = []; State.chatId = null; State.stateOverride = null; State.pendingApproval = null;
  State.view = "prompt"; State.mode = "expanded"; State.isPinned = false; State.droppedFile = null;
  playing = null; played.length = 0; view = buildPrompt(() => {}); unsubscribe = State.subscribe(() => view.sync()); view.sync();
});
afterEach(() => { unsubscribe(); stopAudioMode(); resetAudioModeForTests(player); resetVoiceForTests(); });
const $ = (selector) => view.el.querySelector(selector);
function deferred(t, cmd, fallback = null) {
  const result = Promise.withResolvers(); answers[cmd] = result.promise;
  t.after(async () => { result.resolve(fallback); await flush(); }); return result;
}
function key(key) {
  const e = { key, stopped: false, preventDefault() {}, stopPropagation() { this.stopped = true; } };
  for (const { fn } of keys.filter((x) => x.capture)) fn(e);
  for (const fn of $(".chat-input").listeners.get("keydown") ?? []) fn(e);
  if (!e.stopped) for (const { fn } of keys.filter((x) => !x.capture)) fn(e);
}
async function heard(text = "spoken question") {
  answers.assistant_voice_converse = text; voiceEvent("preview", { transcript: text }); await flush();
}
for (const state of ["error", "idle", "preview"]) test(`native ${state} before capture ID is reconciled`, async (t) => {
  const capture = deferred(t, "assistant_voice_hands_free", "audio-1");
  answers.assistant_voice_converse = "early words"; deferred(t, "chat_send", { text: "answer" });
  const start = startAudioMode(); voiceEvent(state, state === "error" ? { error: "microphone-denied" } : { transcript: "early words" });
  capture.resolve("audio-1"); await start; await flush();
  assert.equal(AudioMode.state, state === "preview" ? "thinking" : "off");
  assert.equal(sent("chat_send").length, state === "preview" ? 1 : 0);
  if (state === "error") assert.ok($(".audio-label").textContent.trim());
});
test("startup terminal event cannot be replaced by another capture or late listening", async (t) => {
  const capture = deferred(t, "assistant_voice_hands_free", "audio-1"); const start = startAudioMode();
  voiceEvent("error", { error: "provider" }, "other"); voiceEvent("idle"); voiceEvent("listening", { level: 0.5 });
  capture.resolve("audio-1"); await start; await flush(); assert.equal(AudioMode.state, "off");
});
for (const phase of ["startup", "listening", "transcribing", "consuming"]) test(`typed send ends ${phase} audio and rejects late speech`, async (t) => {
  const capture = phase === "startup" ? deferred(t, "assistant_voice_hands_free", "audio-1") : null;
  const transcript = phase === "consuming" ? deferred(t, "assistant_voice_converse", "stale") : null;
  deferred(t, "chat_send", { text: "answer" }); const start = startAudioMode(); if (!capture) await start;
  if (phase === "transcribing") voiceEvent("processing");
  if (transcript) { voiceEvent("preview", { transcript: "stale" }); await flush(); }
  $(".chat-input").value = "typed question"; key("Enter");
  assert.equal(AudioMode.state, "off", "manual operation invalidates audio synchronously");
  capture?.resolve("audio-1"); transcript?.resolve("stale"); await start; await flush();
  voiceEvent("preview", { transcript: "stale" }); await flush();
  assert.deepEqual(sent("chat_send").map((x) => x.query), ["typed question"]);
  assert.equal(sent("assistant_voice_hands_free").length, 1);
});
for (const phase of ["starting", "listening", "processing", "preview"]) test(`ordinary voice ${phase} blocks hands-free IPC`, async (t) => {
  const capture = phase === "starting" ? deferred(t, "assistant_voice_toggle", "regular-1") : null;
  const start = voiceShortcut(); if (!capture) await start;
  State.view = "prompt"; if (!capture) Voice.event = ev(phase, {}, "regular-1");
  await startAudioMode(); assert.equal(AudioMode.state, "off"); assert.deepEqual(sent("assistant_voice_hands_free"), []);
  capture?.resolve("regular-1"); await start;
});
test("audio blocks the regular microphone and global voice shortcut", async () => {
  await startAudioMode(); view.sync(); assert.equal($(".voice-btn").disabled, true);
  $(".voice-btn").fire("click"); await voiceShortcut(); await flush();
  assert.deepEqual(sent("assistant_voice_toggle"), []); assert.equal(AudioMode.state, "listening");
});
for (const phase of ["listening", "transcribing", "thinking", "speaking"]) test(`Escape from focused input ends ${phase} audio`, async (t) => {
  const reply = deferred(t, "chat_send", { text: "answer" }); await startAudioMode();
  if (phase === "transcribing") voiceEvent("processing");
  if (phase === "thinking" || phase === "speaking") await heard();
  if (phase === "speaking") { emit("chat-delta", "Here is the first sentence. "); await flush(); }
  assert.equal(AudioMode.state, phase); key("Escape"); assert.equal(AudioMode.state, "off");
  reply.resolve({ text: "late answer" }); await flush(); assert.equal(sent("assistant_voice_hands_free").length, 1);
});
test("streaming interruption waits for chat cancellation before taking the next spoken turn", async (t) => {
  const first = deferred(t, "chat_send", { text: "old answer" }); await startAudioMode(); await heard("first question");
  emit("chat-delta", "Here is the first sentence. "); await flush(); assert.equal(AudioMode.state, "speaking");
  interruptAudio(); await flush(); assert.equal(sent("chat_stop").length, 1);
  assert.equal(sent("assistant_voice_hands_free").length, 1, "wait for old sending flag");
  emit("chat-delta", "An obsolete streamed sentence. "); first.reject(new Error("stopped")); await flush();
  assert.equal(AudioMode.state, "listening"); answers.chat_send = { text: "new answer" }; await heard("second question");
  assert.deepEqual(sent("chat_send").map((x) => x.query), ["first question", "second question"]);
});
test("stop during interruption prevents microphone reopening after a late reply", async (t) => {
  const first = deferred(t, "chat_send", { text: "old answer" }); await startAudioMode(); await heard();
  emit("chat-delta", "Here is the first sentence. "); await flush(); interruptAudio(); stopAudioMode();
  first.reject(new Error("stopped")); await flush(); assert.equal(AudioMode.state, "off");
  assert.equal(sent("assistant_voice_hands_free").length, 1);
});
test("new conversation invalidates a listening audio turn", async () => {
  await startAudioMode(); newConversation(); assert.equal(AudioMode.state, "off"); await heard("stale"); assert.deepEqual(sent("chat_send"), []);
});
for (const kind of ["provider", "simulated", "startup"]) test(`${kind} failure shows an explanation`, async () => {
  if (kind === "startup") answers.assistant_voice_hands_free = null; await startAudioMode();
  if (kind === "provider") voiceEvent("error", { error: "provider" });
  if (kind === "simulated") { answers.assistant_voice_converse = () => { throw new Error("unavailable"); }; voiceEvent("preview", { transcript: "demo", simulated: true }); }
  await flush(); view.sync(); assert.equal(AudioMode.state, "off"); assert.equal($(".audio-row").hidden, false);
  assert.ok($(".audio-label").textContent.trim());
  if (kind === "simulated") assert.match($(".audio-label").textContent, /model.*Settings|Settings.*model/);
});

test("a rejected native startup ends audio with a visible error", async (t) => {
  t.mock.method(console, "error", () => {});
  answers.assistant_voice_hands_free = () => { throw new Error("permission denied"); };
  await startAudioMode(); view.sync();
  assert.equal(AudioMode.state, "off");
  assert.equal(State.isPinned, false);
  assert.ok($(".audio-label").textContent.trim());
});

test("rapid stop/start waits for the prior chat request to release its reservation", async (t) => {
  const reply = deferred(t, "chat_send", { text: "old answer" });
  await startAudioMode(); await heard(); stopAudioMode();
  await startAudioMode();
  assert.equal(AudioMode.state, "off");
  assert.equal(sent("assistant_voice_hands_free").length, 1);
  reply.reject(new Error("stopped")); await flush();
  await startAudioMode();
  assert.equal(AudioMode.state, "listening");
  assert.equal(sent("assistant_voice_hands_free").length, 2);
});

test("late synthesis cannot play or release the next turn's playback", async (t) => {
  const synthesis = deferred(t, "speech_say", { audio: "old clip" });
  answers.chat_send = { text: "An older answer that finishes synthesis late." };
  await startAudioMode(); await heard("old question");
  assert.equal(AudioMode.state, "thinking");
  const oldId = sent("chat_send")[0].requestId;
  stopAudioMode(); await startAudioMode();
  assert.deepEqual(sent("speech_end"), [{ requestId: oldId }]);
  answers.speech_say = { audio: "new clip" };
  answers.chat_send = { text: "The answer for the new spoken question." };
  await heard("new question");
  const newId = sent("chat_send")[1].requestId;
  assert.notEqual(newId, oldId);
  assert.equal(AudioMode.state, "speaking");
  synthesis.resolve({ audio: "old clip" }); await flush();
  assert.deepEqual(sent("speech_end"), [{ requestId: oldId }], "late completion cannot retire the current speech binding");
  assert.deepEqual(played, ["new clip"]);
  assert.equal(sent("assistant_voice_hands_free").length, 2, "current playback still owns the turn");
  player.stop(); await flush();
  assert.equal(sent("assistant_voice_hands_free").length, 3);
  assert.deepEqual(sent("speech_end"), [{ requestId: oldId }, { requestId: newId }]);
});

function speechEnded() {
  view.sync();
  assert.equal(AudioMode.state, "off");
  assert.equal(AudioMode.note, "speech");
  assert.equal(State.isPinned, false);
  assert.equal($(".audio-row").hidden, false);
  assert.match($(".audio-label").textContent, /read.*aloud/i);
  assert.equal(sent("assistant_voice_hands_free").length, 1, "failed speech cannot reopen the microphone");
}
for (const kind of ["rejected", "empty"]) test(`${kind} synthesis ends audio with a visible speech error`, async () => {
  answers.chat_send = { text: "This answer could not be read aloud." };
  answers.speech_say = () => { if (kind === "rejected") throw new Error("remote and Windows voices unavailable"); return { audio: "" }; };
  await startAudioMode(); await heard();
  speechEnded(); assert.deepEqual(played, []);
});

test("a failed prefetched chunk ends a partially spoken answer instead of skipping it", async () => {
  answers.chat_send = { text: "The first sentence is spoken correctly. The second sentence fails to synthesize. The third sentence must not be requested." };
  let clips = 0;
  answers.speech_say = () => { if (++clips === 1) return { audio: "first clip" }; throw new Error("no voice available"); };
  await startAudioMode(); await heard();
  assert.deepEqual(played, ["first clip"]);
  speechEnded(); // Prefetch failure must also stop the still-playing first clip.
  assert.equal(playing, null);
  assert.equal(clips, 2); assert.deepEqual(played, ["first clip"]);
});

test("failed streamed speech cancels the pending answer and ignores its late result", async (t) => {
  const reply = deferred(t, "chat_send", { text: "late answer" });
  answers.speech_say = () => { throw new Error("no voice available"); };
  await startAudioMode(); await heard();
  emit("chat-delta", "This streamed sentence fails to synthesize. "); await flush();
  speechEnded(); assert.equal(sent("chat_stop").length, 1);
  emit("chat-delta", "The stale request keeps producing text. ");
  reply.resolve({ text: "This stale final answer cannot restart audio." }); await flush();
  speechEnded(); assert.equal(sent("speech_say").length, 1);
});

test("late synthesis rejection cannot end or overwrite a newer spoken turn", async (t) => {
  const old = deferred(t, "speech_say", { audio: "old clip" });
  answers.chat_send = { text: "This older answer is still synthesizing." };
  await startAudioMode(); await heard(); stopAudioMode(); await startAudioMode();
  answers.speech_say = { audio: "new clip" };
  await heard("new question");
  old.reject(new Error("old voice failed")); await flush();
  assert.equal(AudioMode.state, "speaking"); assert.equal(AudioMode.note, "");
  assert.deepEqual(played, ["new clip"]); assert.equal(sent("assistant_voice_hands_free").length, 2);
  player.stop(); await flush(); assert.equal(AudioMode.state, "listening");
});

test("late rejected prefetch after interruption cannot end the next listening turn", async (t) => {
  const old = Promise.withResolvers(); t.after(() => old.resolve({ audio: "unused" }));
  answers.chat_send = { text: "The first sentence is spoken correctly. The next sentence is still being synthesized." };
  let clips = 0; answers.speech_say = () => ++clips === 1 ? { audio: "first clip" } : old.promise;
  await startAudioMode(); await heard(); interruptAudio(); await flush();
  assert.equal(AudioMode.state, "listening");
  old.reject(new Error("obsolete prefetched clip failed")); await flush();
  assert.equal(AudioMode.state, "listening"); assert.equal(AudioMode.note, "");
  assert.equal(sent("assistant_voice_hands_free").length, 2);
});

async function limitAnswer(reply) {
  emit("chatgpt-usage-limit", null);
  reply.reject(new Error("usage limit"));
  await flush();
}
test("a failed streamed answer stops playback before opening the next microphone", async (t) => {
  State.settings.chatProvider = "chatgpt";
  const reply = deferred(t, "chat_send", { text: "unused" });
  answers.assistant_voice_hands_free = () => {
    assert.equal(playing, null, "capture cannot begin while the answer is still playing");
    return "audio-1";
  };
  await startAudioMode(); await heard();
  emit("chat-delta", "This partial answer has started playing. There is also another queued sentence. "); await flush();
  assert.equal(AudioMode.state, "speaking"); assert.ok(playing);
  await limitAnswer(reply);
  assert.equal(playing, null); assert.equal(AudioMode.state, "listening");
  assert.equal(sent("assistant_voice_hands_free").length, 2);
  assert.deepEqual(played, ["wav"]);
});

for (const phase of ["first clip", "prefetch"]) for (const outcome of ["resolved", "rejected"]) test(`a ${outcome} old ${phase} cannot cross a failed answer into the next turn`, async (t) => {
  State.settings.chatProvider = "chatgpt";
  const reply = deferred(t, "chat_send", { text: "unused" });
  const clip = Promise.withResolvers(); t.after(() => clip.resolve({ audio: "old clip" }));
  let requests = 0;
  answers.speech_say = () => phase === "prefetch" && ++requests === 1 ? { audio: "first clip" } : clip.promise;
  await startAudioMode(); await heard("old question");
  emit("chat-delta", "The initial sentence is already complete. Another old sentence is still queued up. A final old sentence is still waiting. "); await flush();
  await limitAnswer(reply);
  assert.equal(AudioMode.state, "listening");
  answers.speech_say = { audio: "new clip" };
  answers.chat_send = { text: "This is the answer to the newer question." };
  await heard("new question");
  if (outcome === "resolved") clip.resolve({ audio: "obsolete clip" });
  else clip.reject(new Error("obsolete synthesis failed"));
  await flush();
  assert.equal(AudioMode.state, "speaking"); assert.equal(AudioMode.note, "");
  assert.deepEqual(played, phase === "prefetch" ? ["first clip", "new clip"] : ["new clip"]);
  assert.equal(sent("speech_say").length, phase === "prefetch" ? 3 : 2, "old queued chunks are discarded");
  assert.equal(sent("assistant_voice_hands_free").length, 2);
  player.stop(); await flush(); assert.equal(AudioMode.state, "listening");
});

test("a failed answer that leaves the chat cancels partial playback and late prefetch", async (t) => {
  const reply = deferred(t, "chat_send", { text: "unused" });
  const clip = Promise.withResolvers(); t.after(() => clip.resolve({ audio: "unused" }));
  let clips = 0; answers.speech_say = () => ++clips === 1 ? { audio: "first clip" } : clip.promise;
  await startAudioMode(); await heard();
  emit("chat-delta", "The first sentence is spoken correctly. Another sentence is prefetched meanwhile. "); await flush();
  reply.reject(new Error("provider failed")); await flush();
  assert.equal(State.view, "note"); assert.equal(AudioMode.state, "off"); assert.equal(playing, null);
  clip.resolve({ audio: "late clip" }); await flush();
  assert.deepEqual(played, ["first clip"]); assert.equal(sent("assistant_voice_hands_free").length, 1);
});

test("synchronous cancellation while entering speaking cannot play over a restarted capture", async (t) => {
  answers.chat_send = { text: "This answer is cancelled before the player starts." };
  const unsubscribe = State.subscribe(() => {
    if (AudioMode.state !== "speaking") return;
    stopAudioMode(); void startAudioMode();
  });
  t.after(unsubscribe);
  await startAudioMode(); await heard();
  assert.equal(AudioMode.state, "listening");
  assert.equal(sent("assistant_voice_hands_free").length, 2);
  assert.deepEqual(played, [], "not even one obsolete clip may start after cancellation");
});

for (const provider of ["ollama", "chatgpt"]) test(`speech stays bound to the ${provider} answer after the provider picker changes`, async () => {
  const bound = new Map();
  State.settings.chatProvider = provider;
  const text = "The first sentence is long enough to speak. The second sentence also deserves its own clip. The final sentence must use the same voice provider.";
  answers.chat_send = args => { if (args.requestId) bound.set(args.requestId, State.settings.chatProvider); return { text }; };
  answers.speech_say = args => {
    // Existing native routing reads mutable settings. A matching native turn
    // snapshot must replace that route for every subsequent prefetched clip.
    const source = args.requestId && bound.has(args.requestId) ? bound.get(args.requestId) : State.settings.chatProvider;
    return { audio: `${source}:${args.text}` };
  };
  answers.speech_end = ({ requestId }) => { bound.delete(requestId); };
  await startAudioMode(); await heard("Please read the whole answer");
  assert.equal(played.length, 1);
  State.settings.chatProvider = provider === "ollama" ? "chatgpt" : "ollama";
  State.notify();
  for (let i = 0; i < 5 && AudioMode.state === "speaking"; i++) { player.stop(); await flush(); }
  assert.deepEqual(played.map(clip => clip.split(":")[0]), [provider, provider, provider]);
  const requestId = sent("chat_send")[0].requestId;
  assert.equal(typeof requestId, "string");
  assert.ok(requestId.length > 0);
  assert.deepEqual(sent("speech_say").map(args => args.requestId), [requestId, requestId, requestId]);
  assert.deepEqual(sent("speech_end"), [{ requestId }], "the completed speech authorization ends after every clip drains");
  assert.equal(bound.size, 0);
  assert.equal(AudioMode.state, "listening");
});

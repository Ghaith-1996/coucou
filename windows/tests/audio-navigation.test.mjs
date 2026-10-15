// Production shortcut/file-drop reset paths with real audio ownership. Only
// native IPC, playback, and the island's drawing/animation shell are faked.
import { beforeEach, afterEach, test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import ts from "typescript";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";
installFakeDom();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { userAgent: "Windows" } });
registerHooks({ load(url, context, nextLoad) {
  if (url.startsWith("file:") && url.endsWith(".ts") && !url.includes("/node_modules/")) {
    return { format: "module", shortCircuit: true, source: ts.transpileModule(readFileSync(new URL(url), "utf8"), {
      compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
    }).outputText };
  }
  return nextLoad(url, context);
} });
const { AudioMode, registerAudioMode, resetAudioModeForTests, setAudioAsk, startAudioMode, stopAudioMode } = await import("../src/assistant/audio-mode.ts");
const { resetVoiceForTests } = await import("../src/assistant/voice.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
const { CONTRACT_VERSION } = await import("../src/assistant/contracts.ts");
const { Bridge } = await import("../src/core/bridge.ts");
const { runIslandKey } = await import("../src/island/shortcuts.ts");
const { buildPrompt } = await import("../src/views/chat.ts");
const { Island } = await import("../src/island/island.ts");
const { UploadSeq } = await import("../src/upload/sequence.ts");
const nativeInvoke = internals.invoke;
let answers = {}, asked, playing;
const played = [];
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  if (cmd in answers) return typeof answers[cmd] === "function" ? answers[cmd](args) : answers[cmd];
  return result;
};
const player = { play(audio) { played.push(audio); return new Promise(resolve => { playing = resolve; }); }, stop() { playing?.(); playing = null; } };
const host = { setPinned(on) { State.isPinned = on; }, takeKeyboard() {}, setView(view) { State.view = view; State.notify(); } };
registerAudioMode(host);
const flush = async () => { for (let i = 0; i < 4; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
const voice = (state, extra = {}) => emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "capture", state, ...extra });
beforeEach(async () => {
  answers = { assistant_voice_hands_free: "capture", assistant_voice_converse: "old question", speech_say: { audio: "old audio" }, ingest_file: { name: "new.txt", path: "/inbox/new.txt" } };
  resetAudioModeForTests(player); resetVoiceForTests(); await Bridge.chatReset(); calls.length = 0;
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} }; State.paused = false;
  State.chatId = "c-0000000000000001"; State.chatHistory = [{ id: 1, role: "user", content: "Old conversation" }];
  State.droppedFile = null; State.promptContext = null; State.pendingApproval = null; State.stateOverride = null;
  State.view = "prompt"; State.mode = "expanded"; State.isPinned = false;
  asked = []; played.length = 0; playing = null;
  setAudioAsk(async text => { asked.push(text); return "This is the older spoken answer."; });
});
afterEach(() => { stopAudioMode(); resetAudioModeForTests(player); resetVoiceForTests(); UploadSeq.deactivate(); });
for (const navigation of ["keyboard new chat", "file drop"]) for (const phase of ["startup", "listening", "transcribing", "consuming", "speaking"]) {
  test(`${navigation} retires ${phase} audio before the new conversation can receive it`, async (t) => {
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(phase === "startup" ? "capture" : "late old transcript"); await flush(); });
    if (phase === "startup") answers.assistant_voice_hands_free = pending.promise;
    if (phase === "consuming") answers.assistant_voice_converse = pending.promise;
    const started = startAudioMode(); if (phase !== "startup") await started;
    if (phase === "transcribing") voice("processing");
    if (phase === "consuming" || phase === "speaking") { voice("preview", { transcript: "old question" }); await flush(); }
    if (phase === "speaking") assert.ok(playing);
    if (navigation === "keyboard new chat") runIslandKey(host, { kind: "newChat" });
    else {
      const island = Object.create(Island.prototype);
      island.engine = { gulp() {}, triggerEmote() {}, animateMorph() {} };
      island.ensureRunning = () => {}; island.setView = host.setView;
      island.onDragDrop({ type: "drop", paths: ["/source/new.txt"] });
    }
    assert.equal(AudioMode.state, "off"); assert.equal(playing, null);
    assert.deepEqual(State.chatHistory, []); assert.equal(State.chatId, null);
    pending.resolve(phase === "startup" ? "capture" : "late old transcript"); await started; await flush();
    voice("preview", { transcript: "late old transcript" }); await flush();
    assert.deepEqual(asked, phase === "speaking" ? ["old question"] : []);
    assert.equal(sent("assistant_voice_hands_free").length, 1);
    assert.equal(sent("chat_reset").length, 1);
    assert.equal(AudioMode.state, "off");
  });
}

test("keyboard new chat still refuses to reset an answer that is actively thinking", async () => {
  await startAudioMode(); State.stateOverride = "thinking";
  const messages = State.chatHistory;
  runIslandKey(host, { kind: "newChat" }); await flush();
  assert.equal(State.chatHistory, messages); assert.equal(AudioMode.state, "listening");
  assert.deepEqual(sent("chat_reset"), []);
});


test("the composer keeps both microphone buttons disabled until cancelled audio releases capture", async () => {
  const view = buildPrompt(() => {});
  await startAudioMode(); stopAudioMode(); view.sync();
  assert.equal(AudioMode.state, "off");
  assert.equal(view.el.querySelector(".voice-btn").disabled, true);
  assert.equal(view.el.querySelector(".audio-btn").disabled, true);
  view.el.querySelector(".voice-btn").fire("click");
  view.el.querySelector(".audio-btn").fire("click");
  await flush(); assert.equal(sent("assistant_voice_hands_free").length, 1);
  assert.deepEqual(sent("assistant_voice_toggle"), []);
  voice("idle"); await flush(); view.sync();
  assert.equal(view.el.querySelector(".voice-btn").disabled, false);
  assert.equal(view.el.querySelector(".audio-btn").disabled, false);
});

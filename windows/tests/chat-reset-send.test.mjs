// New conversation (also used by file drop) cancels whichever kind of send owns
// the old transcript. Exercise the real chat/audio controllers and bridge.
import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";
installFakeDom();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { userAgent: "Windows" } });
const { State, DEFAULT_SETTINGS } = await import("../src/core/state.ts");
const { Bridge } = await import("../src/core/bridge.ts");
const { buildPrompt, newConversation } = await import("../src/views/chat.ts");
const { AudioMode, startAudioMode, stopAudioMode, registerAudioMode, resetAudioModeForTests } = await import("../src/assistant/audio-mode.ts");
const { CONTRACT_VERSION } = await import("../src/assistant/contracts.ts");
let pending, view, unsubscribe, stopSawOldConversation;
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  if (cmd === "chat_stop") stopSawOldConversation = sent("chat_reset").length === 0;
  const result = await nativeInvoke(cmd, args);
  if (cmd === "assistant_voice_hands_free") return "capture";
  if (cmd === "assistant_voice_converse") return "Old spoken question";
  if (cmd === "chat_send") return pending ? pending.promise : { text: "New reply" };
  if (cmd === "chat_stop") {
    pending?.reject(new Error("Stopped"));
  }
  return result;
};
const flush = async () => { for (let i = 0; i < 5; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
const player = { play: async () => {}, stop() {} };
beforeEach(async () => {
  await Bridge.chatReset(); calls.length = 0;
  pending = Promise.withResolvers(); stopSawOldConversation = false;
  resetAudioModeForTests(player);
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.chatHistory = []; State.chatId = null; State.droppedFile = null; State.promptContext = null;
  State.pendingApproval = null; State.stateOverride = null; State.view = "prompt"; State.mode = "expanded";
  view = buildPrompt(() => {}); unsubscribe = State.subscribe(() => view.sync()); view.sync();
  registerAudioMode({ setPinned(on) { State.isPinned = on; }, takeKeyboard() {} });
});
afterEach(async () => { pending?.resolve({ text: "cleanup" }); await flush(); unsubscribe(); stopAudioMode(); resetAudioModeForTests(player); });
function type(text) {
  const input = view.el.querySelector(".chat-input"); input.value = text;
  for (const listener of input.listeners.get("keydown") ?? []) listener({ key: "Enter", preventDefault() {}, stopPropagation() {} });
}
for (const origin of ["typed", "audio"]) test(`reset cancels the active ${origin} send before clearing native history`, async () => {
  if (origin === "typed") type("Old typed question");
  else {
    await startAudioMode();
    emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "capture", state: "preview", transcript: "Old spoken question" });
  }
  await flush(); assert.equal(sent("chat_send").length, 1);
  newConversation(); await flush();
  assert.equal(sent("chat_stop").length, 1, "typed and audio sends use the same idempotent cancellation");
  assert.equal(stopSawOldConversation, true, "cancellation is dispatched before native reset");
  assert.equal(sent("chat_reset").length, 1);
  assert.equal(AudioMode.state, "off"); assert.deepEqual(State.chatHistory, []);
  assert.equal(State.stateOverride, null, "the cancelled request unwinds without blocking the new chat");
  pending = null;
  type("New question"); await flush();
  assert.deepEqual(State.chatHistory.map(message => message.content), ["New question", "New reply"]);
  assert.equal(sent("chat_send").length, 2);
});
test("reset without an active answer does not send an unrelated stop", async () => {
  newConversation(); await flush();
  assert.deepEqual(sent("chat_stop"), []);
});

for (const outcome of ["success", "failure"]) {
  test(`a delayed cancelled ${outcome} cannot clear a rebuilt view's current answer`, async (t) => {
    const oldAnswer = pending;
    const nextAnswer = Promise.withResolvers();
    t.after(async () => { oldAnswer.resolve({ text: "cleanup" }); await flush(); });
    const invoke = internals.invoke;
    t.mock.method(internals, "invoke", async (cmd, args) => {
      // Native cancellation is acknowledged before its old Send reply arrives.
      if (cmd === "chat_stop") return nativeInvoke(cmd, args);
      return invoke(cmd, args);
    });
    type("Old question"); await flush();
    assert.equal(sent("chat_send").length, 1);
    newConversation(); await flush();
    assert.equal(sent("chat_stop").length, 1);
    unsubscribe();
    view = buildPrompt(() => {});
    unsubscribe = State.subscribe(() => view.sync());
    pending = nextAnswer;
    type("New question"); await flush();
    assert.equal(sent("chat_send").length, 2);
    assert.equal(State.stateOverride, "thinking");
    if (outcome === "success") oldAnswer.resolve({ text: "Discarded old answer" });
    else oldAnswer.reject(new Error("Discarded old failure"));
    await flush();
    assert.deepEqual(State.chatHistory.map(message => message.content), ["New question"]);
    assert.equal(State.view, "prompt", "an old rejection cannot show an error over the new view");
    assert.equal(State.stateOverride, "thinking", "an old finally cannot clear another request's thinking state");
    nextAnswer.resolve({ text: "New reply" }); await flush();
    assert.deepEqual(State.chatHistory.map(message => message.content), ["New question", "New reply"]);
    assert.equal(State.stateOverride, null);
  });
}

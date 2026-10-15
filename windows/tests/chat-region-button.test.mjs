// The actual composer and native boundary, with deferred replies to reproduce races.
import { beforeEach, afterEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, internals, sent } from "./tauri.mjs";
installFakeDom();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { userAgent: "Windows" } });
const { buildPrompt } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
const { Region, registerRegion, resetRegionForTests, regionCancel } = await import("../src/assistant/region.ts");
const { registerVoice, resetVoiceForTests } = await import("../src/assistant/voice.ts");
const answers = new Map();
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  return answers.has(cmd) ? answers.get(cmd) : result;
};
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const host = {
  alert(view) { State.view = view; State.mode = "expanded"; State.notify(); },
  collapse() { State.mode = "compact"; State.notify(); },
  takeKeyboard() {}, emote() {}, applyPills() { return false; }, showChatDraft() { return false; },
  setPinned(on) { State.isPinned = on; State.notify(); }, chatWithFile() {},
};
let view;
let unsubscribe;
beforeEach(() => {
  resetRegionForTests(); resetVoiceForTests();
  const subscribe = State.subscribe;
  State.subscribe = () => () => {};
  registerRegion(host); registerVoice(host);
  State.subscribe = subscribe;
  calls.length = 0;
  answers.clear();
  answers.set("region_start", "reg-1");
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.chatHistory = []; State.chatId = null;
  State.stateOverride = null; State.pendingApproval = null;
  State.view = "prompt"; State.mode = "expanded"; State.isPinned = false;
  State.droppedFile = null; State.promptContext = null;
  view = buildPrompt(() => {});
  unsubscribe = State.subscribe(() => view.sync());
  view.sync();
});
afterEach(() => { unsubscribe(); resetRegionForTests(); resetVoiceForTests(); });
const $ = (selector) => view.el.querySelector(selector);
function deferred(t, cmd, fallback = null) {
  const pending = Promise.withResolvers();
  answers.set(cmd, pending.promise);
  t.after(async () => { pending.resolve(fallback); await flush(); });
  return pending;
}
test("pending chat replies disable region capture and guard retained clicks", async (t) => {
  const reply = deferred(t, "chat_send", { text: "Hello" });
  $(".chat-input").value = "hello";
  $(".send-btn").fire("click");
  $(".region-btn").fire("click");
  await flush();
  assert.equal($(".region-btn").disabled, true);
  assert.deepEqual(sent("region_start"), []);
  reply.resolve({ text: "Hello" });
  await flush();
  assert.equal($(".region-btn").disabled, false);
  $(".region-btn").fire("click");
  await flush();
  assert.equal(sent("region_start").length, 1);
});
test("microphone startup reserves the composer against region capture", async (t) => {
  const voice = deferred(t, "assistant_voice_toggle");
  $(".voice-btn").fire("click");
  $(".region-btn").fire("click");
  await flush();
  assert.equal($(".region-btn").disabled, true);
  assert.deepEqual(sent("region_start"), []);
  voice.resolve(null);
  await flush();
  assert.equal($(".region-btn").disabled, false);
});
test("region startup blocks Send and microphone clicks until capture is canceled", async (t) => {
  const region = deferred(t, "region_start", "reg-1");
  $(".chat-input").value = "keep this draft";
  $(".region-btn").fire("click");
  $(".send-btn").fire("click");
  $(".voice-btn").fire("click");
  $(".region-btn").fire("click");
  await flush();
  assert.equal(sent("region_start").length, 1);
  assert.deepEqual(sent("chat_send"), []);
  assert.deepEqual(sent("assistant_voice_toggle"), []);
  assert.equal($(".send-btn").disabled, true);
  assert.equal($(".voice-btn").disabled, true);
  assert.equal($(".region-btn").disabled, true);
  assert.equal($(".chat-input").value, "keep this draft");
  region.resolve("reg-1");
  await flush();
  assert.equal(Region.status, "picking");
  assert.equal($(".send-btn").disabled, true);
  regionCancel();
  assert.equal($(".send-btn").disabled, false);
  assert.equal($(".region-btn").disabled, false);
});

test("history opening reserves the composer against region capture", async (t) => {
  const record = { id: "c-0000000000000001", title: "Saved chat", createdAt: 1, updatedAt: 1,
    messages: [{ role: "user", content: "hello" }, { role: "assistant", content: "hi" }] };
  answers.set("chat_history_list", [{ id: record.id, title: record.title, updatedAt: 1, count: 2 }]);
  const opening = deferred(t, "chat_history_open", record);
  $(".history-btn").fire("click");
  await flush();
  view.el.find(".history-row")[0].fire("click");
  $(".region-btn").fire("click");
  await flush();
  assert.equal($(".region-btn").disabled, true);
  assert.deepEqual(sent("region_start"), []);
  opening.resolve(record);
  await flush();
  assert.equal($(".region-btn").disabled, false);
});

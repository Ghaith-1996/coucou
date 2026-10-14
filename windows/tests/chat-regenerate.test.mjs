// Regenerate reserves the composer before native undo yields. Only the native
// commands are simulated; clicks and state updates use the real chat view.
import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, internals, sent, emit } from "./tauri.mjs";

installFakeDom();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { userAgent: "Windows" } });
const { buildPrompt, newConversation } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
let answers;
let view;
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  return cmd in answers ? answers[cmd](args) : result;
};
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const $ = (selector) => view.el.querySelector(selector);
const again = () => view.el.find(".md-copy").find((b) => b.getAttribute("aria-label") === "Regenerate");
function key(key) {
  for (const listener of $(".chat-input").listeners.get("keydown") ?? []) {
    listener({ key, preventDefault() {}, stopPropagation() {} });
  }
}
function deferred(t, command, fallback = true) {
  const pending = Promise.withResolvers();
  answers[command] = () => pending.promise;
  t.after(async () => { pending.resolve(fallback); await flush(); });
  return pending;
}
async function send(query) {
  $(".chat-input").value = query;
  $(".send-btn").fire("click");
  await flush();
  view.sync();
}
beforeEach(async () => {
  calls.length = 0;
  answers = { chat_send: ({ query }) => ({ text: `Answer to ${query}` }), chat_undo_last: () => true };
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.chatHistory = [];
  State.chatId = null;
  State.stateOverride = null;
  State.noteMessage = null;
  State.view = "prompt";
  State.droppedFile = null;
  view = buildPrompt(() => {});
  await send("First question");
  await send("Second question");
  calls.length = 0;
});

test("rapid Regenerate clicks reserve the composer and undo only the last turn once", async (t) => {
  const undo = deferred(t, "chat_undo_last");
  const button = again();
  button.fire("click");
  button.fire("click");
  await flush();
  view.sync();
  assert.equal(sent("chat_undo_last").length, 1);
  assert.equal(sent("chat_send").length, 0);
  assert.equal(State.chatHistory.length, 4, "keep the answer visible until undo succeeds");
  assert.equal($(".chat-input").disabled, true);
  assert.equal($(".model-btn").disabled, true);
  assert.equal($(".history-btn").disabled, true);
  assert.equal($(".voice-btn").disabled, true);
  assert.equal(State.stateOverride, "thinking", "the new-chat shortcut also sees the reservation");
  undo.resolve(true);
  await flush();
  view.sync();
  assert.deepEqual(sent("chat_send").map(({ query }) => query), ["Second question"]);
  assert.deepEqual(State.chatHistory.map(({ content }) => content), ["First question", "Answer to First question", "Second question", "Answer to Second question"]);
  assert.equal($(".chat-input").disabled, false);
});

test("undo preparation blocks Send, Enter, microphone, model and history clicks", async (t) => {
  const undo = deferred(t, "chat_undo_last");
  again().fire("click");
  $(".chat-input").value = "Keep this draft";
  $(".send-btn").fire("click");
  key("Enter");
  $(".voice-btn").fire("click");
  $(".model-btn").fire("click");
  $(".history-btn").fire("click");
  await flush();
  assert.equal(sent("chat_send").length, 0);
  assert.equal(sent("assistant_voice_toggle").length, 0);
  assert.equal(sent("chat_history_list").length, 0);
  assert.equal($(".chat-input").value, "Keep this draft");
  assert.equal($(".send-btn").disabled, true, "Stop is available after atomic undo completes");
  key("Escape");
  assert.equal(sent("chat_stop").length, 0, "there is no running answer to stop yet");
  undo.resolve(true);
  await flush();
  assert.equal(sent("chat_send").length, 1);
});

for (const outcome of [false, null, "error"]) {
  test(`a ${outcome} undo keeps both turns, reports failure and releases the composer`, async (t) => {
    const original = structuredClone(State.chatHistory);
    const undo = deferred(t, "chat_undo_last");
    again().fire("click");
    if (outcome === "error") undo.reject(new Error("Undo unavailable"));
    else undo.resolve(outcome);
    await flush();
    view.sync();
    assert.deepEqual(State.chatHistory, original);
    assert.equal(sent("chat_send").length, 0);
    assert.equal(sent("chat_history_save").length, 0);
    assert.match(State.noteMessage, /regenerate/i);
    assert.equal(State.stateOverride, null);
    assert.equal($(".chat-input").disabled, false);
    State.view = "prompt";
    answers.chat_undo_last = () => true;
    again().fire("click");
    await flush();
    assert.equal(sent("chat_send").length, 1, "retry works after a failed undo");
  });
}

test("Stop still cancels the regenerated answer after undo completes", async (t) => {
  const reply = deferred(t, "chat_send", { text: "Unused" });
  answers.chat_stop = () => reply.reject(new Error("Stopped."));
  again().fire("click");
  await flush();
  view.sync();
  assert.equal($(".send-btn").disabled, false);
  emit("chat-delta", "Partial replacement");
  $(".send-btn").fire("click");
  await flush();
  assert.equal(sent("chat_stop").length, 1);
  assert.equal(State.chatHistory.length, 4);
  assert.equal(State.chatHistory.at(-1).content, "Partial replacement");
  assert.equal(State.chatHistory.at(-1).stopped, true);
  assert.equal(State.view, "prompt");
});

test("regenerating a stopped answer does not undo an earlier complete turn", async () => {
  State.chatHistory.at(-1).stopped = true;
  again().fire("click");
  await flush();
  assert.equal(sent("chat_undo_last").length, 0);
  assert.deepEqual(sent("chat_send").map(({ query }) => query), ["Second question"]);
  assert.equal(State.chatHistory.length, 4);
});

test("a reset while undo is pending cannot revive the old question", async (t) => {
  const undo = deferred(t, "chat_undo_last");
  again().fire("click");
  newConversation();
  view.sync();
  undo.resolve(true);
  await flush();
  view.sync();
  assert.equal(sent("chat_send").length, 0);
  assert.deepEqual(State.chatHistory, []);
  assert.equal(State.chatId, null);
  assert.equal($(".chat-input").disabled, false);
});

test("a pending undo failure does not replace a newer permission card", async (t) => {
  const undo = deferred(t, "chat_undo_last");
  again().fire("click");
  State.view = "approval";
  view.sync();
  undo.resolve(false);
  await flush();
  assert.equal(State.view, "approval");
  assert.equal(sent("chat_send").length, 0);
  assert.equal(State.chatHistory.length, 4);
});

test("late stream deltas during undo cannot replace the preserved answer", async (t) => {
  const undo = deferred(t, "chat_undo_last");
  again().fire("click");
  view.sync();
  emit("chat-delta", "A stale delta");
  assert.deepEqual(view.el.find(".reply").map((reply) => reply.textContent), ["Answer to First question", "Answer to Second question"]);
  undo.resolve(false);
  await flush();
  view.sync();
  assert.deepEqual(view.el.find(".reply").map((reply) => reply.textContent), ["Answer to First question", "Answer to Second question"]);
});

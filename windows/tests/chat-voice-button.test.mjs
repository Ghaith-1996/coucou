// The chat microphone opens the real voice controller; only the native
// microphone/transcriber IPC is simulated, so tests never record the room.
import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";

installFakeDom();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { userAgent: "Windows" } });
const { buildPrompt, applyChatDraft } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
const { CONTRACT_VERSION } = await import("../src/assistant/contracts.ts");
const { Voice, registerVoice, resetVoiceForTests, voiceCancel, voiceUse } = await import("../src/assistant/voice.ts");

let answers = {};
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  return cmd in answers ? (typeof answers[cmd] === "function" ? answers[cmd](args) : answers[cmd]) : result;
};
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const host = {
  alert(view) { State.view = view; State.mode = "expanded"; State.notify(); },
  collapse() { State.mode = "compact"; State.notify(); },
  emote() {},
  takeKeyboard() {},
  setPinned(on) { State.isPinned = on; State.notify(); },
  applyPills() { return false; },
  showChatDraft(text) { State.view = "prompt"; return applyChatDraft(text); },
};
let registered = false;
let view;
beforeEach(() => {
  resetVoiceForTests();
  if (!registered) {
    registerVoice(host);
    registered = true;
  } else {
    // Keep the original state guard, replacing only the host reset for tests.
    const subscribe = State.subscribe;
    State.subscribe = () => () => {};
    registerVoice(host);
    State.subscribe = subscribe;
  }
  calls.length = 0;
  answers = { assistant_voice_toggle: "cap-chat" };
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.chatHistory = [];
  State.stateOverride = null;
  State.pendingApproval = null;
  State.isPinned = false;
  State.view = "prompt";
  State.mode = "expanded";
  State.droppedFile = null;
  view = buildPrompt(() => {});
  view.sync();
});
afterEach(resetVoiceForTests);
const mic = () => {
  const button = view.el.querySelector(".voice-btn");
  assert.ok(button, "a microphone button is available beside Send");
  return button;
};

test("the chat microphone starts voice with the shortcut off and returns an unsent draft", async () => {
  view.el.querySelector(".chat-input").value = "existing draft";
  mic().fire("click");
  await flush();
  assert.equal(State.view, "voice");
  assert.equal(Voice.event.state, "listening");
  assert.equal(State.isPinned, true);
  assert.equal(view.el.querySelector(".chat-input").value, "existing draft");
  emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "cap-chat", state: "preview", transcript: "Dictated text" });
  answers.assistant_voice_use = { actionId: "act-chat", text: "Dictated text" };
  await voiceUse();
  assert.equal(State.view, "prompt");
  assert.equal(view.el.querySelector(".chat-input").value, "Dictated text");
  assert.equal(sent("chat_send").length, 0);
  assert.equal(sent("assistant_voice_toggle").length, 1);
});

test("the chat microphone opens a waiting agent card without recording or answering it", async () => {
  State.pendingApproval = { requestId: "r1", sessionId: "s1", pillId: "p1", tool: "Bash", command: "ls" };
  mic().fire("click");
  await flush();
  assert.equal(State.view, "approval");
  assert.equal(sent("assistant_voice_toggle").length, 0);
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0);
});

// These tests use the real composer/controller with deferred native replies.
// Every deferred reply is settled even after a failed assertion, so no capture
// from one test can complete in another test's view.
function deferredAnswer(t, command, fallback = null) {
  const deferred = Promise.withResolvers();
  answers[command] = deferred.promise;
  t.after(async () => { deferred.resolve(fallback); await flush(); });
  return deferred;
}

function submitDraft(method) {
  if (method === "Send") view.el.querySelector(".send-btn").fire("click");
  else {
    const event = { key: "Enter", preventDefault() {}, stopPropagation() {} };
    for (const listener of view.el.querySelector(".chat-input").listeners.get("keydown") ?? []) listener(event);
  }
}

for (const method of ["Send", "Enter"]) {
  test(`microphone startup blocks ${method} synchronously and keeps the draft`, async (t) => {
    const start = deferredAnswer(t, "assistant_voice_toggle");
    deferredAnswer(t, "chat_send", { text: "Hello" });
    const input = view.el.querySelector(".chat-input");
    input.value = "existing draft";
    mic().fire("click");
    // No turn of the event loop between these user actions.
    submitDraft(method);
    await flush();
    assert.deepEqual(sent("chat_send"), [], "startup must own the composer before its IPC reply");
    assert.deepEqual(State.chatHistory, []);
    assert.equal(input.value, "existing draft");
    assert.equal(mic().disabled, true);
    assert.equal(view.el.querySelector(".send-btn").disabled, true);
    view.sync();
    assert.equal(mic().disabled, true, "a redraw must not release the pending start");
    assert.equal(view.el.querySelector(".send-btn").disabled, true);
    start.resolve("cap-chat");
    await flush();
    assert.equal(State.view, "voice");
    assert.equal(Voice.event.captureId, "cap-chat");
    assert.equal(Voice.event.state, "listening");
    assert.equal(input.value, "existing draft");
    assert.deepEqual(sent("chat_send"), []);
    assert.equal(mic().disabled, false);
    assert.equal(Boolean(view.el.querySelector(".send-btn").disabled), false);
  });

  test(`${method} blocks microphone capture until the pending chat reply completes`, async (t) => {
    const reply = deferredAnswer(t, "chat_send", { text: "Hello" });
    view.el.querySelector(".chat-input").value = "hello";
    submitDraft(method);
    mic().fire("click");
    await flush();
    assert.equal(mic().disabled, true);
    assert.deepEqual(sent("assistant_voice_toggle"), []);
    assert.equal(sent("chat_send").length, 1);
    reply.resolve({ text: "Hello" });
    await flush();
    assert.equal(mic().disabled, false);
    mic().fire("click");
    await flush();
    assert.equal(Voice.event.captureId, "cap-chat");
    assert.equal(sent("assistant_voice_toggle").length, 1);
  });
}

test("repeated microphone clicks during startup do not stop the accepted capture", async (t) => {
  const start = deferredAnswer(t, "assistant_voice_toggle");
  mic().fire("click");
  mic().fire("click");
  start.resolve("cap-chat");
  await flush();
  assert.equal(sent("assistant_voice_toggle").length, 1);
  assert.deepEqual(sent("assistant_voice_stop"), []);
  assert.equal(Voice.event.captureId, "cap-chat");
  assert.equal(Voice.event.state, "listening");
});

for (const outcome of ["rejection", "no capture"]) {
  test(`startup ${outcome} restores the composer without sending or losing its draft`, async (t) => {
    const start = deferredAnswer(t, "assistant_voice_toggle");
    const input = view.el.querySelector(".chat-input");
    input.value = "keep this draft";
    mic().fire("click");
    await flush();
    if (outcome === "rejection") {
      t.mock.method(console, "error", () => {});
      start.reject(new Error("native startup failed"));
    }
    else start.resolve(null);
    await flush();
    assert.equal(State.view, "prompt");
    assert.equal(Voice.event, null);
    assert.equal(input.value, "keep this draft");
    assert.deepEqual(sent("chat_send"), []);
    assert.equal(mic().disabled, false);
    assert.equal(Boolean(view.el.querySelector(".send-btn").disabled), false);
    answers.chat_send = { text: "Hello" };
    submitDraft("Enter");
    await flush();
    assert.equal(sent("chat_send").length, 1);
    assert.equal(State.chatHistory[0].content, "keep this draft");
  });
}

for (const card of ["approval", "question"]) {
  for (const outcome of ["capture", "rejection"]) {
    test(`${card} arriving during startup keeps priority after a late ${outcome}`, async (t) => {
      const start = deferredAnswer(t, "assistant_voice_toggle");
      const input = view.el.querySelector(".chat-input");
      input.value = "keep this draft";
      mic().fire("click");
      await flush();
      State.pendingApproval = { requestId: "r1", ...(card === "question" && { questions: [] }) };
      State.isPinned = true;
      host.alert(card);
      emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "cap-chat", state: "listening" });
      if (outcome === "capture") start.resolve("cap-chat");
      else {
        t.mock.method(console, "error", () => {});
        start.reject(new Error("native startup failed"));
      }
      await flush();
      assert.equal(State.view, card);
      assert.equal(State.isPinned, true);
      assert.equal(Voice.event, null);
      assert.equal(input.value, "keep this draft");
      assert.deepEqual(sent("chat_send"), []);
      assert.deepEqual(sent("assistant_voice_cancel"), outcome === "capture" ? [{ captureId: "cap-chat" }] : []);
      assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0);
      assert.equal(mic().disabled, false);
      assert.equal(Boolean(view.el.querySelector(".send-btn").disabled), false);
    });
  }
}

test("a cancelled startup cannot reclaim a newer capture through late events", async (t) => {
  const start = deferredAnswer(t, "assistant_voice_toggle");
  const input = view.el.querySelector(".chat-input");
  input.value = "keep this draft";
  mic().fire("click");
  await flush();
  voiceCancel();
  State.view = "overview";
  State.notify();
  start.resolve("cap-old");
  await flush();
  assert.equal(State.view, "overview");
  assert.equal(Voice.event, null);
  assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-old" }]);
  assert.equal(mic().disabled, false);
  State.view = "prompt";
  answers.assistant_voice_toggle = "cap-new";
  emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "cap-old", state: "idle" });
  mic().fire("click");
  await flush();
  for (const state of ["listening", "preview", "idle"]) {
    emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "cap-old", state });
    assert.equal(Voice.event.captureId, "cap-new");
    assert.equal(Voice.event.state, "listening");
  }
  assert.equal(State.view, "voice");
  assert.equal(State.isPinned, true);
  assert.equal(input.value, "keep this draft");
  assert.deepEqual(sent("chat_send"), []);
});

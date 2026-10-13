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
const { Voice, registerVoice, resetVoiceForTests, voiceUse } = await import("../src/assistant/voice.ts");

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

test("the microphone cannot capture while a chat reply is pending and recovers afterwards", async () => {
  let finish;
  answers.chat_send = () => new Promise((resolve) => { finish = resolve; });
  view.el.querySelector(".chat-input").value = "hello";
  view.el.querySelector(".send-btn").fire("click");
  await flush();
  assert.equal(mic().disabled, true);
  mic().fire("click");
  await flush();
  assert.equal(sent("assistant_voice_toggle").length, 0);
  finish({ text: "Hello" });
  await flush();
  assert.equal(mic().disabled, false);
});

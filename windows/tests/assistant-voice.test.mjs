// Voice capture on the island (src/assistant/voice.ts): what the shortcut
// does, the two island rules (an agent's waiting card comes first; the
// microphone only stays open while its card is on screen), stale events, and
// the chat draft with its receipt. Rust is the mock in tests/tauri.mjs.

import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { calls, emit, internals, sent } from "./tauri.mjs";
import { State } from "../src/core/state.ts";
import { Voice, registerVoice, resetVoiceForTests, voiceShortcut, voiceUse } from "../src/assistant/voice.ts";

// What the fake Rust answers, per command.
const answers = new Map();
const realInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  await realInvoke(cmd, args);
  if (!answers.has(cmd)) return null;
  const answer = answers.get(cmd);
  if (answer instanceof Error) throw answer;
  return answer;
};

const host = {
  alerts: [],
  drafts: [],
  draftLands: true,
  alert(view) {
    this.alerts.push(view);
    State.view = view;
    State.mode = "expanded";
    State.notify();
  },
  collapse() {
    State.mode = "compact";
    State.notify();
  },
  emote() {},
  takeKeyboard() {},
  setPinned(on) {
    State.isPinned = on;
    State.notify();
  },
  showChatDraft(text) {
    this.drafts.push(text);
    State.view = "prompt";
    State.notify();
    return this.draftLands;
  },
};

let registered = false;

beforeEach(() => {
  resetVoiceForTests();
  if (!registered) {
    registerVoice(host);
    registered = true;
  }
  // resetVoiceForTests forgets the host; the listeners stay registered.
  registerVoiceHostOnly();
  calls.length = 0;
  answers.clear();
  host.alerts = [];
  host.drafts = [];
  host.draftLands = true;
  State.pendingApproval = null;
  State.isPinned = false;
  State.view = "overview";
  State.mode = "compact";
});

function registerVoiceHostOnly() {
  // registerVoice again would add a second guard; the host is all that was lost.
  // Re-registering the event listener is harmless (the mock keeps the last one).
  const subscribe = State.subscribe;
  State.subscribe = () => () => {};
  registerVoice(host);
  State.subscribe = subscribe;
}

const ev = (captureId, state, extra = {}) => ({ v: 1, captureId, state, ...extra });
const flush = () => new Promise((r) => setTimeout(r, 0));

async function startListening(id = "cap-1") {
  answers.set("assistant_voice_toggle", id);
  await voiceShortcut();
  emit("assistant-voice", ev(id, "listening", { level: 0.2, elapsedMs: 100 }));
}

test("the shortcut opens the microphone and its card", async () => {
  await startListening();
  assert.equal(sent("assistant_voice_toggle").length, 1);
  assert.deepEqual(host.alerts, ["voice"]);
  assert.equal(Voice.event.state, "listening");
  assert.equal(Voice.event.level, 0.2);
});

test("the card keeps the island open while it is up, then gives the pin back", async () => {
  await startListening();
  assert.equal(State.isPinned, true, "the auto-close must not cut a capture short");
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hi", simulated: true }));
  assert.equal(State.isPinned, true);
  emit("assistant-voice", ev("cap-1", "idle"));
  assert.equal(State.isPinned, false);

  // Pinned by the user before: still pinned after.
  State.isPinned = true;
  await startListening("cap-2");
  emit("assistant-voice", ev("cap-2", "idle"));
  assert.equal(State.isPinned, true);
});

test("an agent's card that arrived meanwhile keeps its pin", async () => {
  await startListening();
  State.pendingApproval = { requestId: "r1", sessionId: "s", pillId: "p", tool: "Bash", command: "ls" };
  host.alert("approval");
  emit("assistant-voice", ev("cap-1", "idle"));
  assert.equal(State.isPinned, true);
});

test("a second press stops the capture that is listening", async () => {
  await startListening();
  await voiceShortcut();
  assert.deepEqual(sent("assistant_voice_stop"), [{ captureId: "cap-1" }]);
  assert.equal(sent("assistant_voice_toggle").length, 1);
});

test("an agent's waiting card comes first: the shortcut opens it, not the microphone", async () => {
  State.pendingApproval = { requestId: "r1", sessionId: "s", pillId: "p", tool: "Bash", command: "ls" };
  await voiceShortcut();
  assert.equal(sent("assistant_voice_toggle").length, 0);
  assert.deepEqual(host.alerts, ["approval"]);
  // Nothing about the request is answered from here.
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0);
});

test("a permission card arriving while listening cancels the capture", async () => {
  await startListening();
  State.pendingApproval = { requestId: "r1", sessionId: "s", pillId: "p", tool: "Bash", command: "ls" };
  host.alert("approval");
  await flush();
  assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-1" }]);
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0);
});

test("closing the island while listening closes the microphone", async () => {
  await startListening();
  host.collapse();
  await flush();
  assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-1" }]);
});

test("another capture's late events are ignored", async () => {
  await startListening("cap-2");
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "old", simulated: true }));
  assert.equal(Voice.event.captureId, "cap-2");
  assert.equal(Voice.event.state, "listening");
  // And anything malformed never reaches the card.
  emit("assistant-voice", { v: 1, captureId: "cap-2", state: "approving" });
  assert.equal(Voice.event.state, "listening");
});

test("idle from Rust ends the capture and folds the card", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "idle"));
  assert.equal(Voice.event, null);
  assert.equal(State.mode, "compact");
});

test("use puts the transcript in the chat, unsent, and reports the evidence", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello Mochi", simulated: true }));
  answers.set("assistant_voice_use", { actionId: "act-9", text: "hello Mochi" });
  await voiceUse();
  assert.deepEqual(sent("assistant_voice_use"), [{ captureId: "cap-1" }]);
  assert.deepEqual(host.drafts, ["hello Mochi"]);
  assert.deepEqual(sent("assistant_action_receipt"), [
    { receipt: { actionId: "act-9", outcome: "succeeded", evidenceRef: "chat-field:draft-set" } },
  ]);
  // Nothing was sent to a model: the user still presses Send.
  assert.equal(sent("chat_send").length, 0);
  // Leaving the card for the chat is not a cancel.
  assert.equal(sent("assistant_voice_cancel").length, 0);
});

test("a draft that didn't land is reported as failed, not succeeded", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hi", simulated: true }));
  answers.set("assistant_voice_use", { actionId: "act-10", text: "hi" });
  host.draftLands = false;
  await voiceUse();
  assert.deepEqual(sent("assistant_action_receipt"), [
    { receipt: { actionId: "act-10", outcome: "failed", errorCode: "unverified" } },
  ]);
});

test("a refused or expired use does nothing", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hi", simulated: true }));
  answers.set("assistant_voice_use", new Error("expired"));
  await voiceUse();
  assert.deepEqual(host.drafts, []);
  assert.equal(sent("assistant_action_receipt").length, 0);
  assert.equal(Voice.event, null);
});

test("an error shows, then goes by itself", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  await startListening();
  emit("assistant-voice", ev("cap-1", "error", { error: "no-microphone" }));
  assert.equal(Voice.event.error, "no-microphone");
  t.mock.timers.tick(4000);
  assert.equal(Voice.event, null);
});

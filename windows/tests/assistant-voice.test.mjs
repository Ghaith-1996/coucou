// Voice capture on the island (src/assistant/voice.ts): what the shortcut
// does, the two island rules (an agent's waiting card comes first; the
// microphone only stays open while its card is on screen), stale events, and
// the chat draft with its receipt. Rust is the mock in tests/tauri.mjs.

import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { calls, emit, internals, sent } from "./tauri.mjs";
import { State } from "../src/core/state.ts";
import { Voice, registerVoice, resetVoiceForTests, voiceCancel, voiceDo, voiceShortcut, voiceUse } from "../src/assistant/voice.ts";

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
  pillsApplied: [],
  applyPills(next) {
    this.pillsApplied.push(next);
    State.settings.mainPill = next.mainPill;
    State.settings.activeIntegrations = [...next.activeIntegrations];
    return true;
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
  host.pillsApplied = [];
  host.draftLands = true;
  State.settings.mainPill = "integration_claude";
  State.settings.activeIntegrations = ["integration_vercel"];
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

test("a spoken pill command is offered, proposed as a pill-set, then applied", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Ajoute GitHub." }));
  assert.equal(Voice.command.ok, true);
  assert.equal(Voice.command.label, "Add GitHub");
  answers.set("assistant_voice_act", "act-77");
  await voiceDo();
  assert.deepEqual(sent("assistant_voice_act"), [{
    captureId: "cap-1",
    kind: "pill-set",
    args: { mainPill: "integration_claude", activeIntegrations: ["integration_vercel", "integration_github"] },
  }]);
  assert.deepEqual(host.pillsApplied, [{ mainPill: "integration_claude", activeIntegrations: ["integration_vercel", "integration_github"] }]);
  assert.deepEqual(sent("assistant_action_receipt"), [
    { receipt: { actionId: "act-77", outcome: "succeeded", evidenceRef: "settings:pills-shown" } },
  ]);
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0);
});

test("a simulated transcript is never taken for a command", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Ajoute GitHub.", simulated: true }));
  assert.equal(Voice.command, null);
  await voiceDo();
  assert.equal(sent("assistant_voice_act").length, 0);
});

test("a refused pill-set changes nothing", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Ajoute GitHub." }));
  answers.set("assistant_voice_act", new Error("invalid-args"));
  await voiceDo();
  assert.deepEqual(host.pillsApplied, []);
  assert.equal(sent("assistant_action_receipt").length, 0);
});

function pendingStart() {
  const deferred = Promise.withResolvers();
  answers.set("assistant_voice_toggle", deferred.promise);
  return { ...deferred, finished: voiceShortcut() };
}

for (const error of ["no-microphone", "microphone-denied"]) {
  test(`${error} before the toggle reply stays an error, then releases the island`, async (t) => {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    const start = pendingStart();
    emit("assistant-voice", ev("cap-1", "error", { error }));
    start.resolve("cap-1");
    await start.finished;
    assert.equal(Voice.event.state, "error");
    assert.equal(Voice.event.error, error);
    t.mock.timers.tick(4000);
    assert.equal(Voice.event, null);
    assert.equal(State.isPinned, false);
    assert.equal(State.mode, "compact");
  });
}

for (const state of ["processing", "preview", "idle"]) {
  test(`${state} before the toggle reply is not overwritten by listening`, async () => {
    const start = pendingStart();
    emit("assistant-voice", ev("cap-1", "listening"));
    emit("assistant-voice", ev("cap-1", state));
    start.resolve("cap-1");
    await start.finished;
    assert.equal(Voice.event?.state ?? null, state === "idle" ? null : state);
    if (state === "idle") {
      assert.deepEqual(host.alerts, []);
      assert.equal(State.isPinned, false);
    }
  });
}

for (const view of ["approval", "question"]) {
  test(`${view} arriving during startup keeps the island and cancels the returned capture`, async () => {
    const start = pendingStart();
    State.pendingApproval = { requestId: "r1", ...(view === "question" && { questions: [] }) };
    State.isPinned = true;
    host.alert(view);
    emit("assistant-voice", ev("cap-1", "listening"));
    start.resolve("cap-1");
    await start.finished;
    assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-1" }]);
    assert.deepEqual(host.alerts, [view]);
    assert.equal(State.view, view);
    assert.equal(State.isPinned, true);
    assert.equal(Voice.event, null);
    emit("assistant-voice", ev("cap-1", "listening"));
    assert.equal(Voice.event, null);
  });
}

test("an approval seen during startup still cancels if it was withdrawn before the reply", async () => {
  const start = pendingStart();
  State.pendingApproval = { requestId: "r1" };
  State.notify();
  State.pendingApproval = null;
  State.notify();
  start.resolve("cap-1");
  await start.finished;
  assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-1" }]);
  assert.deepEqual(host.alerts, []);
});

test("cancel during startup retires the capture before any late event", async () => {
  const start = pendingStart();
  voiceCancel();
  start.resolve("cap-1");
  await start.finished;
  for (const state of ["listening", "preview", "idle"]) emit("assistant-voice", ev("cap-1", state));
  assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-1" }]);
  assert.equal(Voice.event, null);
  assert.deepEqual(host.alerts, []);
});

test("stale listening cannot replace an active capture or revive an ended capture", async () => {
  await startListening("cap-2");
  emit("assistant-voice", ev("cap-1", "listening"));
  assert.equal(Voice.event.captureId, "cap-2");
  emit("assistant-voice", ev("cap-2", "idle"));
  emit("assistant-voice", ev("cap-2", "listening"));
  assert.equal(Voice.event, null);
  assert.deepEqual(host.alerts, ["voice"]);
});

test("startup only keeps events for the returned capture id", async () => {
  const start = pendingStart();
  emit("assistant-voice", ev("old", "error", { error: "no-microphone" }));
  start.resolve("cap-1");
  await start.finished;
  assert.equal(Voice.event.captureId, "cap-1");
  assert.equal(Voice.event.state, "listening");
});

test("a second shortcut during startup queues stop instead of starting a second capture", async () => {
  const start = pendingStart();
  const second = voiceShortcut();
  start.resolve("cap-1");
  await Promise.all([start.finished, second]);
  assert.equal(sent("assistant_voice_toggle").length, 1);
  assert.deepEqual(sent("assistant_voice_stop"), [{ captureId: "cap-1" }]);
});

test("a null toggle response allows a later start", async () => {
  const start = pendingStart();
  start.resolve(null);
  await start.finished;
  await startListening("cap-2");
  assert.equal(Voice.event.captureId, "cap-2");
});

test("approval cancels before the view has changed away from the expanded voice card", async () => {
  await startListening();
  State.pendingApproval = { requestId: "r1" };
  State.notify();
  assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-1" }]);
  assert.equal(State.isPinned, true);
});

for (const change of ["close", "hide", "navigate", "navigate-back"]) {
  test(`${change} while startup is pending cancels without reopening voice`, async () => {
    State.mode = "expanded";
    const start = pendingStart();
    emit("assistant-voice", ev("cap-1", "listening"));
    if (change === "close") host.collapse();
    else if (change === "hide") {
      State.mode = "hidden";
      State.notify();
    } else {
      State.view = "prompt";
      State.notify();
      if (change === "navigate-back") {
        State.view = "overview";
        State.notify();
      }
    }
    const mode = State.mode;
    const view = State.view;
    start.resolve("cap-1");
    await start.finished;
    assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-1" }]);
    assert.equal(Voice.event, null);
    assert.deepEqual(host.alerts, []);
    assert.equal(State.mode, mode);
    assert.equal(State.view, view);
  });
}


test("a pill command is replanned against settings changed since preview", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Add GitHub." }));
  State.settings.mainPill = "agent_cursor";
  State.settings.activeIntegrations = ["integration_vercel", "integration_notion"];
  answers.set("assistant_voice_act", "act-77");
  await voiceDo();
  const expected = { mainPill: "agent_cursor", activeIntegrations: ["integration_vercel", "integration_notion", "integration_github"] };
  assert.deepEqual(sent("assistant_voice_act")[0].args, expected);
  assert.deepEqual(host.pillsApplied, [expected]);
});

test("a pill command made invalid by settings changes is not proposed or applied", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Add GitHub." }));
  State.settings.activeIntegrations = ["integration_vercel", "integration_notion", "integration_stripe", "integration_resend"];
  answers.set("assistant_voice_act", "act-77");
  await voiceDo();
  assert.equal(sent("assistant_voice_act").length, 0);
  assert.deepEqual(host.pillsApplied, []);
  assert.equal(Voice.command.ok, false);
});

test("settings changed during the policy request are not overwritten", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Add GitHub." }));
  const deferred = Promise.withResolvers();
  answers.set("assistant_voice_act", deferred.promise);
  const done = voiceDo();
  State.settings.activeIntegrations = ["integration_vercel", "integration_notion"];
  deferred.resolve("act-77");
  await done;
  assert.deepEqual(host.pillsApplied, []);
  assert.deepEqual(State.settings.activeIntegrations, ["integration_vercel", "integration_notion"]);
  assert.deepEqual(sent("assistant_action_receipt"), [{ receipt: { actionId: "act-77", outcome: "cancelled", errorCode: "cancelled" } }]);
});

for (const result of ["success", "failure"]) {
  test(`a stale Use ${result} cannot clear a newer capture`, async () => {
    await startListening("cap-1");
    emit("assistant-voice", ev("cap-1", "preview", { transcript: "old", simulated: true }));
    const deferred = Promise.withResolvers();
    answers.set("assistant_voice_use", deferred.promise);
    const use = voiceUse();
    // Rust consumes the preview and goes idle before the command reply arrives.
    emit("assistant-voice", ev("cap-1", "idle"));
    await startListening("cap-2");
    if (result === "success") deferred.resolve({ actionId: "act-old", text: "old" });
    else deferred.reject(new Error("expired"));
    await use;
    assert.equal(Voice.event.captureId, "cap-2");
    assert.equal(Voice.event.state, "listening");
    assert.equal(State.isPinned, true);
    assert.equal(State.view, "voice");
    assert.deepEqual(host.drafts, []);
    emit("assistant-voice", ev("cap-2", "processing"));
    assert.equal(Voice.event.state, "processing");
  });
}

test("Use still completes after its ordinary Rust idle event", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello", simulated: true }));
  const deferred = Promise.withResolvers();
  answers.set("assistant_voice_use", deferred.promise);
  const use = voiceUse();
  emit("assistant-voice", ev("cap-1", "idle"));
  deferred.resolve({ actionId: "act-1", text: "hello" });
  await use;
  assert.deepEqual(host.drafts, ["hello"]);
});

test("Cancel while Use is pending prevents its eventual draft", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello", simulated: true }));
  const deferred = Promise.withResolvers();
  answers.set("assistant_voice_use", deferred.promise);
  const use = voiceUse();
  voiceCancel();
  deferred.resolve({ actionId: "act-1", text: "hello" });
  await use;
  assert.deepEqual(host.drafts, []);
});

test("an untouched preview expires and releases the microphone card", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello" }));
  t.mock.timers.tick(119999);
  assert.equal(Voice.event.state, "preview");
  t.mock.timers.tick(1);
  assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "cap-1" }]);
  assert.equal(Voice.event, null);
  assert.equal(State.isPinned, false);
  assert.equal(State.mode, "compact");
});

test("an old preview timeout cannot cancel a newer capture", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  await startListening("cap-1");
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello" }));
  t.mock.timers.tick(60000);
  await startListening("cap-2");
  t.mock.timers.tick(60000);
  assert.equal(Voice.event.captureId, "cap-2");
  assert.deepEqual(sent("assistant_voice_cancel"), []);
});

for (const result of ["success", "failure"]) {
  test(`a stale Do it ${result} cannot clear a newer capture`, async () => {
    await startListening("cap-1");
    emit("assistant-voice", ev("cap-1", "preview", { transcript: "Add GitHub." }));
    const deferred = Promise.withResolvers();
    answers.set("assistant_voice_act", deferred.promise);
    const done = voiceDo();
    emit("assistant-voice", ev("cap-1", "idle"));
    await startListening("cap-2");
    if (result === "success") deferred.resolve("act-old");
    else deferred.reject(new Error("expired"));
    await done;
    assert.equal(Voice.event.captureId, "cap-2");
    assert.equal(State.isPinned, true);
    assert.deepEqual(host.pillsApplied, []);
  });
}

test("an approval during Do it retains the island", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Add GitHub." }));
  const deferred = Promise.withResolvers();
  answers.set("assistant_voice_act", deferred.promise);
  const done = voiceDo();
  State.pendingApproval = { requestId: "r1" };
  host.alert("approval");
  deferred.resolve("act-old");
  await done;
  assert.equal(State.view, "approval");
  assert.equal(State.isPinned, true);
  assert.deepEqual(host.pillsApplied, []);
});

test("Do it still completes after its ordinary Rust idle event", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Add GitHub." }));
  const deferred = Promise.withResolvers();
  answers.set("assistant_voice_act", deferred.promise);
  const done = voiceDo();
  emit("assistant-voice", ev("cap-1", "idle"));
  deferred.resolve("act-1");
  await done;
  assert.equal(host.pillsApplied.length, 1);
});

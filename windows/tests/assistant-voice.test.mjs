// Voice capture on the island (src/assistant/voice.ts): what the shortcut
// does, the two island rules (an agent's waiting card comes first; the
// microphone only stays open while its card is on screen), stale events, and
// the chat draft with its receipt. Rust is the mock in tests/tauri.mjs.

import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { calls, emit, internals, sent } from "./tauri.mjs";
import { State } from "../src/core/state.ts";
import { CONTRACT_VERSION } from "../src/assistant/contracts.ts";
import { Voice, registerVoice, resetVoiceForTests, voiceCancel, voiceDo, voiceInsert, voiceOpen, voiceShortcut, voiceUse } from "../src/assistant/voice.ts";

// What the fake Rust answers, per command.
const answers = new Map();
const realInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await realInvoke(cmd, args);
  if (!answers.has(cmd)) return result;
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

// Retire the final preview too, so its real two-minute timer cannot hold Node open.
afterEach(resetVoiceForTests);

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

const ev = (captureId, state, extra = {}) => ({ v: CONTRACT_VERSION, captureId, state, ...extra });
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

test("the synthesized listening event uses the current contract version", async () => {
  answers.set("assistant_voice_toggle", "cap-1");
  await voiceShortcut();
  assert.deepEqual(Voice.event, ev("cap-1", "listening", { level: 0, elapsedMs: 0 }));
});

test("old and future contract events cannot become spoken pill commands", async () => {
  await startListening();
  for (const v of [1, CONTRACT_VERSION + 1]) {
    emit("assistant-voice", ev("cap-1", "preview", { v, transcript: "Add GitHub" }));
    assert.equal(Voice.event.state, "listening");
    assert.equal(Voice.command, null);
    await voiceDo();
  }
  assert.deepEqual(sent("assistant_voice_act"), []);
  assert.deepEqual(host.pillsApplied, []);
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
  emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "cap-2", state: "approving" });
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

for (const state of ["listening", "processing", "preview", "error"]) {
  test(`startup idle cannot be revived by a late ${state} event`, async () => {
    const start = pendingStart();
    emit("assistant-voice", ev("cap-1", "listening"));
    emit("assistant-voice", ev("cap-1", "idle"));
    emit("assistant-voice", ev("cap-1", state, state === "error" ? { error: "provider" } : {}));
    start.resolve("cap-1");
    await start.finished;
    assert.equal(Voice.event, null);
    assert.equal(State.isPinned, false);
    assert.deepEqual(host.alerts, []);
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


test("a blocked preview becomes actionable when Settings frees a slot", async () => {
  State.settings.activeIntegrations = ["integration_vercel", "integration_notion", "integration_resend", "integration_n8n"];
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Add GitHub." }));
  assert.equal(Voice.command.ok, false);
  let rendered;
  const unsubscribe = State.subscribe(() => { rendered = Voice.command; });
  try {
    State.settings.activeIntegrations.pop();
    State.notify();
    assert.equal(rendered.ok, true);
    assert.equal(rendered.label, "Add GitHub");
    assert.deepEqual(rendered.next.activeIntegrations, ["integration_vercel", "integration_notion", "integration_resend", "integration_github"]);
    assert.deepEqual(sent("assistant_voice_act"), [], "refreshing a preview never executes it");
    const next = rendered.next;
    answers.set("assistant_voice_act", "act-77");
    await voiceDo();
    assert.deepEqual(host.pillsApplied, [next]);
  } finally {
    unsubscribe();
  }
});

test("a preview tracks valid, blocked and valid settings without extending its expiry", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Add GitHub." }));
  assert.equal(Voice.command.ok, true);
  t.mock.timers.tick(60000);
  State.settings.activeIntegrations = ["integration_vercel", "integration_notion", "integration_resend", "integration_n8n"];
  State.notify();
  assert.equal(Voice.command.ok, false);
  State.settings.activeIntegrations = ["integration_github"];
  State.notify();
  assert.equal(Voice.command.ok, false);
  assert.match(Voice.command.reason, /already/);
  State.settings.activeIntegrations = [];
  State.notify();
  assert.equal(Voice.command.ok, true);
  t.mock.timers.tick(60000);
  assert.equal(Voice.event, null);
  assert.equal(Voice.command, null);
  assert.deepEqual(sent("assistant_voice_act"), []);
});

test("settings changes cannot turn a simulated or cancelled preview into a command", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Add GitHub.", simulated: true }));
  State.settings.activeIntegrations = [];
  State.notify();
  assert.equal(Voice.command, null);
  voiceCancel();
  State.settings.mainPill = "agent_cursor";
  State.notify();
  assert.equal(Voice.command, null);
  assert.deepEqual(sent("assistant_voice_act"), []);
});

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

// ── "Open <app>" (launch-app) ────────────────────────────────────────────────

const GRANT = { actionId: "act-90", digest: "ab".repeat(32), expiresAtMs: 9e12 };

test("article-led app queries reach Rust intact and still require confirmation", async () => {
  await startListening();
  answers.set("assistant_app_propose", { label: "The Witness", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Open an app The Witness." }));
  await flush();
  assert.deepEqual(sent("assistant_app_propose"), [{ captureId: "cap-1", name: "an app The Witness" }]);
  assert.equal(Voice.app.label, "The Witness");
  assert.equal(sent("assistant_app_launch").length, 0, "nothing opens before the click");
});

test("\"open <app>\" offers the app Rust found, and opens it only on the click", async () => {
  await startListening();
  answers.set("assistant_app_propose", { label: "Visual Studio Code", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Ouvre Visual Studio." }));
  assert.deepEqual(Voice.app, { name: "Visual Studio", status: "finding" });
  assert.equal(Voice.command, null, "not a pill command");
  await flush();
  assert.deepEqual(sent("assistant_app_propose"), [{ captureId: "cap-1", name: "Visual Studio" }]);
  assert.equal(Voice.app.status, "ready");
  assert.equal(Voice.app.label, "Visual Studio Code");
  assert.equal(sent("assistant_app_launch").length, 0, "nothing opens before the click");

  answers.set("assistant_app_launch", { actionId: "act-90", outcome: "succeeded", evidenceRef: "window:pid:42" });
  await voiceOpen();
  assert.deepEqual(sent("assistant_app_launch"), [{ captureId: "cap-1", grant: GRANT }]);
  assert.equal(Voice.event, null, "the card is done");
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0, "never the agents' permissions");
});

test("an app the Start menu doesn't have is said so, and the chat stays the choice", async () => {
  await startListening();
  answers.set("assistant_app_propose", null);
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Open Photoshop" }));
  await flush();
  assert.deepEqual(Voice.app, { name: "Photoshop", status: "missing" });
  await voiceOpen();
  assert.equal(sent("assistant_app_launch").length, 0);
  answers.set("assistant_voice_use", { actionId: "act-91", text: "Open Photoshop" });
  await voiceUse();
  assert.deepEqual(host.drafts, ["Open Photoshop"]);
});

test("a late answer about another capture's app changes nothing", async () => {
  await startListening();
  let release;
  answers.set("assistant_app_propose", new Promise((r) => { release = r; }));
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "ouvre Figma" }));
  voiceCancel();
  emit("assistant-voice", ev("cap-1", "idle")); // Native cancellation releases before restart.
  await startListening("cap-2");
  emit("assistant-voice", ev("cap-2", "preview", { transcript: "Ajoute GitHub." }));
  release({ label: "Figma", grant: GRANT });
  await flush();
  assert.equal(Voice.app, null);
  assert.equal(Voice.command.ok, true);
  await voiceOpen();
  assert.equal(sent("assistant_app_launch").length, 0);
});

test("a simulated transcript never looks for an app", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "ouvre Figma", simulated: true }));
  await flush();
  assert.equal(Voice.app, null);
  assert.equal(sent("assistant_app_propose").length, 0);
});

// ── Dictation into another app's field ──────────────────────────────────────

test("plain words are offered for the field the capture started in, and written only on the click", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Dear Sam, thanks for the notes." }));
  assert.deepEqual(Voice.insert, { status: "finding" });
  await flush();
  assert.deepEqual(sent("assistant_insert_offer"), [{ captureId: "cap-1" }]);
  assert.deepEqual(Voice.insert, { status: "ready", label: "Notepad", grant: GRANT });
  assert.equal(sent("assistant_insert_text").length, 0, "nothing is typed before the click");

  answers.set("assistant_insert_text", {
    status: "done", receipt: { actionId: "act-95", outcome: "succeeded", evidenceRef: "uia-value:read-back" },
  });
  await voiceInsert();
  assert.deepEqual(sent("assistant_insert_text"), [{ captureId: "cap-1", grant: GRANT }]);
  assert.equal(Voice.event, null, "the card is done");
  assert.deepEqual(host.drafts, [], "nothing went to the chat");
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0, "never the agents' permissions");
});

test("a command is never offered to another app's field", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "Ajoute GitHub." }));
  await flush();
  assert.equal(Voice.insert, null);
  assert.equal(sent("assistant_insert_offer").length, 0);
});

test("a simulated transcript is never offered to another app's field", async () => {
  await startListening();
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello", simulated: true }));
  await flush();
  assert.equal(Voice.insert, null);
  assert.equal(sent("assistant_insert_offer").length, 0);
});

test("no remembered field leaves the card as it was", async () => {
  await startListening();
  answers.set("assistant_insert_offer", null);
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello" }));
  await flush();
  assert.equal(Voice.insert, null);
  await voiceInsert();
  assert.equal(sent("assistant_insert_text").length, 0);
});

test("a field that can't take the words says why, and Insert does nothing", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "refused", reason: "protected" });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hunter2" }));
  await flush();
  assert.deepEqual(Voice.insert, { status: "refused", reason: "protected", copyAllowed: false });
  await voiceInsert();
  assert.equal(sent("assistant_insert_text").length, 0);
  assert.equal(Voice.event.state, "preview", "the transcript stays");
});

test("a field that moved keeps the transcript on the card, and Insert can be tried again", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello" }));
  await flush();
  answers.set("assistant_insert_text", { status: "kept", reason: "focus-moved" });
  await voiceInsert();
  assert.equal(Voice.event.state, "preview");
  assert.deepEqual(Voice.insert, { status: "ready", label: "Notepad", grant: GRANT, reason: "focus-moved" });
  answers.set("assistant_insert_text", {
    status: "done", receipt: { actionId: "act-96", outcome: "succeeded", evidenceRef: "uia-value:read-back" },
  });
  await voiceInsert();
  assert.equal(sent("assistant_insert_text").length, 2);
  assert.equal(Voice.event, null);
});

test("words that could not be written land in the chat instead, unsent", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "keep these words" }));
  await flush();
  answers.set("assistant_insert_text", {
    status: "failed",
    receipt: { actionId: "act-97", outcome: "failed", errorCode: "failed" },
    draft: { actionId: "act-98", text: "keep these words" },
  });
  await voiceInsert();
  assert.deepEqual(host.drafts, ["keep these words"]);
  assert.deepEqual(sent("assistant_action_receipt"), [
    { receipt: { actionId: "act-98", outcome: "succeeded", evidenceRef: "chat-field:draft-set" } },
  ]);
});

test("an unconfirmed write is not tried again", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "maybe there" }));
  await flush();
  answers.set("assistant_insert_text", { status: "done", receipt: { actionId: "act-99", outcome: "unknown" } });
  await voiceInsert();
  await voiceInsert();
  assert.equal(sent("assistant_insert_text").length, 1);
  assert.equal(Voice.event, null);
});

test("a late field answer for a cancelled capture changes nothing", async () => {
  await startListening();
  let release;
  answers.set("assistant_insert_offer", new Promise((r) => { release = r; }));
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello" }));
  voiceCancel();
  emit("assistant-voice", ev("cap-1", "idle")); // Native cancellation releases before restart.
  await startListening("cap-2");
  answers.set("assistant_insert_offer", null);
  emit("assistant-voice", ev("cap-2", "preview", { transcript: "Ajoute GitHub." }));
  release({ status: "ready", label: "Notepad", grant: GRANT });
  await flush();
  assert.equal(Voice.insert, null);
  await voiceInsert();
  assert.equal(sent("assistant_insert_text").length, 0);
});

for (const reason of ["focus-moved", "busy"]) {
  test(`an initial ${reason} refusal can re-offer the same capture without automatically writing`, async () => {
    await startListening();
    answers.set("assistant_insert_offer", { status: "refused", reason });
    emit("assistant-voice", ev("cap-1", "preview", { transcript: "keep these words" }));
    await flush();
    answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
    await voiceInsert();
    assert.deepEqual(sent("assistant_insert_offer"), [{ captureId: "cap-1" }, { captureId: "cap-1" }]);
    assert.deepEqual(Voice.insert, { status: "ready", label: "Notepad", grant: GRANT });
    assert.equal(sent("assistant_insert_text").length, 0, "the newly named field needs its own confirmation");
    answers.set("assistant_insert_text", { status: "done", receipt: { actionId: "act-retry", outcome: "succeeded", evidenceRef: "uia-value:read-back" } });
    await voiceInsert();
    assert.equal(sent("assistant_insert_text").length, 1);
  });
}

test("a duplicate retry click cannot request two offers", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "refused", reason: "busy" });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "hello" }));
  await flush();
  let release;
  answers.set("assistant_insert_offer", new Promise((resolve) => { release = resolve; }));
  const retry = voiceInsert();
  await voiceInsert();
  await flush();
  const count = sent("assistant_insert_offer").length;
  release({ status: "ready", label: "Notepad", grant: GRANT });
  await retry;
  assert.equal(count, 2);
  assert.equal(sent("assistant_insert_text").length, 0);
});

test("cancelling while a retry is pending cannot revive the old capture", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "refused", reason: "focus-moved" });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "old" }));
  await flush();
  let release;
  answers.set("assistant_insert_offer", new Promise((resolve) => { release = resolve; }));
  const retry = voiceInsert();
  voiceCancel();
  emit("assistant-voice", ev("cap-1", "idle")); // Native cancellation releases before restart.
  await startListening("cap-2");
  release({ status: "ready", label: "Notepad", grant: GRANT });
  await retry;
  assert.equal(Voice.event.captureId, "cap-2");
  assert.equal(Voice.insert, null);
  assert.equal(sent("assistant_insert_text").length, 0);
});

test("a duplicate insert click cannot submit two writes", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "once" }));
  await flush();
  let release;
  answers.set("assistant_insert_text", new Promise((resolve) => { release = resolve; }));
  const inserting = voiceInsert();
  await voiceInsert();
  await flush();
  const count = sent("assistant_insert_text").length;
  release({ status: "done", receipt: { actionId: "act-once", outcome: "unknown" } });
  await inserting;
  assert.equal(count, 1);
  await voiceInsert();
  assert.equal(sent("assistant_insert_text").length, 1);
});

test("a protected refusal at insertion removes Insert and keeps the transcript", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "secret" }));
  await flush();
  answers.set("assistant_insert_text", { status: "kept", reason: "protected" });
  await voiceInsert();
  assert.deepEqual(Voice.insert, { status: "refused", reason: "protected", copyAllowed: false });
  await voiceInsert();
  assert.equal(sent("assistant_insert_text").length, 1);
  assert.equal(Voice.event.state, "preview");
  assert.deepEqual(host.drafts, []);
});

for (const reason of ["gone", "read-only", "unsupported"]) {
  test(`a kept ${reason} refusal removes the grant and cannot retry insertion`, async () => {
    await startListening();
    answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
    emit("assistant-voice", ev("cap-1", "preview", { transcript: "keep these words" }));
    await flush();
    answers.set("assistant_insert_text", { status: "kept", reason });
    await voiceInsert();
    assert.deepEqual(Voice.insert, { status: "refused", reason, copyAllowed: false });
    await voiceInsert();
    assert.equal(sent("assistant_insert_text").length, 1);
    assert.equal(sent("assistant_insert_offer").length, 1);
    assert.equal(Voice.event.transcript, "keep these words");
  });
}

for (const reason of ["busy", "value-changed"]) {
  test(`a ${reason} insertion retains its named grant for a separate retry`, async () => {
    await startListening();
    answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
    emit("assistant-voice", ev("cap-1", "preview", { transcript: "keep these words" }));
    await flush();
    answers.set("assistant_insert_text", { status: "kept", reason, copyAllowed: false });
    await voiceInsert();
    assert.deepEqual(Voice.insert, { status: "ready", label: "Notepad", grant: GRANT, reason });
    assert.equal(Voice.event.state, "preview");
    assert.equal(State.isPinned, true);
    assert.equal(sent("assistant_insert_text").length, 1);
    answers.set("assistant_insert_text", { status: "done", receipt: { actionId: "act-retry", outcome: "unknown" } });
    await voiceInsert();
    assert.equal(sent("assistant_insert_text").length, 2);
    assert.equal(Voice.event, null);
  });
}

for (const reason of ["gone", "protected", "read-only", "unsupported"]) {
  test(`an initial ${reason} refusal cannot re-offer the field`, async () => {
    await startListening();
    answers.set("assistant_insert_offer", { status: "refused", reason });
    emit("assistant-voice", ev("cap-1", "preview", { transcript: "keep these words" }));
    await flush();
    await voiceInsert();
    assert.deepEqual(Voice.insert, { status: "refused", reason, copyAllowed: false });
    assert.equal(sent("assistant_insert_offer").length, 1);
    assert.equal(sent("assistant_insert_text").length, 0);
  });
}

for (const phase of ["re-offer", "insert"]) {
  test(`${phase} retries keep the original preview expiry`, async (t) => {
    await startListening();
    answers.set("assistant_insert_offer", phase === "re-offer"
      ? { status: "refused", reason: "busy" }
      : { status: "ready", label: "Notepad", grant: GRANT });
    // Keep the asynchronous bridge flush real while controlling only the TTL.
    const nativeTimeout = setTimeout;
    t.mock.timers.enable({ apis: ["setTimeout"] });
    emit("assistant-voice", ev("cap-1", "preview", { transcript: "expires on time" }));
    await new Promise((resolve) => nativeTimeout(resolve, 0));
    t.mock.timers.tick(90_000);
    answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
    answers.set("assistant_insert_text", { status: "kept", reason: "busy" });
    await voiceInsert();
    assert.equal(Voice.insert.status, "ready");
    t.mock.timers.tick(29_999);
    assert.equal(Voice.event.state, "preview");
    t.mock.timers.tick(1);
    assert.equal(Voice.event, null);
    await voiceInsert();
    assert.equal(sent("assistant_insert_text").length, phase === "re-offer" ? 0 : 1);
  });
}

for (const outcome of ["kept", "done", "failed"]) {
  test(`a late ${outcome} insertion result cannot replace a newer capture`, async () => {
    await startListening();
    answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
    emit("assistant-voice", ev("cap-1", "preview", { transcript: "old words" }));
    await flush();
    const deferred = Promise.withResolvers();
    answers.set("assistant_insert_text", deferred.promise);
    const inserting = voiceInsert();
    await flush();
    voiceCancel();
    emit("assistant-voice", ev("cap-1", "idle")); // Native cancellation releases before restart.
    await startListening("cap-2");
    deferred.resolve(outcome === "kept" ? { status: "kept", reason: "busy" }
      : outcome === "done" ? { status: "done", receipt: { actionId: "act-late", outcome: "unknown" } }
      : { status: "failed", receipt: { actionId: "act-late", outcome: "failed" }, draft: { actionId: "draft-late", text: "old words" } });
    await inserting;
    assert.equal(Voice.event.captureId, "cap-2");
    assert.equal(Voice.event.state, "listening");
    assert.equal(Voice.insert, null);
    assert.deepEqual(host.drafts, []);
  });
}

test("Use in chat cannot consume the preview while insertion is pending", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "reserved words" }));
  await flush();
  const deferred = Promise.withResolvers();
  answers.set("assistant_insert_text", deferred.promise);
  const inserting = voiceInsert();
  answers.set("assistant_voice_use", { actionId: "draft-race", text: "reserved words" });
  await voiceUse();
  deferred.resolve({ status: "kept", reason: "busy" });
  await inserting;
  assert.equal(sent("assistant_voice_use").length, 0);
  assert.equal(Voice.event.state, "preview");
  assert.equal(Voice.insert.status, "ready");
  assert.deepEqual(host.drafts, []);
});

test("a kept result cannot restore insert state after the native preview disappears", async () => {
  await startListening();
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  emit("assistant-voice", ev("cap-1", "preview", { transcript: "old words" }));
  await flush();
  const deferred = Promise.withResolvers();
  answers.set("assistant_insert_text", deferred.promise);
  const inserting = voiceInsert();
  await flush();
  emit("assistant-voice", ev("cap-1", "idle"));
  deferred.resolve({ status: "kept", reason: "busy" });
  await inserting;
  assert.equal(Voice.event, null);
  assert.equal(Voice.insert, null);
  assert.equal(State.mode, "compact");
});

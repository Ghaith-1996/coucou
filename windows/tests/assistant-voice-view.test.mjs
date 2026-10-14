// Render the real dictation card and drive its buttons/keyboard through the
// controller. Only DOM APIs and the native IPC boundary are simulated.
import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";

installFakeDom();
// The shared fake implements class selectors; this card also looks up spans.
const createElement = document.createElement;
document.createElement = (tag) => {
  const element = createElement(tag);
  const querySelector = element.querySelector.bind(element);
  element.querySelector = (selector) => selector === "span" ? element.find("span")[0] ?? null : querySelector(selector);
  return element;
};
const listeners = new Map();
window.addEventListener = (name, fn) => listeners.set(name, [...listeners.get(name) ?? [], fn]);
const { State } = await import("../src/core/state.ts");
const { CONTRACT_VERSION } = await import("../src/assistant/contracts.ts");
const { Voice, registerVoice, resetVoiceForTests, voiceCopy, voiceShortcut } = await import("../src/assistant/voice.ts");
const { buildVoice } = await import("../src/assistant/voice-view.ts");
const answers = new Map();
const copied = [];
Object.defineProperty(navigator, "clipboard", { configurable: true, value: {
  async writeText(text) { copied.push(text); },
} });
const invoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  await invoke(cmd, args);
  const answer = answers.get(cmd);
  if (answer instanceof Error) throw answer;
  return answer ?? null;
};
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const GRANT = { actionId: "act-insert", digest: "ab".repeat(32), expiresAtMs: 9_000_000_000_000 };
const host = {
  alert(view) { State.view = view; State.mode = "expanded"; State.notify(); },
  collapse() { State.mode = "compact"; State.notify(); },
  emote() {}, takeKeyboard() {}, applyPills() { return false; },
  setPinned(on) { State.isPinned = on; State.notify(); },
  showChatDraft() { return true; },
};
const view = buildVoice();
registerVoice(host);
State.subscribe(() => view.sync());
beforeEach(() => {
  resetVoiceForTests();
  // Reattach the host without accumulating guards between cases.
  const subscribe = State.subscribe;
  State.subscribe = () => () => {};
  registerVoice(host);
  State.subscribe = subscribe;
  calls.length = 0;
  answers.clear();
  copied.length = 0;
  answers.set("assistant_voice_toggle", "cap-1");
  State.pendingApproval = null;
  State.view = "overview";
  State.mode = "compact";
  State.isPinned = false;
});
afterEach(resetVoiceForTests);
const button = (label) => view.el.find("button").find((el) => el.find("span")[0]?.textContent === label);
const labels = () => view.el.find("button").map((el) => el.find("span")[0]?.textContent);
function enter(repeat = false) {
  for (const handler of listeners.get("keydown") ?? []) handler({ key: "Enter", repeat, preventDefault() {} });
}
async function preview(offer) {
  answers.set("assistant_insert_offer", offer);
  await voiceShortcut();
  emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "cap-1", state: "preview", transcript: "keep these words" });
  await flush();
}

for (const reason of ["focus-moved", "busy"]) {
  test(`an initial ${reason} refusal renders Retry before the separately named Insert`, async () => {
    await preview({ status: "refused", reason });
    assert.deepEqual(labels(), ["Retry", "Use in chat", "Cancel"]);
    assert.equal(button("Insert into Notepad"), undefined);
    const deferred = Promise.withResolvers();
    answers.set("assistant_insert_offer", deferred.promise);
    button("Retry").fire("click");
    assert.equal(button("Retry").disabled, true);
    button("Retry").fire("click");
    enter();
    await flush();
    assert.equal(sent("assistant_insert_offer").length, 2);
    assert.equal(sent("assistant_insert_text").length, 0);
    deferred.resolve({ status: "ready", label: "Notepad", grant: GRANT });
    await flush();
    assert.deepEqual(labels(), ["Insert into Notepad", "Use in chat", "Cancel"]);
    assert.equal(sent("assistant_insert_text").length, 0);
    answers.set("assistant_insert_text", { status: "done", receipt: { actionId: GRANT.actionId, outcome: "unknown" } });
    button("Insert into Notepad").fire("click");
    await flush();
    assert.deepEqual(sent("assistant_insert_text"), [{ captureId: "cap-1", grant: GRANT }]);
    assert.equal(Voice.event, null);
  });
}

test("Enter rechecks a refused field but a held key cannot confirm the newly named Insert", async () => {
  await preview({ status: "refused", reason: "busy" });
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  enter();
  await flush();
  assert.equal(sent("assistant_insert_offer").length, 2);
  assert.ok(button("Insert into Notepad"));
  enter(true);
  await flush();
  assert.equal(sent("assistant_insert_text").length, 0);
  assert.equal(sent("assistant_voice_use").length, 0);
});

for (const reason of ["focus-moved", "busy", "gone", "protected", "read-only", "unsupported"]) {
  test(`a kept ${reason} refusal renders only safe actions`, async () => {
    await preview({ status: "ready", label: "Notepad", grant: GRANT });
    const pending = Promise.withResolvers();
    answers.set("assistant_insert_text", pending.promise);
    button("Insert into Notepad").fire("click");
    assert.equal(button("Insert into Notepad").disabled, true);
    button("Insert into Notepad").fire("click");
    enter();
    await flush();
    assert.equal(sent("assistant_insert_text").length, 1);
    pending.resolve({ status: "kept", reason });
    await flush();
    const retryable = reason === "focus-moved" || reason === "busy";
    assert.deepEqual(labels(), retryable ? ["Insert into Notepad", "Use in chat", "Cancel"]
      : ["Use in chat", "Cancel"]);
    assert.equal(Voice.event.transcript, "keep these words");
    assert.equal(State.mode, "expanded");
    assert.equal(State.isPinned, true);
    assert.equal(sent("assistant_insert_text").length, 1);
  });
}

test("pending insertion disables Use in chat while leaving Cancel available", async () => {
  await preview({ status: "ready", label: "Notepad", grant: GRANT });
  const pending = Promise.withResolvers();
  answers.set("assistant_insert_text", pending.promise);
  button("Insert into Notepad").fire("click");
  const useWasDisabled = button("Use in chat").disabled;
  const cancelWasDisabled = Boolean(button("Cancel").disabled);
  button("Use in chat").fire("click");
  await flush();
  const uses = sent("assistant_voice_use").length;
  pending.resolve({ status: "kept", reason: "busy" });
  await flush();
  assert.equal(useWasDisabled, true);
  assert.equal(cancelWasDisabled, false);
  assert.equal(uses, 0);
  assert.equal(button("Use in chat").disabled, false);
});


for (const reason of ["focus-moved", "busy", "gone", "protected", "read-only", "unsupported"]) {
  for (const copyAllowed of [undefined, false, "true"]) {
    test(`${reason} with unverified copy eligibility ${copyAllowed} never copies`, async () => {
      await preview({ status: "refused", reason, copyAllowed });
      assert.equal(button("Copy"), undefined);
      assert.ok(button("Use in chat"));
      assert.ok(button("Cancel"));
      await voiceCopy();
      assert.deepEqual(copied, [], "the handler fails closed independently of the view");
    });
  }
}

for (const reason of ["focus-moved", "read-only", "unsupported"]) {
  test(`a natively checked non-protected ${reason} field can be copied`, async () => {
    await preview({ status: "refused", reason, copyAllowed: true });
    assert.ok(button("Copy"));
    button("Copy").fire("click");
    await flush();
    assert.deepEqual(copied, ["keep these words"]);
    assert.equal(sent("assistant_insert_text").length, 0);
    assert.equal(Voice.event.state, "preview");
  });
}

test("even a contradictory protected reply cannot enable Copy", async () => {
  await preview({ status: "refused", reason: "protected", copyAllowed: true });
  assert.equal(button("Copy"), undefined);
  await voiceCopy();
  assert.deepEqual(copied, []);
});

for (const nextReason of ["protected", "busy", "unsupported"]) {
  test(`a retained Copy button cannot reuse eligibility during re-offer or after ${nextReason}`, async () => {
    await preview({ status: "refused", reason: "focus-moved", copyAllowed: true });
    const staleCopy = button("Copy");
    const pending = Promise.withResolvers();
    answers.set("assistant_insert_offer", pending.promise);
    button("Retry").fire("click");
    assert.equal(button("Copy"), undefined, "fresh classification is still pending");
    staleCopy.fire("click");
    await voiceCopy();
    await flush();
    assert.deepEqual(copied, []);
    pending.resolve({ status: "refused", reason: nextReason, copyAllowed: false });
    await flush();
    assert.equal(button("Copy"), undefined);
    staleCopy.fire("click");
    await voiceCopy();
    await flush();
    assert.deepEqual(copied, []);
    assert.ok(button("Use in chat"));
  });
}

test("a retained Copy button cannot copy during insertion or reuse its old check after a kept refusal", async () => {
  await preview({ status: "refused", reason: "focus-moved", copyAllowed: true });
  const staleCopy = button("Copy");
  answers.set("assistant_insert_offer", { status: "ready", label: "Notepad", grant: GRANT });
  button("Retry").fire("click");
  await flush();
  const pending = Promise.withResolvers();
  answers.set("assistant_insert_text", pending.promise);
  button("Insert into Notepad").fire("click");
  staleCopy.fire("click");
  await voiceCopy();
  await flush();
  assert.deepEqual(copied, []);
  pending.resolve({ status: "kept", reason: "unsupported", copyAllowed: false });
  await flush();
  assert.equal(button("Copy"), undefined);
  staleCopy.fire("click");
  await voiceCopy();
  await flush();
  assert.deepEqual(copied, []);
});

for (const reason of ["read-only", "unsupported"]) {
  test(`a kept ${reason} refusal needs its own positive classification to allow Copy`, async () => {
    await preview({ status: "ready", label: "Notepad", grant: GRANT });
    answers.set("assistant_insert_text", { status: "kept", reason, copyAllowed: true });
    button("Insert into Notepad").fire("click");
    await flush();
    assert.ok(button("Copy"));
    await voiceCopy();
    assert.deepEqual(copied, ["keep these words"]);
  });
}

test("a new capture cannot inherit Copy eligibility or an old button", async () => {
  await preview({ status: "refused", reason: "unsupported", copyAllowed: true });
  const staleCopy = button("Copy");
  button("Cancel").fire("click");
  answers.set("assistant_voice_toggle", "cap-2");
  const pending = Promise.withResolvers();
  answers.set("assistant_insert_offer", pending.promise);
  await voiceShortcut();
  emit("assistant-voice", { v: CONTRACT_VERSION, captureId: "cap-2", state: "preview", transcript: "new words" });
  staleCopy.fire("click");
  await voiceCopy();
  await flush();
  assert.deepEqual(copied, []);
  pending.resolve({ status: "refused", reason: "busy", copyAllowed: false });
  await flush();
  staleCopy.fire("click");
  await voiceCopy();
  await flush();
  assert.deepEqual(copied, []);
});


for (const offer of [null, new Error("UIA unavailable")]) {
  test(`an absent or failed native offer does not allow direct Copy: ${offer}`, async () => {
    await preview(offer);
    assert.equal(button("Copy"), undefined);
    await voiceCopy();
    assert.deepEqual(copied, []);
  });
}

for (const reason of ["protected", "busy", "unsupported"]) {
  test(`Use in chat remains an explicit unsent fallback after ${reason}`, async () => {
    await preview({ status: "refused", reason, copyAllowed: false });
    answers.set("assistant_voice_use", { actionId: "draft-safe", text: "keep these words" });
    button("Use in chat").fire("click");
    await flush();
    assert.deepEqual(sent("assistant_voice_use"), [{ captureId: "cap-1" }]);
    assert.equal(sent("assistant_insert_text").length, 0);
    assert.deepEqual(copied, []);
    assert.equal(Voice.event, null);
  });
}

// The chat's action cards (src/assistant/chat-actions.ts): a card appears
// for what Rust proposes, "Do it" hands back exactly its grant, "No" hands
// back nothing, a card answers once, and it goes when Rust stops waiting.

import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { calls, emit, internals, sent } from "./tauri.mjs";
import { ChatActions, answerCard, beginToolNotes, clearToolNotes, registerChatActions, resetChatActionsForTests, retireActionCards } from "../src/assistant/chat-actions.ts";

import { State } from "../src/core/state.ts";
import { Bridge } from "../src/core/bridge.ts";

let fail = false;
const realInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  await realInvoke(cmd, args);
  if (cmd === "chat_action_answer" && fail) throw new Error("expired");
  return null;
};

const GRANT = { actionId: "act-50", digest: "ef".repeat(32), expiresAtMs: 9e12 };
let requestId;
const card = (id = "act-50") => ({ requestId, actionId: id, kind: "switch-tab", label: "YouTube", detail: "Chrome", grant: { ...GRANT, actionId: id } });
const flush = () => new Promise((r) => setTimeout(r, 0));

let registered = false;
beforeEach(() => {
  resetChatActionsForTests();
  if (!registered) {
    registerChatActions();
    registered = true;
  }
  calls.length = 0;
  fail = false;
  State.chatHistory = [];
  requestId = beginToolNotes();
});

test("Do it hands back exactly the card's grant, once", async () => {
  emit("chat-action", card());
  assert.equal(ChatActions.cards.length, 1);
  answerCard("act-50", true);
  answerCard("act-50", true);
  await flush();
  assert.deepEqual(sent("chat_action_answer"), [{ actionId: "act-50", grant: GRANT }]);
  assert.equal(ChatActions.cards[0].answered, true);
  emit("chat-action-closed", "act-50");
  assert.equal(ChatActions.cards.length, 0);
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("approval_")).length, 0, "never the agents' permissions");
});

test("No hands back no grant", async () => {
  emit("chat-action", card());
  answerCard("act-50", false);
  await flush();
  assert.deepEqual(sent("chat_action_answer"), [{ actionId: "act-50", grant: null }]);
});

test("a card Rust stopped waiting for goes away", async () => {
  emit("chat-action", card());
  emit("chat-action-closed", "act-50");
  assert.equal(ChatActions.cards.length, 0);
  answerCard("act-50", true);
  await flush();
  assert.equal(sent("chat_action_answer").length, 0);
});

test("a refused answer removes the card", async () => {
  fail = true;
  emit("chat-action", card());
  answerCard("act-50", true);
  await flush();
  assert.equal(ChatActions.cards.length, 0);
});

test("the same card is shown once; read-only tools leave a note until the next question", () => {
  emit("chat-action", card());
  emit("chat-action", card());
  assert.equal(ChatActions.cards.length, 1);
  const requestId = beginToolNotes();
  emit("chat-tool-used", { requestId, tool: "browser-tabs" });
  emit("chat-tool-used", { requestId, tool: "browser-tabs" });
  assert.deepEqual(ChatActions.used, ["browser-tabs"]);
  clearToolNotes();
  assert.deepEqual(ChatActions.used, []);
});


test("only the matching request and transcript can add tool notes", () => {
  const oldRequest = beginToolNotes();
  const requestId = beginToolNotes();
  assert.notEqual(requestId, oldRequest);
  for (const payload of [
    null, "browser-tabs", {}, { tool: "browser-tabs" },
    { requestId: null, tool: "browser-tabs" },
    { requestId: oldRequest, tool: "browser-tabs" },
    { requestId, tool: "unknown" },
  ]) {
    emit("chat-tool-used", payload);
    assert.deepEqual(ChatActions.used, [], "unscoped, stale or malformed events do not claim a tool was used");
  }
  State.chatHistory = [];
  emit("chat-tool-used", { requestId, tool: "browser-tabs" });
  assert.deepEqual(ChatActions.used, [], "replacing the transcript also invalidates an otherwise matching request");
  const currentRequest = beginToolNotes();
  emit("chat-tool-used", { requestId: currentRequest, tool: "browser-tabs" });
  assert.deepEqual(ChatActions.used, ["browser-tabs"]);
});

for (const alreadyUsed of [false, true]) {
  test(`clearing tool notes retires their request ${alreadyUsed ? "with an existing note" : "before the first note"}`, () => {
    const requestId = beginToolNotes();
    if (alreadyUsed) emit("chat-tool-used", { requestId, tool: "browser-tabs" });
    clearToolNotes();
    emit("chat-tool-used", { requestId, tool: "browser-tabs" });
    assert.deepEqual(ChatActions.used, []);
  });
}

test("chat send keeps request identity optional for callers without tool notes", async () => {
  await Bridge.chatSend("A legacy caller", null);
  assert.deepEqual(sent("chat_send").at(-1), { query: "A legacy caller", context: null });
});

test("only a well-formed card scoped to the current request is shown", () => {
  const oldRequest = requestId;
  requestId = beginToolNotes();
  for (const payload of [
    null, "act-50", {},
    { ...card(), requestId: undefined },
    { ...card(), requestId: null },
    { ...card(), requestId: 5 },
    { ...card(), requestId: oldRequest },
    { ...card(), actionId: "" },
    { ...card(), actionId: "  " },
    { ...card(), actionId: 5 },
    { ...card(), kind: "unknown" },
    { ...card(), label: null },
    { ...card(), detail: undefined },
    { ...card(), grant: null },
    { ...card(), grant: {} },
    { ...card(), grant: { ...GRANT, actionId: "other-action" } },
    { ...card(), grant: { ...GRANT, digest: "" } },
    { ...card(), grant: { ...GRANT, expiresAtMs: NaN } },
  ]) {
    emit("chat-action", payload);
    assert.deepEqual(ChatActions.cards, [], "unscoped, stale or malformed cards are never actionable");
  }
  emit("chat-action", card());
  assert.equal(ChatActions.cards.length, 1, "a current valid card is still shown");
});

for (const withNote of [false, true]) {
  test(`clearing notes synchronously retires pending cards ${withNote ? "with a tool note" : "before any tool note"}`, async () => {
    const oldCard = card();
    emit("chat-action", oldCard);
    if (withNote) emit("chat-tool-used", { requestId, tool: "browser-tabs" });
    clearToolNotes();
    assert.deepEqual(ChatActions.cards, []);
    assert.deepEqual(ChatActions.used, []);
    emit("chat-action", oldCard);
    assert.deepEqual(ChatActions.cards, [], "late events cannot resurrect a retired request");
    requestId = beginToolNotes();
    emit("chat-action", oldCard);
    answerCard(oldCard.actionId, true);
    await flush();
    assert.deepEqual(ChatActions.cards, [], "a newer answer does not make the old request live again");
    assert.deepEqual(sent("chat_action_answer"), []);
    emit("chat-action", card("new-action"));
    answerCard("new-action", true);
    await flush();
    assert.deepEqual(sent("chat_action_answer"), [{ actionId: "new-action", grant: { ...GRANT, actionId: "new-action" } }]);
  });
}

test("a fresh answer retires existing cards even in the same transcript", async () => {
  const oldCard = card();
  emit("chat-action", oldCard);
  requestId = beginToolNotes();
  assert.deepEqual(ChatActions.cards, []);
  emit("chat-action", oldCard);
  answerCard(oldCard.actionId, true);
  await flush();
  assert.deepEqual(ChatActions.cards, []);
  assert.deepEqual(sent("chat_action_answer"), []);
});

test("starting a request cannot revive ownership cancelled during its notification", (t) => {
  emit("chat-action", card());
  t.after(State.subscribe(() => clearToolNotes()));
  requestId = beginToolNotes();
  emit("chat-action", card("new-action"));
  assert.deepEqual(ChatActions.cards, []);
});

test("a replaced transcript rejects late cards and invalidates an existing click", async () => {
  emit("chat-action", card());
  State.chatHistory = [];
  emit("chat-action", card("late-action"));
  assert.equal(ChatActions.cards.some((c) => c.actionId === "late-action"), false);
  answerCard("act-50", true);
  await flush();
  assert.deepEqual(sent("chat_action_answer"), []);
});

test("retiring action cards keeps existing notes but rejects all late request events", async () => {
  const oldCard = card();
  emit("chat-action", oldCard);
  emit("chat-tool-used", { requestId, tool: "browser-tabs" });
  retireActionCards();
  assert.deepEqual(ChatActions.cards, []);
  assert.deepEqual(ChatActions.used, ["browser-tabs"]);
  emit("chat-action", oldCard);
  answerCard(oldCard.actionId, true);
  await flush();
  assert.deepEqual(ChatActions.cards, []);
  assert.deepEqual(sent("chat_action_answer"), []);
  requestId = beginToolNotes();
  emit("chat-action", oldCard);
  emit("chat-tool-used", { requestId: oldCard.requestId, tool: "browser-tabs" });
  assert.deepEqual(ChatActions.cards, []);
  assert.deepEqual(ChatActions.used, []);
});

for (const cancel of ["clear", "retire", "new answer", "replace transcript", "close"]) {
  test(`a click rechecks ownership after synchronous ${cancel} during notification`, async (t) => {
    emit("chat-action", card());
    let cancelled = false;
    t.after(State.subscribe(() => {
      if (cancelled) return;
      cancelled = true;
      if (cancel === "clear") clearToolNotes();
      if (cancel === "retire") retireActionCards();
      if (cancel === "new answer") requestId = beginToolNotes();
      if (cancel === "replace transcript") State.chatHistory = [];
      if (cancel === "close") emit("chat-action-closed", "act-50");
    }));
    answerCard("act-50", true);
    await flush();
    assert.deepEqual(sent("chat_action_answer"), [], "a retired click cannot submit its grant after a reentrant reset");
  });
}

test("a spoken answer shares its request identity with speech and its action cards", () => {
  const previous = requestId;
  const spoken = "audio-answer-identity";
  assert.equal(beginToolNotes(spoken), spoken);
  emit("chat-action", { ...card("old-card"), requestId: previous });
  emit("chat-action", { ...card("spoken-card"), requestId: spoken });
  assert.deepEqual(ChatActions.cards.map(card => card.actionId), ["spoken-card"]);
  emit("chat-tool-used", { requestId: spoken, tool: "browser-tabs" });
  assert.deepEqual(ChatActions.used, ["browser-tabs"]);
});

// The chat's action cards (src/assistant/chat-actions.ts): a card appears
// for what Rust proposes, "Do it" hands back exactly its grant, "No" hands
// back nothing, a card answers once, and it goes when Rust stops waiting.

import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { calls, emit, internals, sent } from "./tauri.mjs";
import { ChatActions, answerCard, clearToolNotes, registerChatActions, resetChatActionsForTests } from "../src/assistant/chat-actions.ts";

let fail = false;
const realInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  await realInvoke(cmd, args);
  if (cmd === "chat_action_answer" && fail) throw new Error("expired");
  return null;
};

const GRANT = { actionId: "act-50", digest: "ef".repeat(32), expiresAtMs: 9e12 };
const card = (id = "act-50") => ({ actionId: id, kind: "switch-tab", label: "YouTube", detail: "Chrome", grant: { ...GRANT, actionId: id } });
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
  emit("chat-tool-used", "browser-tabs");
  emit("chat-tool-used", "browser-tabs");
  assert.deepEqual(ChatActions.used, ["browser-tabs"]);
  clearToolNotes();
  assert.deepEqual(ChatActions.used, []);
});

test("a memory the chat proposes is a card like any other: nothing is saved before Do it", async () => {
  emit("chat-action", { ...card("act-60"), kind: "memory-save", label: "Prefers answers in French", detail: "memory" });
  assert.equal(ChatActions.cards[0].label, "Prefers answers in French");
  assert.equal(calls.filter(([cmd]) => cmd.startsWith("memory_")).length, 0, "the page never saves a memory itself");
  answerCard("act-60", false);
  await flush();
  assert.deepEqual(sent("chat_action_answer"), [{ actionId: "act-60", grant: null }]);
});

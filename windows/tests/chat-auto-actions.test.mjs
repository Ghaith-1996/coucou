// "Do it without asking" (assistant/tools.rs): Rust runs some actions with no
// card and only reports what came of them. The island shows those reports as
// notes for the answer that owns them, never as buttons, and a page handed to
// the browser is never said to have opened.

import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { calls, emit } from "./tauri.mjs";
import { ChatActions, beginToolNotes, clearToolNotes, registerChatActions, resetChatActionsForTests } from "../src/assistant/chat-actions.ts";
import { State } from "../src/core/state.ts";
import { autoNoteText } from "../src/views/chat.ts";
import { t } from "../src/i18n/i18n.ts";

let requestId;
let registered = false;
beforeEach(() => {
  resetChatActionsForTests();
  if (!registered) {
    registerChatActions();
    registered = true;
  }
  calls.length = 0;
  State.chatHistory = [];
  requestId = beginToolNotes();
});

const ran = (fields) => ({ requestId, tool: "auto-action", kind: "launch-app", label: "Google Chrome", outcome: "succeeded", ...fields });

test("an action that ran without a card is a note, never a card or a call", () => {
  emit("chat-tool-used", ran());
  assert.deepEqual(ChatActions.auto, [{ kind: "launch-app", label: "Google Chrome", outcome: "succeeded" }]);
  assert.equal(ChatActions.cards.length, 0);
  assert.equal(calls.length, 0, "the page answers nothing back");
  clearToolNotes();
  assert.deepEqual(ChatActions.auto, [], "the next question starts without them");
});

test("a malformed or foreign report adds nothing", () => {
  for (const event of [
    ran({ kind: "delete-files" }),
    ran({ kind: undefined }),
    ran({ label: 42 }),
    ran({ outcome: "maybe" }),
    ran({ requestId: "someone-else" }),
    ran({ requestId: null }),
    { requestId, tool: "something-new", kind: "launch-app", label: "x", outcome: "succeeded" },
  ]) emit("chat-tool-used", event);
  assert.deepEqual(ChatActions.auto, []);
  assert.deepEqual(ChatActions.used, []);
});

test("tab reads and automatic actions are kept apart", () => {
  emit("chat-tool-used", { requestId, tool: "browser-tabs" });
  emit("chat-tool-used", ran({ kind: "switch-tab", label: "YouTube" }));
  assert.deepEqual(ChatActions.used, ["browser-tabs"]);
  assert.equal(ChatActions.auto.length, 1);
});

test("each note says what came of the action, in its own words", () => {
  const text = (kind, outcome) => t(autoNoteText({ kind, label: "X", outcome }), { label: "X" });
  assert.equal(text("launch-app", "succeeded"), "Opened X");
  assert.equal(text("launch-app", "unknown"), "Started X");
  assert.equal(text("launch-app", "failed"), "Couldn't open X");
  assert.equal(text("switch-tab", "succeeded"), "Switched to X");
  assert.equal(text("switch-tab", "unknown"), "Couldn't confirm the switch to X");
  assert.equal(text("switch-tab", "cancelled"), "Couldn't switch to X");
  assert.equal(text("memory-save", "succeeded"), "Remembered: X");
  assert.equal(text("memory-save", "failed"), "Couldn't remember: X");
  assert.equal(text("open-url", "failed"), "Couldn't open X");
  for (const outcome of ["succeeded", "unknown"]) {
    assert.equal(text("open-url", outcome), "Sent X to your browser", "Windows can't tell whether the page loaded");
  }
});

// Snapshot revisions through the real view/bridge. Native chat_store tests
// separately force delayed writes and deletion races against the filesystem.
import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, internals, sent } from "./tauri.mjs";

installFakeDom();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { userAgent: "Windows" } });
const { buildPrompt, newConversation } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
const { Bridge } = await import("../src/core/bridge.ts");
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
let records;
let view;
let reply;
let rejectSave;
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  if (cmd === "chat_history_save") {
    if (rejectSave) throw new Error("Save unavailable");
    records.set(args.conversation.id, structuredClone(args.conversation));
  }
  if (cmd === "chat_history_list") {
    return [...records.values()].map(({ id, title, updatedAt, messages }) => ({
      id, title, updatedAt, count: messages.length,
    }));
  }
  if (cmd === "chat_history_open") return structuredClone(records.get(args.id));
  if (cmd === "chat_undo_last") return true;
  if (cmd === "chat_send") return { text: `Answer ${++reply}` };
  return result;
};

beforeEach(async () => {
  records = new Map();
  reply = 0;
  rejectSave = false;
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.stateOverride = null;
  State.view = "prompt";
  newConversation();
  await Bridge.chatReset();
  calls.length = 0;
  view = buildPrompt(() => {});
  await flush();
  view.sync();
});

const $ = (selector) => view.el.querySelector(selector);
const snapshots = () => sent("chat_history_save").map(({ conversation }) => conversation);

async function send(query = "What next?") {
  $(".chat-input").value = query;
  $(".send-btn").fire("click");
  await flush();
  view.sync();
}

function seed(id, updatedAt) {
  records.set(id, {
    id, title: "Saved conversation", createdAt: 1, updatedAt,
    messages: [{ role: "user", content: "A saved question" }, { role: "assistant", content: "A saved answer" }],
  });
}

async function open(id) {
  if (!$(".history").classList.contains("on")) $(".history-btn").fire("click");
  await flush();
  view.el.find(".history-row").find((row) => row.getAttribute("data-id") === id).fire("click");
  await flush();
  view.sync();
}

test("same-millisecond completion and regeneration receive distinct snapshot revisions", async (t) => {
  t.mock.method(Date, "now", () => 1_000);
  await send("First question");
  const regenerate = view.el.find(".md-copy").find((button) => button.getAttribute("aria-label") === "Regenerate");
  assert.ok(regenerate);
  regenerate.fire("click");
  await flush();
  view.sync();
  await send("Second question");
  const saves = snapshots();
  assert.deepEqual(saves.map((c) => c.updatedAt), [1_000, 1_001, 1_002]);
  assert.deepEqual(saves.map((c) => c.messages.length), [2, 2, 4]);
  assert.equal(saves[0].messages[1].content, "Answer 1");
  assert.equal(saves[1].messages[1].content, "Answer 2", "regeneration may replace equal-length content");
  assert.equal(new Set(saves.map((c) => c.id)).size, 1);
});

test("reopening seeds the saved revision even when the system clock moves backward", async (t) => {
  let now = 1_000;
  t.mock.method(Date, "now", () => now);
  const id = "c-0000000000000101";
  seed(id, 5_000);
  await open(id);
  await send();
  now = 100;
  await send();
  assert.deepEqual(snapshots().map((c) => c.updatedAt), [5_001, 5_002]);
  assert.equal(records.get(id).createdAt, 1);
});

test("revision floors belong to each conversation and never regress after reopening", async (t) => {
  t.mock.method(Date, "now", () => 1_000);
  const first = "c-0000000000000102";
  const second = "c-0000000000000103";
  seed(first, 5_000);
  seed(second, 20);
  await open(first);
  await send();
  await open(second);
  await send();
  records.get(first).updatedAt = 4_000; // An older externally restored transcript.
  await open(first);
  await send();
  assert.deepEqual(snapshots().map((c) => [c.id, c.updatedAt]), [
    [first, 5_001], [second, 1_000], [first, 5_002],
  ]);
});

test("a failed fire-and-forget save still consumes its snapshot revision", async (t) => {
  t.mock.method(Date, "now", () => 1_000);
  t.mock.method(console, "error", () => {});
  rejectSave = true;
  await send("First question");
  rejectSave = false;
  await send("Second question");
  assert.deepEqual(snapshots().map((c) => c.updatedAt), [1_000, 1_001]);
  assert.equal(records.get(State.chatId).messages.length, 4);
});

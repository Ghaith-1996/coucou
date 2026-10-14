// Conversation titles through the real chat view and bridge. The native
// commands are simulated; saves deliberately write the whole received record,
// so these checks also catch a stale title sent by the view.
import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";

installFakeDom();
const { buildPrompt, newConversation } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const CURRENT = "c-0000000000000001";
const OTHER = "c-0000000000000002";
let records;
let answers;
let view;
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  if (cmd in answers) return answers[cmd](args);
  if (cmd === "chat_history_save") records.set(args.conversation.id, structuredClone(args.conversation));
  if (cmd === "chat_history_list") {
    return [...records.values()].map(({ id, title, updatedAt, messages }) => ({ id, title, updatedAt, count: messages.length }));
  }
  if (cmd === "chat_history_open") return structuredClone(records.get(args.id));
  if (cmd === "chat_history_rename") records.get(args.id).title = args.title;
  if (cmd === "chat_send") return { text: "An answer" };
  return result;
};

beforeEach(async () => {
  calls.length = 0;
  records = new Map();
  answers = {};
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.chatHistory = [];
  State.chatId = null;
  State.stateOverride = null;
  State.view = "prompt";
  State.droppedFile = null;
  State.promptContext = null;
  view = buildPrompt(() => {});
  await flush();
  view.sync();
});

const $ = (selector) => view.el.querySelector(selector);

function seed(id, title, firstQuestion = "Plan my trip") {
  records.set(id, {
    id, title, createdAt: 123, updatedAt: 456,
    messages: [{ role: "user", content: firstQuestion }, { role: "assistant", content: "Day one" }],
  });
}

async function history() {
  if (!$(".history").classList.contains("on")) $(".history-btn").fire("click");
  await flush();
}

async function open(id) {
  await history();
  const index = [...records.keys()].indexOf(id);
  view.el.find(".history-row")[index].fire("click");
  await flush();
  view.sync();
}

async function send(query = "What next?") {
  $(".chat-input").value = query;
  $(".send-btn").fire("click");
  await flush();
  view.sync();
}

async function rename(id, title) {
  await history();
  const index = [...records.keys()].indexOf(id);
  const row = view.el.find(".history-row")[index];
  row.find(".history-act")[0].fire("click");
  const input = row.querySelector(".history-rename");
  input.value = title;
  input.fire("blur");
  await flush();
}

test("new conversations derive their title from the first question only", async () => {
  await send("  Plan my trip  ");
  const id = State.chatId;
  const createdAt = records.get(id).createdAt;
  assert.equal(records.get(id).title, "Plan my trip");
  await send("A different question");
  assert.equal(records.get(id).title, "Plan my trip");
  assert.equal(records.get(id).createdAt, createdAt);
  assert.equal(records.get(id).messages.length, 4);
});

test("renaming the active conversation survives its next completed answer", async () => {
  await send("Plan my trip");
  const id = State.chatId;
  await rename(id, "  Lisbon\ttrip  ");
  assert.equal(records.get(id).title, "Lisbon trip");
  await send();
  assert.equal(records.get(id).title, "Lisbon trip");
  assert.equal(records.get(id).messages.length, 4);
});

for (const finish of ["reply", "Stop", "Regenerate"]) {
  test(`a reopened custom title survives ${finish}`, async () => {
    seed(CURRENT, "Lisbon trip");
    await open(CURRENT);
    if (finish === "Stop") {
      let reject;
      answers.chat_send = () => new Promise((_, no) => { reject = no; });
      answers.chat_stop = () => reject(new Error("Stopped."));
      await send();
      emit("chat-delta", "A partial answer");
      $(".send-btn").fire("click");
      await flush();
      assert.equal(records.get(CURRENT).messages.at(-1).stopped, true);
    } else if (finish === "Regenerate") {
      view.el.find(".md-copy").find((button) => button.getAttribute("aria-label") === "Regenerate").fire("click");
      await flush();
      assert.equal(records.get(CURRENT).messages.at(-1).content, "An answer");
    } else {
      await send();
      assert.equal(records.get(CURRENT).messages.length, 4);
    }
    assert.equal(records.get(CURRENT).title, "Lisbon trip");
    assert.equal(records.get(CURRENT).createdAt, 123);
  });
}

test("renaming another conversation leaves the active conversation's title alone", async () => {
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Other plans");
  await open(CURRENT);
  await rename(OTHER, "Paris trip");
  await send();
  assert.equal(records.get(CURRENT).title, "Lisbon trip");
  assert.equal(records.get(OTHER).title, "Paris trip");
  await open(OTHER);
  await send();
  assert.equal(records.get(OTHER).title, "Paris trip");
});

test("starting a new conversation does not inherit the previous custom title", async () => {
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  newConversation();
  view.sync();
  await send("A fresh question");
  assert.notEqual(State.chatId, CURRENT);
  assert.equal(records.get(State.chatId).title, "A fresh question");
  assert.notEqual(records.get(State.chatId).createdAt, 123);
});

test("a delayed rename cannot change the title of a different active conversation", async (t) => {
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  await open(CURRENT);
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(); await flush(); });
  answers.chat_history_rename = async ({ id, title }) => {
    await pending.promise;
    records.get(id).title = title;
  };
  await rename(CURRENT, "Lisbon holiday");
  await open(OTHER);
  pending.resolve();
  await flush();
  await send();
  assert.equal(records.get(OTHER).title, "Paris trip");
  assert.equal(records.get(CURRENT).title, "Lisbon holiday");
});

test("a rejected rename keeps the last saved title for later answers", async () => {
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  answers.chat_history_rename = () => Promise.reject(new Error("Disk unavailable"));
  await rename(CURRENT, "Lost rename");
  await send();
  assert.equal(records.get(CURRENT).title, "Lisbon trip");
  assert.equal(sent("chat_history_save").at(-1).conversation.title, "Lisbon trip");
});

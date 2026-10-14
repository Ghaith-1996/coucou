// Attachment eligibility belongs to Rust's committed turns, not the visible
// transcript. Reopened records contain text only and must say so in the view.
import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";

installFakeDom();
const { buildPrompt, newConversation } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
let answers;
let records;
let view;
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  if (cmd in answers) return answers[cmd](args);
  if (cmd === "chat_send") return { text: "An answer" };
  if (cmd === "chat_undo_last") return true;
  if (cmd === "chat_history_save") records.set(args.conversation.id, structuredClone(args.conversation));
  if (cmd === "chat_history_list") return [...records.values()].map(({ id, title, updatedAt, messages }) => ({ id, title, updatedAt, count: messages.length }));
  if (cmd === "chat_history_open") return structuredClone(records.get(args.id));
  return result;
};
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const $ = (selector) => view.el.querySelector(selector);
const again = () => view.el.find(".md-copy").find((b) => b.getAttribute("aria-label") === "Regenerate");
const file = (name) => ({ name, path: `/inbox/${name}` });
const context = (name) => ({ kind: "file", ...file(name) });
function pendingReply(t) {
  const pending = Promise.withResolvers();
  answers.chat_send = () => pending.promise;
  t.after(async () => { pending.resolve({ text: "Late answer" }); await flush(); });
  return pending;
}
async function send(query = "Describe it") {
  $(".chat-input").value = query;
  $(".send-btn").fire("click");
  await flush();
  view.sync();
}
async function open(id) {
  $(".history-btn").fire("click");
  await flush();
  view.el.find(".history-row")[[...records.keys()].indexOf(id)].fire("click");
  await flush();
  view.sync();
}
beforeEach(async () => {
  answers = {};
  records = new Map();
  newConversation();
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.stateOverride = null;
  State.noteMessage = null;
  State.view = "prompt";
  calls.length = 0;
  view = buildPrompt(() => {});
  await flush();
  view.sync();
});

for (const name of ["picture.png", "notes.txt"]) {
  for (const partial of [false, true]) {
    test(`${name}: Stop ${partial ? "with" : "without"} a partial answer preserves the file candidate for the first completed turn`, async (t) => {
      State.droppedFile = file(name);
      const pending = pendingReply(t);
      answers.chat_stop = () => pending.reject(new Error("Stopped."));
      await send();
      if (partial) emit("chat-delta", "Partial answer");
      $(".send-btn").fire("click");
      await flush();
      view.sync();
      assert.equal(State.chatHistory.length, partial ? 2 : 1);
      if (partial) assert.equal(State.chatHistory.at(-1).stopped, true);
      delete answers.chat_send;
      await send("Try again");
      assert.deepEqual(sent("chat_send").map(({ context }) => context), [context(name), context(name)]);
      assert.equal(State.chatHistory.at(-1).content, "An answer");
      await send("What else?");
      assert.deepEqual(sent("chat_send").at(-1).context, context(name), "native committed-turn state decides whether to use the candidate");
    });
  }

  test(`${name}: failure and retry retain the file without persisting its path or bytes`, async () => {
    State.droppedFile = file(name);
    answers.chat_send = () => Promise.reject(new Error("Model unavailable"));
    await send();
    assert.equal(State.view, "note");
    delete answers.chat_send;
    State.view = "prompt";
    await send("Try again");
    assert.deepEqual(sent("chat_send").at(-1).context, context(name));
    const saved = JSON.stringify(records.get(State.chatId));
    assert.ok(!saved.includes(name));
    assert.ok(!saved.includes("/inbox/"));
  });

  test(`${name}: reopening clearly discloses text-only context and never recreates the dropped file`, async () => {
    State.droppedFile = file(name);
    await send();
    const id = State.chatId;
    assert.equal($(".chat-restored"), null);
    await open(id);
    assert.equal(State.droppedFile, null);
    assert.equal($(".chip"), null);
    assert.match($(".chat-restored")?.textContent ?? "", /text only/i);
    assert.match($(".chat-restored")?.textContent ?? "", /original attachments.*unavailable/i);
    assert.match($(".chat-restored")?.textContent ?? "", /drop.*new chat/i);
    await send("What else is in it?");
    assert.equal(sent("chat_send").at(-1).context, null);
    assert.ok($(".chat-restored"), "the notice persists after an answer and save");
    again().fire("click");
    await flush();
    view.sync();
    assert.ok($(".chat-restored"), "regeneration does not hide the missing attachment");
    await open(id);
    assert.ok($(".chat-restored"), "the limitation is still visible on the next reopen");
  });
}

test("legacy text-only records get the same honest notice, alongside any save failure", async () => {
  const id = "c-0000000000000001";
  records.set(id, { id, title: "Old chat", createdAt: 1, updatedAt: 2, messages: [{ role: "user", content: "Describe it" }, { role: "assistant", content: "A picture" }] });
  await open(id);
  assert.ok($(".chat-restored"), "old records do not identify whether a file was attached");
  answers.chat_history_save = () => Promise.reject(new Error("Disk full"));
  await send();
  assert.match($(".chat-notice.warn")?.textContent ?? "", /Disk full/);
  assert.ok($(".chat-restored"), "a save warning must not conceal the context limitation");
});

for (const reset of ["new chat", "file drop"]) {
  test(`${reset} clears the reopened-text notice and uses only the new conversation's file`, async () => {
    State.droppedFile = file("old.png");
    await send();
    await open(State.chatId);
    assert.ok($(".chat-restored"));
    if (reset === "new chat") newConversation();
    else {
      // The island's drop lifecycle replaces both transcript and identity.
      State.chatHistory = [];
      State.chatId = null;
      State.droppedFile = file("new.txt");
    }
    view.sync();
    assert.equal($(".chat-restored"), null);
    await send("New question");
    assert.equal($(".chat-restored"), null);
    assert.deepEqual(sent("chat_send").at(-1).context, reset === "new chat" ? null : context("new.txt"));
  });
}

test("closing the picker during a delayed reopen still applies the native restored conversation", async (t) => {
  State.droppedFile = file("old.png");
  await send();
  const saved = {
    id: "c-0000000000000001", title: "Another saved chat", createdAt: 1, updatedAt: 2,
    messages: [{ role: "user", content: "A different question" }, { role: "assistant", content: "A different answer" }],
  };
  records.set(saved.id, saved);
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(saved); await flush(); });
  answers.chat_history_open = () => pending.promise;
  await open(saved.id);
  // Native open restores the model history before its IPC reply. Closing its
  // picker is not a cancellation of that already-started command.
  $(".history-btn").fire("click");
  pending.resolve(saved);
  await flush();
  view.sync();
  assert.equal(State.chatId, saved.id);
  assert.deepEqual(State.chatHistory.map(({ content }) => content), saved.messages.map(({ content }) => content));
  assert.ok($(".chat-restored"));
  assert.equal(State.droppedFile, null);
});

test("a reset while a reply is pending releases its thinking override when that reply ends", async (t) => {
  State.droppedFile = file("old.png");
  const pending = pendingReply(t);
  await send();
  State.chatHistory = [];
  State.chatId = null;
  State.droppedFile = file("new.txt");
  pending.resolve({ text: "Old answer" });
  await flush();
  view.sync();
  assert.equal(State.stateOverride, null);
  assert.deepEqual(State.chatHistory, []);
  delete answers.chat_send;
  await send("New question");
  assert.deepEqual(sent("chat_send").at(-1).context, context("new.txt"));
});

test("a stale usage-limit event cannot warn in a replacement chat", async (t) => {
  State.settings = { ...State.settings, chatProvider: "chatgpt" };
  const pending = pendingReply(t);
  await send();
  newConversation();
  emit("chatgpt-usage-limit", null);
  pending.reject(new Error("Old usage limit"));
  await flush();
  view.sync();
  assert.equal($(".chat-notice.warn")?.textContent ?? "", "");
  assert.equal(State.view, "prompt");
});

for (const outcome of ["reply", "error", "Stop"]) {
  test(`a stale ${outcome} after a file-drop reset cannot save an old attachment's answer in the new chat`, async (t) => {
    State.droppedFile = file("old.png");
    const pending = pendingReply(t);
    await send();
    if (outcome === "Stop") $(".send-btn").fire("click");
    State.chatHistory = [];
    State.chatId = null;
    State.droppedFile = file("new.txt");
    State.view = "uploading";
    State.stateOverride = "happy";
    view.sync();
    emit("chat-delta", "Old partial answer");
    assert.equal($(".reply"), null, "stale deltas stay out of the reset transcript");
    if (outcome === "reply") pending.resolve({ text: "Old answer" });
    else pending.reject(new Error(outcome === "Stop" ? "Stopped." : "Old failure"));
    await flush();
    view.sync();
    assert.deepEqual(State.chatHistory, []);
    assert.equal(State.chatId, null);
    assert.equal(State.view, "uploading");
    assert.equal(State.stateOverride, "happy");
    assert.equal(sent("chat_history_save").length, 0);
  });
}

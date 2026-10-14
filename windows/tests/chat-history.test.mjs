// Conversation titles through the real chat view and bridge. The native
// commands are simulated; saves deliberately write the whole received record,
// so these checks also catch a stale title sent by the view.
import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";

installFakeDom();
Object.defineProperty(globalThis, "navigator", { configurable: true, value: { userAgent: "Windows" } });
const { buildPrompt, newConversation } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
const { Sound } = await import("../src/core/sound.ts");
const { Bridge } = await import("../src/core/bridge.ts");

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const CURRENT = "c-0000000000000001";
const OTHER = "c-0000000000000002";
// Match the native validation boundary rather than inventing a mock limit.
const MAX_MESSAGES = Number(readFileSync(new URL("../src-tauri/src/chat_store.rs", import.meta.url), "utf8")
  .match(/pub const MAX_MESSAGES: usize = (\d+);/)[1]);
let records;
let answers;
let view;
let nativeHistory;
let sendHistories;
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  if (cmd === "chat_send") sendHistories.push(structuredClone(nativeHistory));
  if (cmd in answers) {
    const answer = await answers[cmd](args);
    if (cmd === "chat_history_open") nativeHistory = structuredClone(answer.messages);
    if (cmd === "chat_reset") nativeHistory = [];
    return answer;
  }
  if (cmd === "chat_reset") nativeHistory = [];
  if (cmd === "chat_history_save") {
    if (args.conversation.messages.length > MAX_MESSAGES) throw new Error("too many messages");
    records.set(args.conversation.id, structuredClone(args.conversation));
  }
  if (cmd === "chat_undo_last") return true;
  if (cmd === "chat_history_list") {
    return [...records.values()].map(({ id, title, updatedAt, messages }) => ({ id, title, updatedAt, count: messages.length }));
  }
  if (cmd === "chat_history_open") {
    const saved = structuredClone(records.get(args.id));
    nativeHistory = structuredClone(saved.messages);
    return saved;
  }
  if (cmd === "chat_history_rename") records.get(args.id).title = args.title;
  if (cmd === "chat_history_delete") records.delete(args.id);
  if (cmd === "chat_send") return { text: "An answer" };
  return result;
};

beforeEach(async () => {
  records = new Map();
  answers = {};
  nativeHistory = [];
  sendHistories = [];
  await Bridge.chatReset();
  calls.length = 0;
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

// The island redraws after State.notify(), not after every button click.
function observeView(t) {
  t.after(State.subscribe(() => view.sync()));
}

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

test("New chat redraws the empty conversation and does not inherit its title or context", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  State.droppedFile = { name: "old.txt", path: "/inbox/old.txt" };
  State.promptContext = { kind: "file", ...State.droppedFile };
  State.notify();
  assert.equal(view.el.find(".chat-row").length, 2);
  assert.ok($(".chat-restored"));
  assert.ok($(".chip"));
  assert.equal($(".chat-input").placeholder, "Continue…");
  await history();
  $(".history-new").fire("click");
  assert.equal(view.el.find(".chat-row").length, 0, "the click redraws without a manual sync");
  assert.equal($(".chat-restored"), null);
  assert.equal($(".chip"), null);
  assert.equal($(".chat-input").placeholder, "Ask me anything…");
  assert.equal($(".history").classList.contains("on"), false);
  assert.deepEqual(State.chatHistory, []);
  assert.equal(State.chatId, null);
  assert.equal(State.droppedFile, null);
  assert.equal(State.promptContext, null);
  assert.equal(sent("chat_reset").length, 1);
  assert.ok(records.has(CURRENT), "New chat keeps the saved conversation");
  await send("A fresh question");
  assert.notEqual(State.chatId, CURRENT);
  assert.equal(records.get(State.chatId).title, "A fresh question");
  assert.notEqual(records.get(State.chatId).createdAt, 123);
});

for (const deleted of [CURRENT, OTHER]) {
  test(`confirmed deletion of ${deleted === CURRENT ? "the active" : "another"} conversation redraws only the affected chat`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    seed(OTHER, "Paris trip");
    await open(CURRENT);
    const messages = State.chatHistory;
    const transcript = $(".chat-log").textContent;
    await history();
    const row = view.el.find(".history-row")[[...records.keys()].indexOf(deleted)];
    const button = row.find(".history-act").find((b) => b.getAttribute("aria-label") === "Delete");
    button.fire("click");
    assert.equal(sent("chat_history_delete").length, 0, "the first click only asks for confirmation");
    assert.equal($(".chat-log").textContent, transcript);
    button.fire("click");
    await flush();
    assert.deepEqual(sent("chat_history_delete"), [{ id: deleted }]);
    assert.equal(records.has(deleted), false);
    assert.equal(view.el.find(".history-row").length, 1);
    if (deleted === CURRENT) {
      assert.equal(State.chatId, null);
      assert.deepEqual(State.chatHistory, []);
      assert.equal(view.el.find(".chat-row").length, 0, "deletion redraws without a manual sync");
      assert.equal($(".chat-restored"), null);
      assert.equal($(".chat-input").placeholder, "Ask me anything…");
      assert.equal(sent("chat_reset").length, 1);
    } else {
      assert.equal(State.chatId, CURRENT);
      assert.equal(State.chatHistory, messages);
      assert.equal($(".chat-log").textContent, transcript);
      assert.ok($(".chat-restored"));
      assert.equal($(".chat-input").placeholder, "Continue…");
      assert.equal(sent("chat_reset").length, 0);
    }
  });
}

// Native deletion is authoritative: neither errors nor older completions may
// discard a transcript that the user can still read and continue.
function deleteButton(id) {
  const row = view.el.find(".history-row")[[...records.keys()].indexOf(id)];
  return row.find(".history-act").find((button) => button.getAttribute("aria-label") === "Delete");
}

async function confirmDelete(id) {
  await history();
  const button = deleteButton(id);
  button.fire("click");
  button.fire("click");
  await flush();
  return button;
}

for (const deleted of [CURRENT, OTHER]) {
  test(`failed deletion of ${deleted === CURRENT ? "the active" : "another"} conversation preserves the chat and allows retry`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    seed(OTHER, "Paris trip");
    await open(CURRENT);
    answers.chat_history_save = () => Promise.reject(new Error("Unsaved messages"));
    await send("A question still only on screen");
    State.droppedFile = { name: "itinerary.txt", path: "/inbox/itinerary.txt" };
    State.promptContext = { kind: "file", ...State.droppedFile };
    State.notify();
    const messages = State.chatHistory;
    const context = State.promptContext;
    const file = State.droppedFile;
    const transcript = $(".chat-log").textContent;
    const warning = $(".chat-notice.warn").textContent;
    const sounds = t.mock.method(Sound, "play", () => {});
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(); await flush(); });
    answers.chat_history_delete = () => pending.promise;
    await confirmDelete(deleted);
    assert.equal(State.chatHistory, messages, "a pending delete must not discard messages");
    assert.equal(State.promptContext, context);
    pending.reject(new Error("Disk is read-only"));
    await flush();
    assert.equal(State.chatId, CURRENT);
    assert.equal(State.chatHistory, messages);
    assert.equal(State.promptContext, context);
    assert.equal(State.droppedFile, file);
    assert.equal($(".chat-log").textContent, transcript);
    assert.equal($(".chat-notice.warn").textContent, warning);
    assert.ok($(".chat-restored"));
    assert.equal(State.view, "prompt");
    assert.equal(sent("chat_reset").length, 0);
    assert.equal(records.size, 2);
    assert.match($(".history").textContent, /Disk is read-only/);
    assert.deepEqual(sounds.mock.calls.map(({ arguments: args }) => args[0]), ["error"]);

    // Continuing after the failed deletion must retain the saved title too.
    delete answers.chat_history_save;
    await send("Keep planning");
    assert.equal(records.get(CURRENT).title, "Lisbon trip");
    delete answers.chat_history_delete;
    await history();
    const retry = deleteButton(deleted);
    retry.fire("click");
    assert.equal(sent("chat_history_delete").length, 1, "retry asks for confirmation again");
    retry.fire("click");
    await flush();
    assert.equal(sent("chat_history_delete").length, 2);
    assert.equal(records.has(deleted), false);
    assert.doesNotMatch($(".history").textContent, /Disk is read-only/);
    assert.equal(State.chatId, deleted === CURRENT ? null : CURRENT);
    assert.equal(sent("chat_reset").length, deleted === CURRENT ? 1 : 0);
  });
}

test("a failed history refresh keeps the failed deletion visible and retryable", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  answers.chat_history_delete = () => Promise.reject(new Error("Deletion unavailable"));
  await confirmDelete(CURRENT);
  assert.match($(".history").textContent, /Deletion unavailable/);
  $(".history-btn").fire("click");
  t.mock.method(console, "error", () => {});
  answers.chat_history_list = () => Promise.reject(new Error("Listing unavailable"));
  await history();
  assert.equal(view.el.find(".history-row").length, 1);
  assert.match($(".history").textContent, /Deletion unavailable/);
  delete answers.chat_history_delete;
  await confirmDelete(CURRENT);
  assert.equal(records.has(CURRENT), false);
  assert.equal(State.chatId, null);
  assert.doesNotMatch($(".history").textContent, /Deletion unavailable/);
});

test("repeated confirmation clicks cannot submit a second pending deletion", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(); await flush(); });
  answers.chat_history_delete = async ({ id }) => { await pending.promise; records.delete(id); };
  const button = await confirmDelete(CURRENT);
  button.fire("click");
  button.fire("click");
  await flush();
  assert.equal(sent("chat_history_delete").length, 1);
  assert.equal(deleteButton(CURRENT).disabled, true);
  // Reopening the panel rebuilds its rows but does not release the operation.
  $(".history-btn").fire("click");
  await history();
  const rebuilt = deleteButton(CURRENT);
  assert.equal(rebuilt.disabled, true);
  rebuilt.fire("click");
  rebuilt.fire("click");
  await flush();
  assert.equal(sent("chat_history_delete").length, 1);
  pending.resolve();
  await flush();
  assert.equal(records.has(CURRENT), false);
  assert.equal(sent("chat_reset").length, 1);
});

for (const destination of ["another chat", "a new chat", "a replaced transcript", "a continued transcript"]) {
  test(`a delayed successful deletion cannot clear ${destination}`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    seed(OTHER, "Paris trip");
    await open(CURRENT);
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(); await flush(); });
    answers.chat_history_delete = async ({ id }) => { await pending.promise; records.delete(id); };
    await confirmDelete(CURRENT);
    if (destination === "another chat") await open(OTHER);
    else if (destination === "a new chat") {
      $(".history-new").fire("click");
      await send("A new beginning");
    } else if (destination === "a continued transcript") await send("A newer question");
    else {
      State.chatHistory = [{ id: 999, role: "user", content: "A newer transcript" }];
      State.droppedFile = { name: "new.txt", path: "/inbox/new.txt" };
      State.promptContext = { kind: "file", ...State.droppedFile };
      State.notify();
    }
    const id = State.chatId;
    const messages = State.chatHistory;
    const context = State.promptContext;
    const resets = sent("chat_reset").length;
    pending.resolve();
    await flush();
    assert.equal(records.has(CURRENT), false);
    assert.equal(State.chatId, id);
    assert.equal(State.chatHistory, messages);
    assert.equal(State.promptContext, context);
    assert.equal(sent("chat_reset").length, resets);
  });
}

test("a successful deletion cannot reset native context while a newer chat is opening", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  await open(CURRENT);
  const deletion = Promise.withResolvers();
  const opening = Promise.withResolvers();
  t.after(async () => { deletion.resolve(); opening.resolve(); await flush(); });
  answers.chat_history_delete = async ({ id }) => { await deletion.promise; records.delete(id); };
  await confirmDelete(CURRENT);
  answers.chat_history_open = async ({ id }) => { await opening.promise; return structuredClone(records.get(id)); };
  await open(OTHER);
  const messages = State.chatHistory;
  deletion.resolve();
  await flush();
  assert.equal(sent("chat_reset").length, 0, "the newer native open owns the context even before its reply arrives");
  assert.equal(State.chatHistory, messages);
  opening.resolve();
  await flush();
  assert.equal(State.chatId, OTHER);
  await send("Keep planning Paris");
  assert.equal(records.get(OTHER).title, "Paris trip");
});

test("a delayed deletion failure cannot replace a newer view or its error", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(); await flush(); });
  answers.chat_history_delete = () => pending.promise;
  await confirmDelete(CURRENT);
  State.view = "note";
  State.noteMessage = "A newer error";
  State.notify();
  const messages = State.chatHistory;
  pending.reject(new Error("Old deletion failed"));
  await flush();
  assert.equal(State.view, "note");
  assert.equal(State.noteMessage, "A newer error");
  assert.equal(State.chatHistory, messages);
  assert.equal(State.chatId, CURRENT);
  assert.equal(sent("chat_reset").length, 0);
  State.view = "prompt";
  State.notify();
  await history();
  assert.match($(".history").textContent, /Old deletion failed/);
});

for (const outcome of ["resolves", "rejects"]) {
  test(`New chat redraws before native reset ${outcome}`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    await open(CURRENT);
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(); await flush(); });
    answers.chat_reset = () => pending.promise;
    const errors = t.mock.method(console, "error", () => {});
    await history();
    $(".history-new").fire("click");
    assert.equal(view.el.find(".chat-row").length, 0, "redraw cannot wait for native reset");
    assert.equal($(".chat-restored"), null);
    assert.equal($(".chat-input").placeholder, "Ask me anything…");
    if (outcome === "rejects") pending.reject(new Error("Native reset unavailable"));
    else pending.resolve();
    await flush();
    assert.equal(errors.mock.callCount(), outcome === "rejects" ? 1 : 0);
    assert.equal(State.chatId, null);
    assert.deepEqual(State.chatHistory, []);
    assert.equal(view.el.find(".chat-row").length, 0);
    assert.equal(State.view, "prompt");
  });
}

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

test("400 messages save; a rejected 402-message save warns without deleting history", async () => {
  assert.equal(MAX_MESSAGES, 400);
  seed(CURRENT, "A long conversation");
  records.get(CURRENT).messages = Array.from({ length: MAX_MESSAGES - 2 }, (_, i) => ({
    role: i % 2 ? "assistant" : "user", content: `Message ${i}`,
  }));
  await open(CURRENT);
  await send("Question 200");
  assert.equal(records.get(CURRENT).messages.length, 400);
  assert.equal($(".chat-notice.warn"), null);
  await send("Question 201");
  assert.equal(State.chatHistory.length, 402, "nothing is truncated from the live conversation");
  assert.equal(State.chatHistory[0].content, "Message 0");
  assert.equal(State.chatHistory.at(-1).content, "An answer");
  assert.equal(records.get(CURRENT).messages.length, 400, "the last successful disk save is intact");
  assert.equal(records.get(CURRENT).title, "A long conversation");
  assert.match($(".chat-notice-title")?.textContent ?? "", /not saved/i);
  assert.match($(".chat-notice-body")?.textContent ?? "", /400-message/);
  assert.match($(".chat-notice-body")?.textContent ?? "", /copy/i);
  assert.equal(State.view, "prompt", "the user can still read and copy all messages");
  view.sync();
  assert.ok($(".chat-notice.warn"), "redraws do not hide unsaved history");
});

for (const finish of ["reply", "Stop"]) {
  test(`a disk error after ${finish} warns and keeps the visible and saved history`, async () => {
    await send("Saved question");
    const id = State.chatId;
    answers.chat_history_save = () => Promise.reject(new Error("Disk full"));
    if (finish === "Stop") {
      let reject;
      answers.chat_send = () => new Promise((_, no) => { reject = no; });
      answers.chat_stop = () => reject(new Error("Stopped."));
      await send("Unsaved question");
      emit("chat-delta", "Unsaved partial answer");
      $(".send-btn").fire("click");
      await flush();
      view.sync();
      assert.equal(State.chatHistory.at(-1).stopped, true);
    } else await send("Unsaved question");
    assert.equal(State.chatHistory.length, 4);
    assert.equal(records.get(id).messages.length, 2);
    assert.match($(".chat-notice-title")?.textContent ?? "", /not saved/i);
    assert.match($(".chat-notice-body")?.textContent ?? "", /Disk full/);
    assert.match($(".chat-notice-body")?.textContent ?? "", /copy/i);
    assert.equal(State.view, "prompt");
    delete answers.chat_history_save;
    delete answers.chat_send;
    await send("Retry by continuing");
    assert.equal(records.get(id).messages.length, 6);
    assert.equal($(".chat-notice.warn"), null, "a later successful save clears the warning");
  });
}

test("a failed first save warns; New chat immediately clears that warning", async (t) => {
  observeView(t);
  answers.chat_history_save = () => Promise.reject(new Error("Disk full"));
  await send("Unsaved first question");
  assert.equal(records.size, 0);
  assert.equal(State.chatHistory.length, 2);
  assert.ok($(".chat-notice.warn"));
  await history();
  $(".history-new").fire("click");
  assert.equal($(".chat-notice.warn"), null);
  delete answers.chat_history_save;
  await send("New question");
  assert.equal(records.get(State.chatId).title, "New question");
});

for (const destination of [CURRENT, OTHER]) {
  test(`a late failed save cannot warn after reopening ${destination}`, async (t) => {
    seed(CURRENT, "Lisbon trip");
    seed(OTHER, "Paris trip");
    await open(CURRENT);
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(); await flush(); });
    answers.chat_history_save = () => pending.promise;
    await send("Unsaved answer");
    await open(destination);
    pending.reject(new Error("Old save failed"));
    await flush();
    view.sync();
    assert.equal(State.chatId, destination);
    assert.equal(State.chatHistory.length, 2);
    assert.equal($(".chat-notice.warn"), null);
  });
}

test("an older failed save cannot replace a newer successful save's status", async (t) => {
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(); await flush(); });
  answers.chat_history_save = () => pending.promise;
  await send("First question");
  delete answers.chat_history_save;
  await send("Second question");
  assert.equal(records.get(State.chatId).messages.length, 4);
  pending.reject(new Error("Old save failed"));
  await flush();
  view.sync();
  assert.equal($(".chat-notice.warn"), null);
});

test("a failed Regenerate cannot hide an earlier unsaved-history warning", async () => {
  await send("Saved question");
  answers.chat_history_save = () => Promise.reject(new Error("Disk full"));
  await send("Unsaved question");
  assert.ok($(".chat-notice.warn"));
  answers.chat_send = () => Promise.reject(new Error("Model unavailable"));
  view.el.find(".md-copy").find((button) => button.getAttribute("aria-label") === "Regenerate").fire("click");
  await flush();
  State.view = "prompt";
  view.sync();
  assert.equal(State.chatHistory.at(-1).content, "Unsaved question");
  assert.equal(records.get(State.chatId).messages.length, 2);
  assert.match($(".chat-notice-body")?.textContent ?? "", /Disk full/);
});

test("a pending save failure still warns if Regenerate fails in the same conversation", async (t) => {
  await send("Saved question");
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(); await flush(); });
  answers.chat_history_save = () => pending.promise;
  await send("Unsaved question");
  answers.chat_send = () => Promise.reject(new Error("Model unavailable"));
  view.el.find(".md-copy").find((button) => button.getAttribute("aria-label") === "Regenerate").fire("click");
  await flush();
  pending.reject(new Error("Disk full"));
  await flush();
  State.view = "prompt";
  view.sync();
  assert.equal(State.chatHistory.at(-1).content, "Unsaved question");
  assert.equal(records.get(State.chatId).messages.length, 2);
  assert.match($(".chat-notice-body")?.textContent ?? "", /Disk full/);
});


// Opening restores native context, so delayed IPC must be ordered with every
// reset as well as fenced from replacing a newer UI transcript.
function deferredOpen(t, id) {
  const pending = Promise.withResolvers();
  const saved = structuredClone(records.get(id));
  t.after(async () => { pending.resolve(saved); await flush(); });
  return { ...pending, saved };
}

for (const first of [CURRENT, OTHER]) {
  test(`two pending history opens finish in selection order from ${first}`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip", "Lisbon question");
    seed(OTHER, "Paris trip", "Paris question");
    await open(CURRENT);
    const last = first === CURRENT ? OTHER : CURRENT;
    const expected = structuredClone(records.get(last).messages);
    const pending = deferredOpen(t, first);
    answers.chat_history_open = ({ id }) => id === first ? pending.promise : structuredClone(records.get(id));
    await open(first);
    await open(last);
    assert.deepEqual(sent("chat_history_open").slice(1), [{ id: first }], "the second restore waits for the first reply");
    pending.resolve(pending.saved);
    await flush();
    assert.equal(State.chatId, last);
    assert.deepEqual(State.chatHistory.map(({ content }) => content), expected.map(({ content }) => content));
    assert.deepEqual(nativeHistory, expected);
    assert.equal($(".chat-input").disabled, false);
    await send("Continue the last selected chat");
    assert.deepEqual(sendHistories.at(-1), expected);
  });
}

test("New chat immediately replaces a pending open and resets native context before sending", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  const pending = deferredOpen(t, CURRENT);
  answers.chat_history_open = () => pending.promise;
  await open(CURRENT);
  $(".history-new").fire("click");
  assert.deepEqual(State.chatHistory, []);
  assert.equal(State.chatId, null);
  assert.equal($(".chat-log").textContent, "");
  assert.equal(sent("chat_reset").length, 0, "reset waits for the already-started native restoration");
  await send("Keep this draft");
  assert.equal(sent("chat_send").length, 0);
  assert.equal($(".chat-input").value, "Keep this draft");
  pending.resolve(pending.saved);
  await flush();
  assert.deepEqual(State.chatHistory, []);
  assert.equal(State.chatId, null);
  assert.equal($(".chat-restored"), null);
  assert.deepEqual(nativeHistory, []);
  assert.equal(sent("chat_reset").length, 1);
  await send("A new beginning");
  assert.deepEqual(sendHistories.at(-1), []);
  assert.notEqual(State.chatId, CURRENT);
});

test("a file-drop reset supersedes every already-selected history open", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  const pending = deferredOpen(t, CURRENT);
  answers.chat_history_open = ({ id }) => id === CURRENT ? pending.promise : structuredClone(records.get(id));
  await open(CURRENT);
  await open(OTHER);
  State.chatHistory = [];
  State.chatId = null;
  State.droppedFile = { name: "new.txt", path: "/inbox/new.txt" };
  State.promptContext = { kind: "file", ...State.droppedFile };
  void Bridge.chatReset();
  State.view = "uploading";
  State.notify();
  const messages = State.chatHistory;
  const file = State.droppedFile;
  const context = State.promptContext;
  pending.resolve(pending.saved);
  await flush();
  assert.equal(State.chatHistory, messages);
  assert.equal(State.chatId, null);
  assert.equal(State.droppedFile, file);
  assert.equal(State.promptContext, context);
  assert.equal(State.view, "uploading");
  assert.deepEqual(nativeHistory, []);
  State.view = "prompt";
  State.notify();
  await send("Read the new file");
  assert.deepEqual(sendHistories.at(-1), []);
  assert.deepEqual(sent("chat_send").at(-1).context, context);
});

test("a failed later open keeps the last successfully restored conversation in UI and native context", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  const pending = deferredOpen(t, CURRENT);
  answers.chat_history_open = ({ id }) => id === CURRENT ? pending.promise : Promise.reject(new Error("Paris unavailable"));
  await open(CURRENT);
  await open(OTHER);
  pending.resolve(pending.saved);
  await flush();
  assert.equal(State.chatId, CURRENT);
  assert.deepEqual(nativeHistory, pending.saved.messages);
  assert.match($(".history").textContent, /Paris unavailable/);
  assert.equal($(".chat-input").disabled, false);
  await send("Continue Lisbon");
  assert.deepEqual(sendHistories.at(-1), pending.saved.messages);
  delete answers.chat_history_open;
  await open(OTHER);
  assert.equal(State.chatId, OTHER, "retry remains available after a failed queued open");
});

test("a failed older open cannot replace the newer selected history row with its error", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  const pending = deferredOpen(t, CURRENT);
  answers.chat_history_open = ({ id }) => id === CURRENT ? pending.promise : structuredClone(records.get(id));
  await open(CURRENT);
  await open(OTHER);
  pending.reject(new Error("Old open failed"));
  await flush();
  assert.equal(State.chatId, OTHER);
  assert.deepEqual(nativeHistory, records.get(OTHER).messages);
  assert.doesNotMatch($(".history").textContent, /Old open failed/);
  assert.equal($(".chat-input").disabled, false);
});

test("closing and reopening history retains the open reservation and fences its old error", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  const pending = deferredOpen(t, CURRENT);
  answers.chat_history_open = () => pending.promise;
  await open(CURRENT);
  $(".history-btn").fire("click");
  await history();
  assert.equal($(".chat-input").disabled, true);
  assert.equal($(".send-btn").disabled, true);
  $(".chat-input").value = "Keep my draft";
  $(".send-btn").fire("click");
  for (const listener of $(".chat-input").listeners.get("keydown") ?? []) {
    listener({ key: "Enter", preventDefault() {}, stopPropagation() {} });
  }
  view.el.find(".md-copy").find((button) => button.getAttribute("aria-label") === "Regenerate").fire("click");
  $(".voice-btn").fire("click");
  await flush();
  assert.equal(sent("chat_send").length, 0);
  assert.equal(sent("chat_undo_last").length, 0);
  assert.equal(sent("assistant_voice_toggle").length, 0);
  assert.equal($(".chat-input").value, "Keep my draft");
  pending.reject(new Error("Dismissed open failed"));
  await flush();
  assert.doesNotMatch($(".history").textContent, /Dismissed open failed/);
  assert.equal(view.el.find(".history-row").length, 1);
  assert.equal($(".chat-input").disabled, false);
  assert.equal($(".send-btn").disabled, false);
  assert.equal(State.chatId, CURRENT);
});


test("Send waits for a delayed reset before starting a native turn", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(); await flush(); });
  answers.chat_reset = () => pending.promise;
  newConversation();
  await send("A new beginning");
  assert.equal(sent("chat_send").length, 0);
  assert.equal(State.chatHistory.at(-1).content, "A new beginning");
  pending.resolve();
  await flush();
  assert.equal(sent("chat_send").length, 1);
  assert.deepEqual(sendHistories.at(-1), []);
  assert.equal(State.chatHistory.at(-1).content, "An answer");
});

for (const cancellation of ["another reset", "Stop"]) {
  test(`${cancellation} cancels a Send that is still waiting for native reset`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    await open(CURRENT);
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(); await flush(); });
    answers.chat_reset = () => pending.promise;
    newConversation();
    await send("Do not send this after cancellation");
    assert.equal(sent("chat_send").length, 0);
    if (cancellation === "another reset") newConversation();
    else {
      $(".send-btn").fire("click");
      assert.equal(sent("chat_stop").length, 1, "Stop bypasses the navigation queue");
    }
    pending.resolve();
    await flush();
    assert.equal(sent("chat_send").length, 0);
    assert.deepEqual(nativeHistory, []);
    assert.equal(State.view, "prompt");
    assert.equal(State.stateOverride, null);
    assert.equal($(".chat-input").disabled, false);
    await send("Now send this");
    assert.equal(sent("chat_send").length, 1);
    assert.deepEqual(sendHistories.at(-1), []);
  });
}

for (const outcome of ["success", "failure"]) {
  test(`a reset during pending undo waits for its ${outcome} without reviving the old turn`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    await open(CURRENT);
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(true); await flush(); });
    answers.chat_undo_last = () => pending.promise;
    view.el.find(".md-copy").find((button) => button.getAttribute("aria-label") === "Regenerate").fire("click");
    await flush();
    newConversation();
    assert.equal(sent("chat_reset").length, 0);
    if (outcome === "failure") pending.reject(new Error("Undo unavailable"));
    else pending.resolve(true);
    await flush();
    assert.equal(sent("chat_reset").length, 1);
    assert.equal(sent("chat_send").length, 0);
    assert.deepEqual(State.chatHistory, []);
    assert.deepEqual(nativeHistory, []);
    assert.equal(State.stateOverride, null);
    await send("Fresh question");
    assert.deepEqual(sendHistories.at(-1), []);
  });
}

test("an open selected after New chat still runs after the superseded open and reset", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  const pending = deferredOpen(t, CURRENT);
  answers.chat_history_open = ({ id }) => id === CURRENT ? pending.promise : structuredClone(records.get(id));
  await open(CURRENT);
  $(".history-new").fire("click");
  await open(OTHER);
  pending.resolve(pending.saved);
  await flush();
  assert.equal(State.chatId, OTHER);
  assert.deepEqual(nativeHistory, records.get(OTHER).messages);
  assert.deepEqual(calls.filter(([name]) => name === "chat_history_open" || name === "chat_reset"), [
    ["chat_history_open", { id: CURRENT }], ["chat_reset", {}], ["chat_history_open", { id: OTHER }],
  ]);
  assert.equal($(".chat-input").disabled, false);
});


for (const recovery of ["reset", "open"]) {
  test(`failed reset blocks queued and future turns until a successful ${recovery}`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    seed(OTHER, "Paris trip");
    await open(CURRENT);
    const oldContext = structuredClone(nativeHistory);
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(); await flush(); });
    const errors = t.mock.method(console, "error", () => {});
    answers.chat_reset = () => pending.promise;
    newConversation();
    await send("A new question, never for Lisbon");
    pending.reject(new Error("Native reset unavailable"));
    await flush();
    assert.equal(sent("chat_send").length, 0, "a failed reset must not send using the old context");
    assert.deepEqual(nativeHistory, oldContext);
    assert.equal(State.view, "note");
    assert.match(State.noteMessage, /Native reset unavailable/);
    assert.equal(errors.mock.callCount(), 1, "reset keeps its nonthrowing, logged failure behavior");
    State.view = "prompt";
    await send("Retry while context is still stale");
    assert.equal(sent("chat_send").length, 0);
    await assert.rejects(Bridge.chatUndoLast(), /Native reset unavailable/);
    assert.equal(sent("chat_undo_last").length, 0);
    answers.chat_history_open = () => Promise.reject(new Error("Open unavailable"));
    await assert.rejects(Bridge.chatHistoryOpen(OTHER), /Open unavailable/);
    await assert.rejects(Bridge.chatSend("Still stale", null), /Native reset unavailable/);
    assert.equal(sent("chat_send").length, 0, "a failed open does not certify native context");
    delete answers.chat_history_open;
    delete answers.chat_reset;
    State.view = "prompt";
    if (recovery === "reset") {
      newConversation();
      await flush();
    } else await open(OTHER);
    const expected = recovery === "reset" ? [] : structuredClone(records.get(OTHER).messages);
    await send("After successful recovery");
    assert.equal(sent("chat_send").length, 1);
    assert.deepEqual(sendHistories.at(-1), expected);
    assert.equal(State.chatHistory.at(-1).content, "An answer");
  });
}

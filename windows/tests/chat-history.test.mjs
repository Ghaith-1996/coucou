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
const MAX_LISTED = Number(readFileSync(new URL("../src-tauri/src/chat_store.rs", import.meta.url), "utf8")
  .match(/pub const MAX_LISTED: usize = (\d+);/)[1]);
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
    return historyPage(args?.before);
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
  historyRow(id).fire("click");
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
  const row = historyRow(id);
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
    const row = historyRow(deleted);
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
  const row = historyRow(id);
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
    if (destination === "another chat") {
      newConversation();
      await open(OTHER);
    }
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

for (const outcome of ["success", "failure"]) {
  test(`active deletion reserves other history rows until its ${outcome}`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    seed(OTHER, "Paris trip");
    await open(CURRENT);
    const original = structuredClone(nativeHistory);
    const deletion = Promise.withResolvers();
    t.after(async () => { deletion.resolve(); await flush(); });
    answers.chat_history_delete = async ({ id }) => { await deletion.promise; records.delete(id); };
    await confirmDelete(CURRENT);
    await open(OTHER);
    for (const key of ["Enter", " "]) pressKey(historyRow(OTHER), key);
    await flush();
    assert.equal(sent("chat_history_open").length, 1, "blocked selection must not change the deletion's navigation ownership");
    assert.equal(State.chatId, CURRENT);
    assert.deepEqual(nativeHistory, original);
    $(".history-btn").fire("click");
    await history();
    if (outcome === "success") deletion.resolve();
    else deletion.reject(new Error("Deletion unavailable"));
    await flush();
    assert.equal(sent("chat_history_open").length, 1, "opening must require a fresh selection after deletion");
    assert.equal(State.chatId, outcome === "success" ? null : CURRENT);
    assert.deepEqual(nativeHistory, outcome === "success" ? [] : original);
    answers.chat_history_open = () => Promise.reject(new Error("Paris unavailable"));
    await open(OTHER);
    assert.match($(".history").textContent, /Paris unavailable/);
    assert.equal(State.chatId, outcome === "success" ? null : CURRENT, "failed navigation cannot restore a successfully deleted fallback");
    assert.deepEqual(nativeHistory, outcome === "success" ? [] : original);
    delete answers.chat_history_open;
    $(".history-btn").fire("click");
    await open(OTHER);
    assert.equal(State.chatId, OTHER);
    await send("Keep planning Paris");
    assert.equal(records.get(OTHER).title, "Paris trip");
    assert.equal(records.has(CURRENT), outcome === "failure");
  });
}

test("deleting an inactive conversation still permits unrelated history navigation", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  const deletion = Promise.withResolvers();
  t.after(async () => { deletion.resolve(); await flush(); });
  answers.chat_history_delete = async ({ id }) => { await deletion.promise; records.delete(id); };
  await confirmDelete(CURRENT);
  await open(OTHER);
  assert.equal(State.chatId, OTHER);
  const messages = State.chatHistory;
  const expected = structuredClone(nativeHistory);
  deletion.resolve();
  await flush();
  assert.equal(State.chatHistory, messages);
  assert.deepEqual(nativeHistory, expected);
  assert.equal(sent("chat_reset").length, 0);
  await send("Keep this newer conversation");
  assert.deepEqual(sendHistories.at(-1), expected);
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

function renameButton(id) {
  return historyRow(id).find(".history-act").find((button) => button.getAttribute("aria-label") === "Rename");
}

function pressKey(target, key, { repeat = false } = {}) {
  const event = {
    key, target, repeat, defaultPrevented: false, propagationStopped: false,
    preventDefault() { this.defaultPrevented = true; },
    stopPropagation() { this.propagationStopped = true; },
  };
  for (let node = target; node; node = node.parentNode) {
    event.currentTarget = node;
    for (const listener of node.listeners.get("keydown") ?? []) listener(event);
    if (event.propagationStopped) break;
  }
  return event;
}

for (const key of ["Enter", " "]) {
  test(`history rows open with ${key === " " ? "Space" : key} and suppress repeated activation`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    await history();
    const row = historyRow(CURRENT);
    assert.equal(row.getAttribute("role"), "button");
    assert.equal(row.getAttribute("tabindex"), "0");
    const pending = deferredOpen(t, CURRENT);
    answers.chat_history_open = () => pending.promise;
    const event = pressKey(row, key);
    assert.equal(event.defaultPrevented, true, "activation must not scroll the history panel");
    assert.equal(pressKey(row, key, { repeat: true }).defaultPrevented, true);
    await flush();
    assert.deepEqual(sent("chat_history_open"), [{ id: CURRENT }]);
    assert.equal($(".chat-input").disabled, true);
    assert.equal($(".send-btn").disabled, true);
    pending.resolve(pending.saved);
    await flush();
    assert.deepEqual(sent("chat_history_open"), [{ id: CURRENT }], "held keys must not enqueue another open");
    assert.equal(State.chatId, CURRENT);
    assert.deepEqual(nativeHistory, pending.saved.messages);
    assert.equal(view.el.find(".chat-row").length, 2);
    assert.equal($(".history").classList.contains("on"), false);
    assert.equal($(".chat-input").disabled, false);
  });

  test(`history row ${key === " " ? "Space" : key} ignores nested action buttons and the rename input`, async () => {
    seed(CURRENT, "Lisbon trip");
    await history();
    const row = historyRow(CURRENT);
    for (const button of row.find(".history-act")) {
      assert.equal(pressKey(button, key).defaultPrevented, false, "native button activation stays available");
    }
    assert.equal(sent("chat_history_open").length, 0);
    renameButton(CURRENT).fire("click");
    const input = $(".history-rename");
    input.value = "Lisbon holiday";
    pressKey(input, key);
    if (key === " ") input.fire("blur");
    await flush();
    assert.equal(sent("chat_history_open").length, 0);
    assert.equal(records.get(CURRENT).title, "Lisbon holiday");
  });
}

test("history rows leave other keys and closed-panel key presses alone", async () => {
  seed(CURRENT, "Lisbon trip");
  await history();
  const row = historyRow(CURRENT);
  for (const key of ["Tab", "Escape", "ArrowDown", "a"]) {
    assert.equal(pressKey(row, key).defaultPrevented, false);
  }
  $(".history-btn").fire("click");
  for (const key of ["Enter", " "]) assert.equal(pressKey(row, key).defaultPrevented, false);
  await flush();
  assert.equal(sent("chat_history_open").length, 0);
  assert.equal(State.chatId, null);
});

test("keyboard history selection queues behind a pending open without bypassing its reservation", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  const pending = deferredOpen(t, CURRENT);
  answers.chat_history_open = ({ id }) => id === CURRENT ? pending.promise : structuredClone(records.get(id));
  await open(CURRENT);
  pressKey(historyRow(OTHER), " ");
  await flush();
  assert.deepEqual(sent("chat_history_open"), [{ id: CURRENT }]);
  assert.equal($(".chat-input").disabled, true);
  pending.resolve(pending.saved);
  await flush();
  assert.deepEqual(sent("chat_history_open"), [{ id: CURRENT }, { id: OTHER }]);
  assert.equal(State.chatId, OTHER);
  assert.deepEqual(nativeHistory, records.get(OTHER).messages);
  assert.equal($(".chat-input").disabled, false);
});

test("keyboard history activation respects pending deletion and recovers after its failure", async (t) => {
  seed(CURRENT, "Lisbon trip");
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(); await flush(); });
  answers.chat_history_delete = () => pending.promise;
  await confirmDelete(CURRENT);
  for (const key of ["Enter", " "]) pressKey(historyRow(CURRENT), key);
  await flush();
  assert.equal(sent("chat_history_open").length, 0);
  pending.reject(new Error("Delete unavailable"));
  await flush();
  pressKey(historyRow(CURRENT), "Enter");
  await flush();
  assert.deepEqual(sent("chat_history_open"), [{ id: CURRENT }]);
  assert.equal(State.chatId, CURRENT);
});

test("a failed keyboard history open releases the composer and permits retry", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  await open(CURRENT);
  const messages = State.chatHistory;
  answers.chat_history_open = () => Promise.reject(new Error("Paris unavailable"));
  await history();
  pressKey(historyRow(OTHER), "Enter");
  await flush();
  assert.equal(State.chatId, CURRENT);
  assert.equal(State.chatHistory, messages);
  assert.match($(".history").textContent, /Paris unavailable/);
  assert.equal($(".chat-input").disabled, false);
  assert.equal($(".send-btn").disabled, false);
  $(".history-btn").fire("click");
  delete answers.chat_history_open;
  await history();
  pressKey(historyRow(OTHER), " ");
  await flush();
  assert.equal(State.chatId, OTHER);
  assert.deepEqual(nativeHistory, records.get(OTHER).messages);
  assert.equal($(".history").classList.contains("on"), false);
});

test("a failed rename shows its error and retries the typed title without changing the saved title", async () => {
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  answers.chat_history_rename = () => Promise.reject(new Error("Disk unavailable"));
  await rename(CURRENT, "  Lisbon holiday  ");
  assert.match(historyRow(CURRENT).textContent, /Lisbon trip/);
  assert.match($(".history-error")?.textContent ?? "", /Disk unavailable/);
  assert.equal(records.get(CURRENT).title, "Lisbon trip");
  assert.equal($(".history-rename-retry")?.textContent, "Retry");
  delete answers.chat_history_rename;
  $(".history-rename-retry").fire("click");
  await flush();
  assert.deepEqual(sent("chat_history_rename"), [
    { id: CURRENT, title: "Lisbon holiday" }, { id: CURRENT, title: "Lisbon holiday" },
  ]);
  assert.equal($(".history-error"), null);
  assert.match(historyRow(CURRENT).textContent, /Lisbon holiday/);
  await send();
  assert.equal(records.get(CURRENT).title, "Lisbon holiday");
});

test("editing a rejected rename restores typed intent and Escape keeps the saved title", async () => {
  seed(CURRENT, "Lisbon trip");
  answers.chat_history_rename = () => Promise.reject(new Error("Disk unavailable"));
  await rename(CURRENT, "Lisbon holiday");
  renameButton(CURRENT).fire("click");
  const input = $(".history-rename");
  assert.equal(input.value ?? input.getAttribute("value"), "Lisbon holiday");
  input.value = "Discard this";
  pressKey(input, "Escape");
  input.fire("blur");
  await flush();
  assert.equal(sent("chat_history_rename").length, 1);
  assert.match(historyRow(CURRENT).textContent, /Lisbon trip/);
});

test("Enter followed by blur submits one rename and reserves the row until settlement", async (t) => {
  seed(CURRENT, "Lisbon trip");
  await history();
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(); await flush(); });
  answers.chat_history_rename = () => pending.promise;
  renameButton(CURRENT).fire("click");
  const input = $(".history-rename");
  input.value = "Lisbon holiday";
  pressKey(input, "Enter");
  input.fire("blur");
  await flush();
  assert.equal(sent("chat_history_rename").length, 1);
  assert.equal(renameButton(CURRENT).disabled, true);
  assert.match(historyRow(CURRENT).textContent, /Lisbon trip/);
  pending.reject(new Error("Rename failed once"));
  await flush();
  assert.equal(renameButton(CURRENT).disabled, false);
  assert.match($(".history-error")?.textContent ?? "", /Rename failed once/);
});

for (const transition of ["close and reopen", "New chat", "delete", "failed open"]) {
  test(`a late rename rejection cannot replace history after ${transition}`, async (t) => {
    seed(CURRENT, "Lisbon trip");
    seed(OTHER, "Paris trip");
    await open(CURRENT);
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(); await flush(); });
    answers.chat_history_rename = () => pending.promise;
    await rename(CURRENT, "A stale intent");
    if (transition === "close and reopen") {
      $(".history-btn").fire("click");
      await history();
    } else if (transition === "New chat") {
      $(".history-new").fire("click");
      await send("Fresh conversation");
      await history();
    } else if (transition === "delete") await confirmDelete(CURRENT);
    else {
      answers.chat_history_open = () => Promise.reject(new Error("New open failed"));
      await open(OTHER);
    }
    const active = State.chatId;
    pending.reject(new Error("Old rename failed"));
    await flush();
    assert.doesNotMatch($(".history").textContent, /Old rename failed/);
    assert.equal($(".history-rename-retry"), null);
    assert.equal(State.chatId, active);
    if (transition === "failed open") assert.match($(".history").textContent, /New open failed/);
    if (transition === "delete") assert.equal(historyRow(CURRENT), undefined);
  });
}

test("a successful pending rename remains authoritative when overlapping deletion fails", async (t) => {
  seed(CURRENT, "Lisbon trip");
  await open(CURRENT);
  const changing = Promise.withResolvers();
  const deleting = Promise.withResolvers();
  t.after(async () => { changing.resolve(); deleting.resolve(); await flush(); });
  answers.chat_history_rename = async ({ id, title }) => { await changing.promise; records.get(id).title = title; };
  answers.chat_history_delete = () => deleting.promise;
  await rename(CURRENT, "Lisbon holiday");
  await confirmDelete(CURRENT);
  changing.resolve();
  await flush();
  deleting.reject(new Error("Delete unavailable"));
  await flush();
  assert.match(historyRow(CURRENT).textContent, /Lisbon holiday/);
  assert.match($(".history-error")?.textContent ?? "", /Delete unavailable/);
  await send();
  assert.equal(records.get(CURRENT).title, "Lisbon holiday");
});

for (const outcome of ["success", "failure"]) {
  test(`a failed overlapping delete keeps rename reserved until its ${outcome}`, async (t) => {
    seed(CURRENT, "Lisbon trip");
    await open(CURRENT);
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(); await flush(); });
    answers.chat_history_rename = async ({ id, title }) => { await pending.promise; records.get(id).title = title; };
    answers.chat_history_delete = () => Promise.reject(new Error("Delete unavailable"));
    await rename(CURRENT, "Lisbon holiday");
    await confirmDelete(CURRENT);
    assert.equal(renameButton(CURRENT).disabled, true);
    renameButton(CURRENT).fire("click");
    assert.equal($(".history-rename"), null, "the handler also guards the reservation");
    assert.equal(sent("chat_history_rename").length, 1);
    if (outcome === "success") pending.resolve();
    else pending.reject(new Error("Rename unavailable"));
    await flush();
    assert.equal(renameButton(CURRENT).disabled, false);
    if (outcome === "success") assert.match(historyRow(CURRENT).textContent, /Lisbon holiday/);
    else {
      assert.match(historyRow(CURRENT).textContent, /Lisbon trip/);
      assert.match($(".history").textContent, /Rename unavailable/);
      assert.ok($(".history-rename-retry"));
    }
  });
}

for (const transition of ["failed open", "New chat", "delete"]) {
  test(`a late rename success preserves the newer view after ${transition}`, async (t) => {
    seed(CURRENT, "Lisbon trip");
    seed(OTHER, "Paris trip");
    await open(CURRENT);
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(); await flush(); });
    answers.chat_history_rename = async ({ id, title }) => {
      await pending.promise;
      if (records.has(id)) records.get(id).title = title;
    };
    await rename(CURRENT, "Lisbon holiday");
    if (transition === "failed open") {
      answers.chat_history_open = () => Promise.reject(new Error("New open failed"));
      await open(OTHER);
    } else if (transition === "New chat") {
      $(".history-new").fire("click");
      await send("Fresh conversation");
      await history();
    } else await confirmDelete(CURRENT);
    const active = State.chatId;
    pending.resolve();
    await flush();
    assert.equal(State.chatId, active);
    if (transition === "failed open") assert.match($(".history").textContent, /New open failed/);
    if (transition === "delete") assert.equal(historyRow(CURRENT), undefined);
    if (transition === "New chat") {
      await send();
      assert.equal(records.get(active).title, "Fresh conversation");
    }
  });
}

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


// The file chip can outlive native attachment content after a provider switch.
// Keep the policy visible without guessing whether an attempted turn committed.
function droppedAttachment(name = "picture.png") {
  State.droppedFile = { name, path: `/inbox/${name}` };
  State.promptContext = { kind: "file", ...State.droppedFile };
  view.sync();
}

function attachmentWarning() {
  const warning = $(".chat-attachment");
  assert.ok(warning, "file context must explain provider-switch attachment loss");
  assert.equal(warning.parentElement, $(".chat-log"), "the policy scrolls with messages instead of clipping the composer");
  assert.equal(view.el.find(".chat-attachment").length, 1, "redraws and streams cannot duplicate the policy");
  assert.equal(warning.getAttribute("role"), "status");
  assert.equal(warning.querySelector(".chat-notice-title"), null, "the policy stays compact in the fixed-height chat");
  const body = warning.querySelector(".chat-notice-body").textContent;
  assert.match(body, /Sending with another provider can discard attachments/);
  assert.match(body, /even on failure or Stop/);
  assert.match(body, /Switching back won't restore them/);
  assert.match(body, /Drop the file again for a new chat/);
  return warning;
}

test("a completed file turn explains provider changes before the next request, including after rebuilding the view", async () => {
  assert.equal($(".chat-attachment"), null);
  droppedAttachment();
  assert.equal($(".chat-attachment"), null, "a fresh file has no committed context to lose yet");
  assert.equal(sent("chat_send").length, 0);
  assert.equal($(".chat-restored"), null, "a fresh file is not falsely described as already unavailable");
  await send("Describe this file");
  attachmentWarning();
  view = buildPrompt(() => {});
  await flush();
  view.sync();
  attachmentWarning();
  State.settings = { ...State.settings, chatProvider: "chatgpt" };
  view.sync();
  attachmentWarning();
});

for (const outcome of ["failure", "Stop"]) {
  test(`a first file turn ending in ${outcome} does not claim committed attachment context`, async () => {
    droppedAttachment();
    if (outcome === "failure") answers.chat_send = () => Promise.reject(new Error("Provider unavailable"));
    else {
      let reject;
      answers.chat_send = () => new Promise((_, no) => { reject = no; });
      answers.chat_stop = () => reject(new Error("Stopped."));
    }
    await send();
    if (outcome === "Stop") {
      emit("chat-delta", "Partial first answer");
      $(".send-btn").fire("click");
      await flush();
    }
    State.view = "prompt";
    view.sync();
    assert.equal($(".chat-attachment"), null);
    delete answers.chat_send;
    await send("Retry with the file");
    attachmentWarning();
  });
}

for (const outcome of ["reply", "failure", "Stop"]) {
  test(`the attachment policy survives a provider switch ending in ${outcome} and switching back`, async () => {
    State.settings = { ...State.settings, chatProvider: "chatgpt" };
    droppedAttachment();
    await send("Describe this file");
    attachmentWarning();
    State.settings = { ...State.settings, chatProvider: "openai" };
    view.sync();
    attachmentWarning();
    if (outcome === "failure") answers.chat_send = () => Promise.reject(new Error("Provider unavailable"));
    if (outcome === "Stop") {
      let reject;
      answers.chat_send = () => new Promise((_, no) => { reject = no; });
      answers.chat_stop = () => reject(new Error("Stopped."));
    }
    await send("What else is in the file?");
    attachmentWarning();
    if (outcome === "Stop") {
      emit("chat-delta", "A partial answer");
      assert.equal($(".chat-log").children.at(-1), attachmentWarning(), "the policy remains after the live answer");
      view.sync();
      emit("chat-delta", "A longer partial answer");
      assert.equal($(".chat-log").children.at(-1), attachmentWarning());
      assert.match($(".chat-log").textContent, /A longer partial answer/);
      $(".send-btn").fire("click");
      await flush();
      assert.equal(State.chatHistory.at(-1).stopped, true);
    }
    State.view = "prompt";
    State.settings = { ...State.settings, chatProvider: "chatgpt" };
    view.sync();
    attachmentWarning();
    assert.equal(State.droppedFile.name, "picture.png", "the policy does not change file/retry semantics");
    delete answers.chat_send;
    await send("Try again with the original provider");
    attachmentWarning();
  });
}

for (const outcome of ["reply", "failure", "Stop", "undo failure"]) {
  test(`the attachment policy follows committed context after first-turn Regenerate ending in ${outcome}`, async () => {
    droppedAttachment("notes.txt");
    await send("Summarize the file");
    State.settings = { ...State.settings, chatProvider: "chatgpt" };
    if (outcome === "failure") answers.chat_send = () => Promise.reject(new Error("Provider unavailable"));
    if (outcome === "undo failure") answers.chat_undo_last = () => Promise.reject(new Error("Undo unavailable"));
    if (outcome === "Stop") {
      let reject;
      answers.chat_send = () => new Promise((_, no) => { reject = no; });
      answers.chat_stop = () => reject(new Error("Stopped."));
    }
    view.el.find(".md-copy").find((button) => button.getAttribute("aria-label") === "Regenerate").fire("click");
    await flush();
    if (outcome === "Stop") {
      emit("chat-delta", "A partial regenerated answer");
      $(".send-btn").fire("click");
      await flush();
    }
    State.view = "prompt";
    view.sync();
    if (outcome === "failure" || outcome === "Stop") assert.equal($(".chat-attachment"), null, "undo removed the only committed turn, so the file can attach again");
    else attachmentWarning();
    assert.equal(State.droppedFile.name, "notes.txt");
  });
}

test("a later failed Regenerate retains the attachment policy for earlier completed turns", async () => {
  droppedAttachment();
  await send("Describe this file");
  await send("What else?");
  answers.chat_send = () => Promise.reject(new Error("Provider unavailable"));
  view.el.find(".md-copy").find((button) => button.getAttribute("aria-label") === "Regenerate").fire("click");
  await flush();
  State.view = "prompt";
  view.sync();
  attachmentWarning();
});

test("closing the provider picker keeps one attachment policy before the next send", async () => {
  droppedAttachment();
  await send();
  $(".model-btn").fire("click");
  await flush();
  view.el.find(".picker-chip").find((chip) => chip.textContent === "OpenAI").fire("click");
  await flush();
  $(".model-btn").fire("click");
  view.sync();
  assert.equal(State.settings.chatProvider, "openai");
  assert.equal($(".chat-body").classList.contains("picking"), false);
  attachmentWarning();
});

test("the attachment policy relabels in every supported language without rebuilding its state", async () => {
  const { LANGUAGE_CODES, setLanguage, t } = await import("../src/i18n/i18n.ts");
  droppedAttachment();
  await send();
  const warning = attachmentWarning();
  const key = warning.querySelector(".chat-notice-body").textContent;
  try {
    for (const lang of LANGUAGE_CODES.filter((lang) => lang !== "en")) {
      setLanguage(lang);
      view.sync();
      assert.equal($(".chat-attachment"), warning);
      assert.equal(warning.querySelector(".chat-notice-body").textContent, t(key));
      assert.notEqual(t(key), key, `${lang} needs a translated attachment policy`);
    }
  } finally {
    setLanguage("en");
  }
});

test("attachment policy and save failures stay visible together", async () => {
  droppedAttachment();
  answers.chat_history_save = () => Promise.reject(new Error("Disk full"));
  await send();
  attachmentWarning();
  assert.match($(".chat-notice.warn").textContent, /Disk full/);
});

test("attachment policy and ChatGPT usage limits stay visible together", async () => {
  State.settings = { ...State.settings, chatProvider: "chatgpt" };
  droppedAttachment();
  await send("Describe this file");
  answers.chat_send = () => {
    emit("chatgpt-usage-limit", null);
    return Promise.reject(new Error("Usage limit"));
  };
  await send();
  attachmentWarning();
  assert.match($(".chat-notice.warn").textContent, /Usage limit reached/);
});

test("New chat and cancelling a drop clear the attachment policy without retaining old state", async (t) => {
  observeView(t);
  droppedAttachment();
  await send();
  newConversation();
  assert.equal($(".chat-attachment"), null);
  droppedAttachment("replacement.txt");
  assert.equal($(".chat-attachment"), null);
  await send();
  attachmentWarning();
  State.droppedFile = null;
  State.promptContext = null;
  State.notify();
  assert.equal($(".chat-attachment"), null);
});

test("replacing a drop clears the old policy until the new file has a completed turn", async () => {
  droppedAttachment("old.png");
  await send();
  State.chatHistory = [];
  State.chatId = null;
  await Bridge.chatReset();
  droppedAttachment("new.txt");
  assert.equal($(".chat-attachment"), null);
  assert.equal(view.el.find(".chat-row").length, 0);
  assert.match($(".chip").textContent, /new.txt/);
  assert.doesNotMatch(view.el.textContent, /old.png/);
  await send();
  attachmentWarning();
});

for (const outcome of ["success", "failure"]) {
  test(`history open ${outcome} replaces the attachment policy only if the file is actually cleared`, async () => {
    droppedAttachment();
    await send();
    seed(OTHER, "Saved text");
    if (outcome === "failure") answers.chat_history_open = () => Promise.reject(new Error("Open failed"));
    await open(OTHER);
    if (outcome === "success") {
      assert.equal($(".chat-attachment"), null);
      assert.ok($(".chat-restored"), "reopened text-only history keeps its distinct notice");
      assert.equal(State.droppedFile, null);
    } else {
      attachmentWarning();
      assert.equal(State.droppedFile.name, "picture.png");
    }
  });
}

// Opening restores native context, so delayed IPC must be ordered with every
// reset as well as fenced from replacing a newer UI transcript.
function deferredOpen(t, id) {
  const pending = Promise.withResolvers();
  const saved = structuredClone(records.get(id));
  t.after(async () => { pending.resolve(saved); await flush(); });
  return { ...pending, saved };
}

// Delete and open must reserve synchronously: either native completion order
// used to leave a deleted conversation active and recreate its file on Send.
for (const order of ["open first", "delete first"]) {
  test(`deletion of a pending open needs fresh confirmation when ${order}`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    const opening = deferredOpen(t, CURRENT);
    const deletion = Promise.withResolvers();
    t.after(async () => { deletion.resolve(); await flush(); });
    answers.chat_history_open = () => opening.promise;
    answers.chat_history_delete = async ({ id }) => { await deletion.promise; records.delete(id); };
    await open(CURRENT);
    await confirmDelete(CURRENT);
    assert.equal(sent("chat_history_delete").length, 0, "Delete cannot overlap an already-selected open");
    if (order === "open first") opening.resolve(opening.saved);
    else deletion.resolve();
    await flush();
    opening.resolve(opening.saved);
    deletion.resolve();
    await flush();
    assert.equal(records.has(CURRENT), true, "blocked clicks must not queue a later deletion");
    assert.equal(State.chatId, CURRENT);
    assert.deepEqual(nativeHistory, opening.saved.messages);
    assert.equal($(".chat-input").disabled, false);
    await history();
    const button = deleteButton(CURRENT);
    assert.equal(button.disabled, false);
    button.fire("click");
    assert.equal(sent("chat_history_delete").length, 0, "confirmation starts over after opening");
    button.fire("click");
    await flush();
    assert.equal(records.has(CURRENT), false);
    assert.equal(State.chatId, null);
    assert.deepEqual(nativeHistory, []);
    await send("A new question");
    assert.equal(records.has(CURRENT), false, "Send must not recreate the deleted file");
    assert.deepEqual(sendHistories.at(-1), []);
  });
}

test("opening disarms existing Delete controls and keeps rebuilt controls reserved", async (t) => {
  observeView(t);
  seed(CURRENT, "Lisbon trip");
  seed(OTHER, "Paris trip");
  await history();
  const armed = deleteButton(CURRENT);
  const unrelated = deleteButton(OTHER);
  armed.fire("click");
  assert.match(armed.textContent, /Delete\?/);
  const opening = deferredOpen(t, CURRENT);
  answers.chat_history_open = () => opening.promise;
  await open(CURRENT);
  assert.equal(armed.disabled, true);
  assert.equal(unrelated.disabled, true);
  assert.doesNotMatch(armed.textContent, /Delete\?/);
  for (const button of [armed, unrelated]) {
    button.fire("click");
    button.fire("click");
  }
  $(".history-btn").fire("click");
  await history();
  const rebuilt = deleteButton(CURRENT);
  assert.notEqual(rebuilt, armed);
  assert.equal(rebuilt.disabled, true);
  rebuilt.fire("click");
  rebuilt.fire("click");
  assert.equal(sent("chat_history_delete").length, 0);
  opening.resolve(opening.saved);
  await flush();
  assert.equal(records.size, 2);
  assert.deepEqual(nativeHistory, opening.saved.messages);
  assert.equal($(".send-btn").disabled, false);
  await send("Continue this conversation");
  assert.equal(records.get(CURRENT).messages.at(-2).content, "Continue this conversation");
});

for (const first of ["success", "failure"]) {
  for (const last of ["success", "failure"]) {
    test(`Delete stays reserved across queued opens ending in ${first} then ${last}`, async (t) => {
      observeView(t);
      seed(CURRENT, "Lisbon trip");
      seed(OTHER, "Paris trip");
      const opening = deferredOpen(t, CURRENT);
      const later = deferredOpen(t, OTHER);
      answers.chat_history_open = ({ id }) => id === CURRENT ? opening.promise : later.promise;
      await open(CURRENT);
      await open(OTHER);
      assert.equal(deleteButton(CURRENT).disabled, true);
      assert.equal(deleteButton(OTHER).disabled, true);
      if (first === "success") opening.resolve(opening.saved);
      else opening.reject(new Error("Earlier open failed"));
      await flush();
      assert.equal(deleteButton(CURRENT).disabled, true, "one settled open cannot release the remaining reservation");
      await confirmDelete(CURRENT);
      assert.equal(sent("chat_history_delete").length, 0);
      assert.equal($(".chat-input").disabled, true);
      if (last === "success") later.resolve(later.saved);
      else later.reject(new Error("Latest open failed"));
      await flush();
      const expected = last === "success" ? later.saved : first === "success" ? opening.saved : null;
      assert.equal(State.chatId, expected?.id ?? null);
      assert.deepEqual(nativeHistory, expected?.messages ?? []);
      assert.equal($(".chat-input").disabled, false);
      if (last === "failure") {
        assert.match($(".history").textContent, /Latest open failed/, "restoring controls must not redraw away the current error");
        assert.doesNotMatch($(".history").textContent, /Earlier open failed/);
        $(".history-btn").fire("click");
      }
      await history();
      assert.equal(deleteButton(CURRENT).disabled, false);
      assert.equal(deleteButton(OTHER).disabled, false);
      assert.equal(records.size, 2, "no blocked deletion runs when the queue settles");
      await send("Keep the last successful context");
      assert.deepEqual(sendHistories.at(-1), expected?.messages ?? []);
    });
  }
}

for (const destination of ["New chat", "file drop"]) {
  test(`a blocked deletion preserves ${destination} while its older open settles`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    const opening = deferredOpen(t, CURRENT);
    answers.chat_history_open = () => opening.promise;
    await open(CURRENT);
    await confirmDelete(CURRENT);
    if (destination === "New chat") $(".history-new").fire("click");
    else {
      State.chatHistory = [];
      State.chatId = null;
      State.droppedFile = { name: "new.txt", path: "/inbox/new.txt" };
      State.promptContext = { kind: "file", ...State.droppedFile };
      void Bridge.chatReset();
      State.notify();
    }
    const messages = State.chatHistory;
    const context = State.promptContext;
    opening.resolve(opening.saved);
    await flush();
    assert.equal(State.chatHistory, messages);
    assert.equal(State.chatId, null);
    assert.equal(State.promptContext, context);
    assert.equal(records.has(CURRENT), true);
    assert.equal(sent("chat_history_delete").length, 0);
    assert.deepEqual(nativeHistory, []);
    assert.equal($(".chat-input").disabled, false);
    await send("A newer question");
    assert.notEqual(State.chatId, CURRENT);
    assert.deepEqual(sendHistories.at(-1), []);
    assert.deepEqual(sent("chat_send").at(-1).context, context);
  });
}

for (const outcome of ["success", "failure"]) {
  test(`a pending deletion blocks same-row opens until its ${outcome}`, async (t) => {
    observeView(t);
    seed(CURRENT, "Lisbon trip");
    const deletion = Promise.withResolvers();
    t.after(async () => { deletion.resolve(); await flush(); });
    answers.chat_history_delete = async ({ id }) => { await deletion.promise; records.delete(id); };
    await confirmDelete(CURRENT);
    await open(CURRENT);
    assert.equal(sent("chat_history_open").length, 0);
    assert.equal(State.chatId, null);
    assert.deepEqual(nativeHistory, []);
    if (outcome === "success") deletion.resolve();
    else deletion.reject(new Error("Delete failed"));
    await flush();
    assert.equal(sent("chat_history_open").length, 0, "blocked navigation must not run later");
    assert.equal($(".chat-input").disabled, false);
    assert.equal(records.has(CURRENT), outcome === "failure");
    if (outcome === "failure") {
      assert.match($(".history").textContent, /Delete failed/);
      await open(CURRENT);
      assert.equal(State.chatId, CURRENT);
      assert.deepEqual(nativeHistory, records.get(CURRENT).messages);
    }
  });
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

// Paging follows the native cursor contract; store.rs tests exercise real files.
function historyPage(before = null) {
  return [...records.values()]
    .map(({ id, title, updatedAt, messages }) => ({ id, title, updatedAt, count: messages.length }))
    .sort((a, b) => b.updatedAt - a.updatedAt || a.id.localeCompare(b.id))
    .filter((item) => !before || item.updatedAt < before.updatedAt || (item.updatedAt === before.updatedAt && item.id > before.id))
    .slice(0, MAX_LISTED);
}

function historyRow(id) {
  return view.el.find(".history-row").find((row) => row.getAttribute("data-id") === id);
}

function seedHistory(count) {
  const ids = Array.from({ length: count }, (_, i) => `c-${(i + 1).toString(16).padStart(16, "0")}`);
  // Reverse insertion so ties across page boundaries need the id ordering.
  for (const id of [...ids].reverse()) seed(id, `Conversation ${id}`);
  return ids;
}

async function moreHistory() {
  assert.ok($(".history-more"), "older conversations remain reachable");
  $(".history-more").fire("click");
  await flush();
}

test("history paging reaches every saved conversation beyond 500 and opens the oldest", async () => {
  const ids = seedHistory(MAX_LISTED * 2 + 3);
  await history();
  assert.equal(view.el.find(".history-row").length, MAX_LISTED);
  assert.deepEqual(sent("chat_history_list"), [{ before: null }]);
  await moreHistory();
  assert.equal(view.el.find(".history-row").length, MAX_LISTED * 2);
  assert.deepEqual(sent("chat_history_list").at(-1), { before: { id: ids[MAX_LISTED - 1], updatedAt: 456 } });
  await moreHistory();
  assert.deepEqual(view.el.find(".history-row").map((row) => row.getAttribute("data-id")), ids);
  assert.equal($(".history-more"), null);
  assert.equal(records.size, ids.length, "listing never removes saved files");
  await open(ids.at(-1));
  assert.equal(State.chatId, ids.at(-1));
  assert.match($(".chat-log").textContent, /Plan my trip/);
});

test("history paging handles empty history and an exact full final page", async () => {
  await history();
  assert.match($(".history").textContent, /No saved conversations yet/);
  assert.equal($(".history-more"), null);
  $(".history-btn").fire("click");
  seedHistory(MAX_LISTED);
  await history();
  await moreHistory();
  assert.equal(view.el.find(".history-row").length, MAX_LISTED);
  assert.equal($(".history-more"), null);
});

test("history paging retries first-page and later-page failures without losing rows", async () => {
  const ids = seedHistory(MAX_LISTED + 1);
  answers.chat_history_list = () => Promise.reject(new Error("Listing unavailable"));
  await history();
  assert.match($(".history").textContent, /Listing unavailable/);
  assert.doesNotMatch($(".history").textContent, /No saved conversations yet/);
  assert.equal($(".history-retry").textContent, "Retry");
  delete answers.chat_history_list;
  $(".history-retry").fire("click");
  await flush();
  assert.equal(view.el.find(".history-row").length, MAX_LISTED);
  answers.chat_history_list = () => Promise.reject(new Error("Page unavailable"));
  await moreHistory();
  assert.equal(view.el.find(".history-row").length, MAX_LISTED);
  assert.match($(".history").textContent, /Page unavailable/);
  const failedArgs = sent("chat_history_list").at(-1);
  delete answers.chat_history_list;
  $(".history-retry").fire("click");
  await flush();
  assert.deepEqual(sent("chat_history_list").at(-1), failedArgs);
  assert.ok(historyRow(ids.at(-1)));
  assert.equal($(".history-retry"), null);
  assert.equal($(".history-more"), null);
});

test("history paging reserves one request across repeated Load more clicks", async (t) => {
  seedHistory(MAX_LISTED + 1);
  await history();
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve([]); await flush(); });
  answers.chat_history_list = () => pending.promise;
  const button = $(".history-more");
  button.fire("click");
  button.fire("click");
  await flush();
  assert.equal(sent("chat_history_list").length, 2);
  assert.equal($(".history-more").disabled, true);
});

for (const outcome of ["resolves", "rejects"]) {
  test(`history paging ignores an old page that ${outcome} after close and reopen`, async (t) => {
    const ids = seedHistory(MAX_LISTED + 1);
    await history();
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve([]); await flush(); });
    answers.chat_history_list = () => pending.promise;
    await moreHistory();
    $(".history-btn").fire("click");
    records.delete(ids[0]);
    delete answers.chat_history_list;
    await history();
    assert.equal(view.el.find(".history-row").length, MAX_LISTED);
    if (outcome === "resolves") pending.resolve([{ id: "c-ffffffffffffffff", title: "Stale page", updatedAt: 1, count: 2 }]);
    else pending.reject(new Error("Old page failed"));
    await flush();
    assert.doesNotMatch($(".history").textContent, /Stale page|Old page failed/);
    assert.equal(historyRow(ids[0]), undefined);
    assert.ok(historyRow(ids.at(-1)));
  });
}

test("history paging keeps older loaded rows through rename and delete, including its cursor row", async () => {
  const ids = seedHistory(MAX_LISTED * 2 + 1);
  await history();
  await moreHistory();
  await rename(ids[MAX_LISTED + 1], "An older renamed conversation");
  assert.equal(view.el.find(".history-row").length, MAX_LISTED * 2);
  assert.match(historyRow(ids[MAX_LISTED + 1]).textContent, /An older renamed conversation/);
  const boundary = ids[MAX_LISTED * 2 - 1];
  await confirmDelete(boundary);
  assert.equal(historyRow(boundary), undefined);
  assert.equal(view.el.find(".history-row").length, MAX_LISTED * 2 - 1);
  await moreHistory();
  assert.ok(historyRow(ids.at(-1)), "deleting the cursor cannot skip the next row");
  assert.deepEqual(view.el.find(".history-row").map((row) => row.getAttribute("data-id")), ids.filter((id) => id !== boundary));
});

for (const mutation of ["rename", "delete"]) {
  test(`history paging fences a pending page when ${mutation} completes`, async (t) => {
    const ids = seedHistory(MAX_LISTED + 1);
    await history();
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve([]); await flush(); });
    answers.chat_history_list = () => pending.promise;
    await moreHistory();
    if (mutation === "rename") await rename(ids[0], "New title");
    else await confirmDelete(ids[0]);
    pending.resolve([{ id: ids[0], title: "Old title", updatedAt: 456, count: 2 }]);
    await flush();
    assert.doesNotMatch($(".history").textContent, /Old title/);
    if (mutation === "rename") assert.match(historyRow(ids[0]).textContent, /New title/);
    else assert.equal(historyRow(ids[0]), undefined);
    delete answers.chat_history_list;
    await moreHistory();
    assert.ok(historyRow(ids.at(-1)));
    assert.equal(new Set(view.el.find(".history-row").map((row) => row.getAttribute("data-id"))).size, records.size);
  });
}

test("history paging does not skip older rows when a new save sorts before its cursor", async () => {
  const ids = seedHistory(MAX_LISTED + 1);
  await history();
  const newest = "c-ffffffffffffffff";
  seed(newest, "Saved while browsing");
  records.get(newest).updatedAt = 999;
  await moreHistory();
  assert.deepEqual(view.el.find(".history-row").map((row) => row.getAttribute("data-id")), ids);
  $(".history-btn").fire("click");
  await history();
  assert.equal(view.el.find(".history-row")[0].getAttribute("data-id"), newest);
});

test("history paging ignores an old page after New chat saves another conversation", async (t) => {
  seedHistory(MAX_LISTED + 1);
  await history();
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve([]); await flush(); });
  answers.chat_history_list = () => pending.promise;
  await moreHistory();
  $(".history-new").fire("click");
  await send("A newer saved chat");
  delete answers.chat_history_list;
  await history();
  const id = State.chatId;
  assert.equal(view.el.find(".history-row")[0].getAttribute("data-id"), id);
  pending.resolve([{ id: "c-ffffffffffffffff", title: "Stale page", updatedAt: 1, count: 2 }]);
  await flush();
  assert.doesNotMatch($(".history").textContent, /Stale page/);
  assert.equal(State.chatId, id);
});

test("history paging cannot replace a newer failed conversation open", async (t) => {
  const ids = seedHistory(MAX_LISTED + 1);
  await history();
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve([]); await flush(); });
  answers.chat_history_list = () => pending.promise;
  await moreHistory();
  answers.chat_history_open = () => Promise.reject(new Error("Conversation unavailable"));
  await open(ids[0]);
  assert.match($(".history").textContent, /Conversation unavailable/);
  pending.resolve(historyPage({ id: ids[MAX_LISTED - 1], updatedAt: 456 }));
  await flush();
  assert.match($(".history").textContent, /Conversation unavailable/);
});

for (const mutation of ["rename", "delete"]) {
  test(`history paging restarts a first-page refresh interrupted by ${mutation}`, async (t) => {
    seed(CURRENT, "Cached conversation");
    await history();
    assert.equal($(".history-more"), null, "the cached list was exhausted");
    const changing = Promise.withResolvers();
    const listing = Promise.withResolvers();
    t.after(async () => { changing.resolve(); listing.resolve([]); await flush(); });
    if (mutation === "rename") {
      answers.chat_history_rename = async ({ id, title }) => { await changing.promise; records.get(id).title = title; };
      await rename(CURRENT, "Renamed conversation");
    } else {
      answers.chat_history_delete = async ({ id }) => { await changing.promise; records.delete(id); };
      await confirmDelete(CURRENT);
    }
    $(".history-btn").fire("click");
    seed(OTHER, "Newly saved conversation");
    const stale = historyPage();
    answers.chat_history_list = () => listing.promise;
    await history();
    delete answers.chat_history_list;
    changing.resolve();
    await flush();
    assert.ok(historyRow(OTHER), "the interrupted refresh must be retried so new saves remain reachable");
    listing.resolve(stale);
    await flush();
    assert.ok(historyRow(OTHER));
    if (mutation === "rename") assert.match(historyRow(CURRENT).textContent, /Renamed conversation/);
    else assert.equal(historyRow(CURRENT), undefined, "an old refresh cannot resurrect the deleted row");
  });
}

for (const phase of ["listening", "transcribing", "starting", "fetching transcript"]) {
  test(`history restoration ends audio while ${phase} and ignores its late work`, async (t) => {
    const audio = await import("../src/assistant/audio-mode.ts");
    const { CONTRACT_VERSION } = await import("../src/assistant/contracts.ts");
    audio.resetAudioModeForTests({ play: async () => {}, stop() {} });
    audio.registerAudioMode({ setPinned() {}, takeKeyboard() {} });
    t.after(() => { audio.stopAudioMode(); audio.resetAudioModeForTests(); });
    observeView(t);
    State.mode = "expanded";
    State.pendingApproval = null;
    const capture = Promise.withResolvers();
    const transcript = Promise.withResolvers();
    t.after(async () => { capture.resolve("old-audio"); transcript.resolve("Old spoken question"); await flush(); });
    answers.assistant_voice_hands_free = () => phase === "starting" ? capture.promise : "old-audio";
    answers.assistant_voice_converse = () => transcript.promise;
    const voiceEvent = (state, captureId = "old-audio") => emit("assistant-voice", {
      v: CONTRACT_VERSION, captureId, state, transcript: "Old spoken question",
    });
    seed(CURRENT, "Lisbon trip");
    const pending = deferredOpen(t, CURRENT);
    answers.chat_history_open = () => pending.promise;
    $(".audio-btn").fire("click");
    await flush();
    if (phase === "transcribing") voiceEvent("processing");
    if (phase === "fetching transcript") voiceEvent("preview");
    await flush();
    await history();
    assert.notEqual(audio.AudioMode.state, "off", "browsing history alone does not end audio");
    assert.equal($(".audio-row").hidden, false);
    await open(CURRENT);
    assert.equal(audio.AudioMode.state, "off", "selecting a conversation ends the old spoken turn immediately");
    assert.equal($(".audio-row").hidden, true, "the listening/transcribing indicator visibly disappears");
    assert.equal($(".audio-btn").classList.contains("on"), false, "the audio toggle visibly switches off");
    assert.equal($(".audio-btn").disabled, true, "audio cannot restart while restoration is pending");
    assert.deepEqual(sent("assistant_voice_cancel"), phase === "listening" || phase === "transcribing" ? [{ captureId: "old-audio" }] : []);
    assert.equal(sent("chat_send").length, 0);
    assert.deepEqual(State.chatHistory, []);

    pending.resolve(pending.saved);
    await flush();
    assert.equal(State.chatId, CURRENT);
    assert.equal(audio.AudioMode.state, "off", "history completion does not restart the microphone");
    assert.equal($(".audio-btn").disabled, false);
    answers.assistant_voice_hands_free = () => "new-audio";
    $(".audio-btn").fire("click");
    await flush();
    assert.equal(audio.AudioMode.state, "listening");
    capture.resolve("old-audio");
    transcript.resolve("Old spoken question");
    for (let i = 0; i < 4; i++) await flush();
    voiceEvent("preview");
    voiceEvent("idle");
    for (let i = 0; i < 4; i++) await flush();
    assert.equal(audio.AudioMode.state, "listening", "old capture replies and events cannot stop the new capture");
    assert.equal(sent("assistant_voice_converse").length, phase === "fetching transcript" ? 1 : 0);
    assert.equal(sent("assistant_voice_hands_free").length, 2, "only the user's two explicit starts request captures");
    assert.equal(sent("chat_send").length, 0, "late speech is never sent into the restored conversation");
    if (phase === "starting") assert.deepEqual(sent("assistant_voice_cancel"), [{ captureId: "old-audio" }], "the late startup is cancelled by its own capture ID");
    assert.ok(!sent("assistant_voice_cancel").some(({ captureId }) => captureId === "new-audio"));
    audio.stopAudioMode();
    await flush();
    assert.equal(sent("assistant_voice_cancel").at(-1).captureId, "new-audio");
  });
}

test("audio mode cannot start during history restoration, but can start afterward", async (t) => {
  const audio = await import("../src/assistant/audio-mode.ts");
  audio.resetAudioModeForTests({ play: async () => {}, stop() {} });
  audio.registerAudioMode({ setPinned() {}, takeKeyboard() {} });
  t.after(() => { audio.stopAudioMode(); audio.resetAudioModeForTests(); });
  observeView(t);
  State.mode = "expanded";
  State.pendingApproval = null;
  answers.assistant_voice_hands_free = () => "audio-after-history";
  seed(CURRENT, "Lisbon trip");
  const pending = deferredOpen(t, CURRENT);
  answers.chat_history_open = () => pending.promise;
  await open(CURRENT);
  assert.equal($(".audio-btn").disabled, true);
  $(".audio-btn").fire("click"); // A stale/programmatic click must also be refused.
  await flush();
  assert.equal(sent("assistant_voice_hands_free").length, 0);
  assert.equal(audio.AudioMode.state, "off");
  pending.resolve(pending.saved);
  await flush();
  assert.equal($(".audio-btn").disabled, false);
  $(".audio-btn").fire("click");
  await flush();
  assert.equal(sent("assistant_voice_hands_free").length, 1);
  assert.equal(audio.AudioMode.state, "listening");
  $(".audio-btn").fire("click");
  await flush();
  assert.equal(audio.AudioMode.state, "off");
});

for (const alreadyListening of [false, true]) {
  test(`a pending chat answer ${alreadyListening ? "keeps audio Stop usable" : "blocks starting audio mode"}`, async (t) => {
    const audio = await import("../src/assistant/audio-mode.ts");
    audio.resetAudioModeForTests({ play: async () => {}, stop() {} });
    audio.registerAudioMode({ setPinned() {}, takeKeyboard() {} });
    t.after(() => { audio.stopAudioMode(); audio.resetAudioModeForTests(); });
    observeView(t);
    State.mode = "expanded";
    State.pendingApproval = null;
    answers.assistant_voice_hands_free = () => "audio-during-answer";
    if (alreadyListening) await audio.startAudioMode();
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve({ text: "Finished" }); await flush(); });
    answers.chat_send = () => pending.promise;
    await send("A typed question");
    assert.equal($(".audio-btn").disabled, !alreadyListening);
    $(".audio-btn").fire("click");
    await flush();
    assert.equal(sent("assistant_voice_hands_free").length, alreadyListening ? 1 : 0);
    assert.equal(audio.AudioMode.state, "off");
    pending.resolve({ text: "Finished" });
    await flush();
    assert.equal($(".audio-btn").disabled, false);
  });
}

// Exercise the real region attachment handoff and chat persistence together.
import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import ts from "typescript";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";

registerHooks({
  load(url, context, nextLoad) {
    if (url.startsWith("file:") && url.endsWith(".ts") && !url.includes("/node_modules/")) {
      return { format: "module", shortCircuit: true, source: ts.transpileModule(readFileSync(new URL(url), "utf8"), {
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
      }).outputText };
    }
    return nextLoad(url, context);
  },
});
installFakeDom();
const { Island } = await import("../src/island/island.ts");
const { buildPrompt, newConversation } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
const { ChatActions, answerCard, registerChatActions, resetChatActionsForTests } = await import("../src/assistant/chat-actions.ts");
const nativeInvoke = internals.invoke;
let records, answers, view, host;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  if (cmd in answers) return answers[cmd](args);
  if (cmd === "chat_send") return { text: "An answer" };
  if (cmd === "chat_history_save") records.set(args.conversation.id, structuredClone(args.conversation));
  if (cmd === "chat_history_list") return [...records.values()].map(({ id, title, updatedAt, messages }) => ({ id, title, updatedAt, count: messages.length }));
  if (cmd === "chat_history_open") return structuredClone(records.get(args.id));
  return result;
};
const flush = () => new Promise(resolve => setTimeout(resolve, 0));
const $ = selector => view.el.querySelector(selector);
const attachment = { name: "Screen region.png", path: "/inbox/Screen region.png" };
async function send(text) {
  $(".chat-input").value = text;
  $(".send-btn").fire("click");
  await flush();
  view.sync();
}
async function open(id) {
  $(".history-btn").fire("click");
  await flush();
  view.el.find(".history-row").find(row => row.getAttribute("data-id") === id).fire("click");
  await flush();
  view.sync();
}
beforeEach(async () => {
  records = new Map();
  answers = {};
  resetChatActionsForTests(); registerChatActions();
  newConversation();
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.stateOverride = null;
  State.noteMessage = null;
  State.pendingApproval = null;
  State.isPinned = false;
  State.view = "prompt";
  State.mode = "expanded";
  host = Object.create(Island.prototype);
  host.setView = view => { State.view = view; State.notify(); };
  view = buildPrompt(() => {});
  await flush();
  view.sync();
  calls.length = 0;
});

for (const reopened of [false, true]) {
  test(`a confirmed region from a ${reopened ? "reopened" : "saved"} chat preserves the old record and starts fresh`, async () => {
    await send("Original question");
    await send("Original follow-up");
    const id = State.chatId;
    const original = structuredClone(records.get(id));
    if (reopened) {
      // Deliberately old metadata makes leaking the reopened title/date visible.
      original.title = "A renamed original conversation";
      original.createdAt = 123;
      records.set(id, structuredClone(original));
      await open(id);
      assert.ok($(".chat-restored"));
    }
    calls.length = 0;
    host.chatWithFile(attachment);
    view.sync();
    const resetId = State.chatId;
    assert.deepEqual(State.chatHistory, []);
    assert.deepEqual(State.droppedFile, attachment);
    assert.deepEqual(State.promptContext, { kind: "file", ...attachment });
    const restoredNotice = $(".chat-restored");
    assert.deepEqual(sent("chat_reset"), [{}]);
    assert.deepEqual(sent("chat_send"), [], "confirmation still leaves the question unsent");
    await send("What is in this screenshot?");
    assert.deepEqual(records.get(id), original, "every old message and its metadata survive");
    assert.equal(resetId, null, "the screenshot must not retain the old storage key");
    assert.equal(restoredNotice, null);
    assert.notEqual(State.chatId, id);
    assert.equal(records.size, 2);
    const fresh = records.get(State.chatId);
    assert.equal(fresh.title, "What is in this screenshot?");
    assert.ok(fresh.createdAt > 123);
    assert.deepEqual(fresh.messages.map(message => message.content), ["What is in this screenshot?", "An answer"]);
    assert.deepEqual(sent("chat_send").at(-1).context, { kind: "file", ...attachment });
    assert.ok(!JSON.stringify(fresh).includes(attachment.path), "the screenshot path stays out of persisted history");
  });
}

test("a region chat invalidates a delayed old save failure and preserves an approval", async t => {
  const delayed = Promise.withResolvers();
  t.after(async () => { delayed.resolve(); await flush(); });
  answers.chat_history_save = ({ conversation }) => {
    records.set(conversation.id, structuredClone(conversation));
    return delayed.promise;
  };
  await send("Original question");
  const id = State.chatId;
  const original = structuredClone(records.get(id));
  State.pendingApproval = { requestId: "approval-1" };
  State.isPinned = true;
  State.view = "approval";
  host.chatWithFile(attachment);
  delayed.reject(new Error("Old disk failure"));
  await flush();
  view.sync();
  assert.equal(State.view, "approval");
  assert.equal(State.isPinned, true);
  assert.equal(State.chatId, null);
  assert.deepEqual(State.droppedFile, attachment);
  assert.equal($(".chat-notice.warn"), null, "an old save cannot warn in the fresh region chat");
  assert.deepEqual(records.get(id), original);
});


test("a confirmed region retires the old chat action and rejects its late events", async t => {
  const delayed = Promise.withResolvers();
  t.after(async () => { delayed.resolve({ text: "Old answer" }); await flush(); });
  answers.chat_send = () => delayed.promise;
  await send("Open the old chat's app");
  const requestId = sent("chat_send").at(-1).requestId;
  const card = { requestId, actionId: "old-action", kind: "launch-app", label: "Old app", detail: "app",
    grant: { actionId: "old-action", digest: "a".repeat(64), expiresAtMs: Date.now() + 60_000 } };
  const note = { requestId, tool: "browser-tabs" };
  emit("chat-action", card); emit("chat-tool-used", note);
  assert.equal(ChatActions.cards.length, 1);
  assert.deepEqual(ChatActions.used, ["browser-tabs"]);

  host.chatWithFile(attachment);
  view.sync();
  assert.equal(sent("chat_stop").length, 1, "the old native request is cancelled before resetting");
  assert.deepEqual(ChatActions.cards, []);
  assert.deepEqual(ChatActions.used, []);
  answerCard(card.actionId, true);
  emit("chat-action", card); emit("chat-tool-used", note);
  assert.deepEqual(sent("chat_action_answer"), [], "a retained old confirmation cannot execute");
  assert.deepEqual(ChatActions.cards, []);
  assert.deepEqual(ChatActions.used, []);

  delayed.resolve({ text: "Old answer" });
  await flush(); view.sync();
  assert.deepEqual(State.chatHistory, []);
  assert.deepEqual(State.droppedFile, attachment);
  delete answers.chat_send;
  await send("Describe this region");
  assert.notEqual(sent("chat_send").at(-1).requestId, requestId);
  assert.deepEqual(State.chatHistory.map(message => message.content), ["Describe this region", "An answer"]);
});

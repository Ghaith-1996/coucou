// File-drop reset through the production island handler and chat view. Keep
// canvas animation out of the test, but retain the real State and bridge.
import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import ts from "typescript";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";

installFakeDom();
// Island's upload canvas uses parameter properties, which Node cannot strip.
registerHooks({
  load(url, context, nextLoad) {
    if (url.startsWith("file:") && url.endsWith(".ts") && !url.includes("/node_modules/")) {
      return {
        format: "module", shortCircuit: true,
        source: ts.transpileModule(readFileSync(new URL(url), "utf8"), {
          compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
        }).outputText,
      };
    }
    return nextLoad(url, context);
  },
});
const { Island } = await import("../src/island/island.ts");
const { buildPrompt } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
const { Bridge } = await import("../src/core/bridge.ts");
const { UploadSeq } = await import("../src/upload/sequence.ts");
const { ChatActions, beginToolNotes, registerChatActions, resetChatActionsForTests } = await import("../src/assistant/chat-actions.ts");
const flush = () => new Promise(resolve => setTimeout(resolve, 0));
function dropHost() {
  const host = Object.create(Island.prototype);
  host.engine = { gulp() {}, triggerEmote() {}, animateMorph() {} };
  host.ensureRunning = () => {};
  host.setView = name => { State.view = name; State.notify(); };
  return host;
}
let ingest;
const nativeInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await nativeInvoke(cmd, args);
  if (cmd === "ingest_file") return ingest.promise;
  return result;
};
registerChatActions();

beforeEach(async () => {
  await Bridge.chatReset();
  calls.length = 0;
  resetChatActionsForTests();
  State.settings = { ...DEFAULT_SETTINGS, chatModels: {} };
  State.paused = false;
  State.stateOverride = null;
  State.view = "prompt";
  State.droppedFile = null;
  State.promptContext = null;
  State.chatId = "c-0000000000000001";
  State.chatHistory = [
    { id: 1, role: "user", content: "What tabs are open?" },
    { id: 2, role: "assistant", content: "Your travel plans" },
  ];
  ingest = Promise.withResolvers();
});

test("dropping a file clears the old chat's tool notes before the file finishes importing", async (t) => {
  t.after(async () => {
    ingest.resolve({ name: "new.txt", path: "/inbox/new.txt" });
    await flush();
    UploadSeq.deactivate();
  });
  const view = buildPrompt(() => {});
  t.after(State.subscribe(() => view.sync()));
  await flush();
  emit("chat-tool-used", { requestId: beginToolNotes(), tool: "browser-tabs" });
  assert.ok(view.el.querySelector(".tool-note"));
  const host = dropHost();
  host.onDragDrop({ type: "drop", paths: ["/source/new.txt"] });
  assert.deepEqual(State.chatHistory, []);
  assert.equal(State.chatId, null);
  assert.deepEqual(ChatActions.used, []);
  assert.equal(view.el.querySelector(".tool-note"), null);
  assert.equal(State.promptContext.path, "/source/new.txt");
  await flush();
  assert.equal(sent("chat_reset").length, 1);
  assert.deepEqual(sent("ingest_file"), [{ path: "/source/new.txt" }]);
  ingest.resolve({ name: "new.txt", path: "/inbox/new.txt" });
  await flush();
  host.setView("prompt");
  assert.equal(State.promptContext.path, "/inbox/new.txt");
  assert.equal(view.el.querySelector(".tool-note"), null);
});

for (const usedBeforeReset of [false, true]) {
  test(`a file-drop reset ignores an old answer's late tool event ${usedBeforeReset ? "after clearing an existing note" : "before any note existed"}`, async (t) => {
    const answer = Promise.withResolvers();
    const nextAnswer = Promise.withResolvers();
    t.after(async () => {
      answer.resolve({ text: "Old answer" });
      nextAnswer.resolve({ text: "New answer" });
      ingest.resolve({ name: "new.txt", path: "/inbox/new.txt" });
      await flush();
      UploadSeq.deactivate();
    });
    const invoke = internals.invoke;
    t.mock.method(internals, "invoke", async (cmd, args) => {
      if (cmd === "chat_send") {
        // Preserve command recording without awaiting its eventual answer.
        await nativeInvoke(cmd, args);
        return sent("chat_send").length === 1 ? answer.promise : nextAnswer.promise;
      }
      return invoke(cmd, args);
    });
    const view = buildPrompt(() => {});
    t.after(State.subscribe(() => view.sync()));
    await flush();
    const input = view.el.querySelector(".chat-input");
    input.value = "Read the browser tabs";
    view.el.querySelector(".send-btn").fire("click");
    await flush();
    assert.equal(sent("chat_send").length, 1);
    const requestId = sent("chat_send")[0].requestId;
    assert.match(requestId, /^[a-f0-9]{32}$/, "the real chat view passes its request identity through the bridge");
    if (usedBeforeReset) {
      emit("chat-tool-used", { requestId, tool: "browser-tabs" });
      assert.ok(view.el.querySelector(".tool-note"), "the current answer's tool event is shown");
    }
    const host = dropHost();
    host.onDragDrop({ type: "drop", paths: ["/source/new.txt"] });
    assert.deepEqual(ChatActions.used, []);
    // tabs() can finish after chat_reset increments the native history epoch;
    // chat_reset does not cancel the old tool future.
    emit("chat-tool-used", { requestId, tool: "browser-tabs" });
    assert.deepEqual(ChatActions.used, [], "a late tool result belongs to the discarded transcript");
    assert.equal(view.el.querySelector(".tool-note"), null);
    answer.resolve({ text: "Old answer" });
    await flush();
    host.setView("prompt");
    assert.deepEqual(State.chatHistory, []);
    assert.equal(view.el.querySelector(".tool-note"), null);
    input.value = "Read tabs in this new chat";
    view.el.querySelector(".send-btn").fire("click");
    await flush();
    assert.equal(sent("chat_send").length, 2);
    const nextRequest = sent("chat_send")[1].requestId;
    assert.notEqual(nextRequest, requestId);
    // Event delivery ordering is not assumed: an older event can still be
    // queued when a newer native request starts.
    emit("chat-tool-used", { requestId, tool: "browser-tabs" });
    assert.deepEqual(ChatActions.used, []);
    assert.equal(view.el.querySelector(".tool-note"), null);
    emit("chat-tool-used", { requestId: nextRequest, tool: "browser-tabs" });
    assert.deepEqual(ChatActions.used, ["browser-tabs"]);
    assert.ok(view.el.querySelector(".tool-note"));
    nextAnswer.resolve({ text: "New answer" });
    await flush();
    assert.equal(State.chatHistory.at(-1).content, "New answer");
    assert.ok(view.el.querySelector(".tool-note"), "valid notes remain visible with their completed answer");
  });
}

for (const cardShownBeforeReset of [false, true]) {
  test(`a file drop retires ${cardShownBeforeReset ? "visible" : "queued"} action cards before its delayed reset settles`, async (t) => {
    const answer = Promise.withResolvers();
    const nextAnswer = Promise.withResolvers();
    const reset = Promise.withResolvers();
    t.after(async () => {
      reset.resolve();
      answer.resolve({ text: "Old answer" });
      nextAnswer.resolve({ text: "New answer" });
      ingest.resolve({ name: "new.txt", path: "/inbox/new.txt" });
      await flush();
      UploadSeq.deactivate();
    });
    const invoke = internals.invoke;
    t.mock.method(internals, "invoke", async (cmd, args) => {
      if (cmd === "chat_send") {
        await nativeInvoke(cmd, args);
        return sent("chat_send").length === 1 ? answer.promise : nextAnswer.promise;
      }
      if (cmd === "chat_reset") {
        await nativeInvoke(cmd, args);
        return reset.promise;
      }
      if (cmd === "chat_stop") {
        await nativeInvoke(cmd, args);
        if (cardShownBeforeReset) answer.resolve({ text: "Old answer" });
        else answer.reject(new Error("Stopped old answer"));
        return null;
      }
      return invoke(cmd, args);
    });
    const view = buildPrompt(() => {});
    t.after(State.subscribe(() => view.sync()));
    await flush();
    const input = view.el.querySelector(".chat-input");
    const send = view.el.querySelector(".send-btn");
    input.value = "Open the old browser tab";
    send.fire("click");
    await flush();
    const requestId = sent("chat_send")[0].requestId;
    const oldCard = {
      requestId, actionId: "old-action", kind: "switch-tab", label: "Old tab", detail: "Chrome",
      grant: { actionId: "old-action", digest: "ab".repeat(32), expiresAtMs: 9e12 },
    };
    let oldButton;
    if (cardShownBeforeReset) {
      emit("chat-action", oldCard);
      oldButton = view.el.querySelector(".action-card").querySelector(".btn.primary");
      assert.ok(oldButton, "the old answer has a live confirmation button");
    }
    const host = dropHost();
    host.onDragDrop({ type: "drop", paths: ["/source/new.txt"] });
    assert.deepEqual(ChatActions.cards, [], "cards retire synchronously before either IPC settles");
    assert.equal(view.el.querySelector(".action-card"), null);
    assert.deepEqual(State.chatHistory, []);
    assert.equal(sent("chat_stop").length, 1, "file drop stops the typed answer as well as resetting history");
    assert.equal(sent("chat_reset").length, 1);
    oldButton?.fire("click");
    emit("chat-action", oldCard);
    assert.deepEqual(ChatActions.cards, [], "queued old cards cannot reappear while reset is pending");
    assert.deepEqual(sent("chat_action_answer"), [], "a detached Do it button cannot send its old grant");
    await flush();
    assert.deepEqual(State.chatHistory, [], "the cancelled old completion cannot append its answer");
    assert.equal(State.view, "uploading", "the cancelled old rejection cannot replace the upload with an error");

    host.setView("prompt");
    input.value = "Open a tab in the new conversation";
    send.fire("click");
    await flush();
    assert.equal(State.chatHistory.at(-1)?.content, "Open a tab in the new conversation");
    assert.equal(sent("chat_send").length, 1, "the new send waits for the in-flight native reset");
    emit("chat-action", oldCard);
    assert.deepEqual(ChatActions.cards, []);
    reset.resolve();
    await flush();
    assert.equal(sent("chat_send").length, 2);
    const nextRequest = sent("chat_send")[1].requestId;
    assert.notEqual(nextRequest, requestId);
    emit("chat-action", oldCard);
    assert.deepEqual(ChatActions.cards, [], "starting a newer native request cannot revive the old request");
    const currentCard = {
      ...oldCard, requestId: nextRequest, actionId: "new-action", label: "New tab",
      grant: { ...oldCard.grant, actionId: "new-action" },
    };
    emit("chat-action", currentCard);
    assert.equal(ChatActions.cards.length, 1);
    view.el.querySelector(".action-card").querySelector(".btn.primary").fire("click");
    await flush();
    assert.deepEqual(sent("chat_action_answer"), [{ actionId: "new-action", grant: currentCard.grant }]);
    nextAnswer.resolve({ text: "New answer" });
    await flush();
    assert.equal(State.chatHistory.at(-1).content, "New answer");
    assert.deepEqual(ChatActions.cards, [], "completion also retires a card before its delayed close event");
    emit("chat-action", currentCard);
    assert.deepEqual(ChatActions.cards, [], "completed requests reject delayed proposal events");
  });
}

for (const outcome of ["success", "failure"]) {
  test(`a delayed old-view ${outcome} cannot retire a newer view's card after file drop`, async (t) => {
    const answer = Promise.withResolvers();
    const nextAnswer = Promise.withResolvers();
    t.after(async () => {
      answer.resolve({ text: "Old answer" });
      nextAnswer.resolve({ text: "New answer" });
      ingest.resolve({ name: "new.txt", path: "/inbox/new.txt" });
      await flush();
      UploadSeq.deactivate();
    });
    const invoke = internals.invoke;
    t.mock.method(internals, "invoke", async (cmd, args) => {
      if (cmd === "chat_send") {
        await nativeInvoke(cmd, args);
        return sent("chat_send").length === 1 ? answer.promise : nextAnswer.promise;
      }
      // Native Stop is acknowledged; only its old command reply is delayed.
      return invoke(cmd, args);
    });
    const oldView = buildPrompt(() => {});
    oldView.el.querySelector(".chat-input").value = "Old question";
    oldView.el.querySelector(".send-btn").fire("click");
    await flush();
    const host = dropHost();
    host.onDragDrop({ type: "drop", paths: ["/source/new.txt"] });
    await flush();
    host.setView("prompt");
    const view = buildPrompt(() => {});
    t.after(State.subscribe(() => view.sync()));
    view.el.querySelector(".chat-input").value = "New question";
    view.el.querySelector(".send-btn").fire("click");
    await flush();
    assert.equal(sent("chat_send").length, 2);
    const currentCard = {
      requestId: sent("chat_send")[1].requestId,
      actionId: "new-view-action", kind: "launch-app", label: "Calculator", detail: "app",
      grant: { actionId: "new-view-action", digest: "cd".repeat(32), expiresAtMs: 9e12 },
    };
    emit("chat-action", currentCard);
    if (outcome === "success") answer.resolve({ text: "Old answer" });
    else answer.reject(new Error("Stopped old answer"));
    await flush();
    assert.equal(State.chatHistory.length, 1);
    assert.equal(State.chatHistory[0].content, "New question");
    assert.equal(State.view, "prompt");
    assert.equal(State.stateOverride, "thinking", "the old finally cannot end the new answer's thinking state");
    assert.equal(ChatActions.cards[0]?.actionId, currentCard.actionId);
    view.el.querySelector(".action-card").querySelector(".btn.primary").fire("click");
    await flush();
    assert.deepEqual(sent("chat_action_answer"), [{ actionId: currentCard.actionId, grant: currentCard.grant }]);
    nextAnswer.resolve({ text: "New answer" });
    await flush();
    assert.equal(State.chatHistory.at(-1).content, "New answer");
  });
}

test("a file drop triggered while beginning a request cannot send the discarded query", async (t) => {
  t.after(async () => {
    ingest.resolve({ name: "new.txt", path: "/inbox/new.txt" });
    await flush();
    UploadSeq.deactivate();
  });
  const invoke = internals.invoke;
  t.mock.method(internals, "invoke", async (cmd, args) => {
    if (cmd === "chat_send") {
      await nativeInvoke(cmd, args);
      return { text: "New answer" };
    }
    return invoke(cmd, args);
  });
  const view = buildPrompt(() => {});
  t.after(State.subscribe(() => view.sync()));
  await flush();
  emit("chat-tool-used", { requestId: beginToolNotes(), tool: "browser-tabs" });
  const host = dropHost();
  let dropped = false;
  t.after(State.subscribe(() => {
    if (dropped || ChatActions.used.length) return;
    dropped = true;
    host.onDragDrop({ type: "drop", paths: ["/source/new.txt"] });
  }));
  const input = view.el.querySelector(".chat-input");
  const send = view.el.querySelector(".send-btn");
  input.value = "This query belongs to the discarded conversation";
  send.fire("click");
  assert.equal(dropped, true, "beginToolNotes notifies while clearing the previous answer's note");
  await flush();
  assert.deepEqual(sent("chat_send"), [], "the reset cannot be adopted as the old query's new transcript");
  assert.deepEqual(State.chatHistory, []);
  assert.deepEqual(ChatActions.cards, []);
  assert.deepEqual(ChatActions.used, []);
  assert.equal(sent("chat_stop").length, 1);
  assert.equal(sent("chat_reset").length, 1);
  host.setView("prompt");
  input.value = "New query";
  send.fire("click");
  await flush();
  assert.equal(sent("chat_send").length, 1, "the cancelled send releases the composer for the new conversation");
  assert.equal(sent("chat_send")[0].query, "New query");
  assert.equal(State.chatHistory.at(-1).content, "New answer");
});

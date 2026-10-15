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
  const host = Object.create(Island.prototype);
  host.engine = { gulp() {}, triggerEmote() {}, animateMorph() {} };
  host.ensureRunning = () => {};
  host.setView = name => { State.view = name; State.notify(); };
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
    const host = Object.create(Island.prototype);
    host.engine = { gulp() {}, triggerEmote() {}, animateMorph() {} };
    host.ensureRunning = () => {};
    host.setView = name => { State.view = name; State.notify(); };
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

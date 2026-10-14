// The chat on the ChatGPT plan (core/chatgpt.ts, views/chat.ts) and the
// saved conversations (core/conversations.ts), on a fake DOM through the real
// bridge. Rust is the mock in tests/tauri.mjs: what it would answer, and what
// the island asked of it, never a token.

import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";

installFakeDom();
const { buildPrompt, newConversation } = await import("../src/views/chat.ts");
const { DEFAULT_SETTINGS, State } = await import("../src/core/state.ts");
const { EFFORT_LABELS, LUNA, SOL, effortFor, effortsFor, withEffort } = await import("../src/core/chatgpt.ts");
const { cleanTitle, fromSaved, newChatId, titleFor, toSaved } = await import("../src/core/conversations.ts");
const { highlightTokens } = await import("../src/views/markdown.ts");

const table = JSON.parse(readFileSync(new URL("./fixtures/chatgpt-efforts.json", import.meta.url), "utf8"));

// ── Pure helpers ─────────────────────────────────────────────────────────────

test("each model offers exactly the efforts Rust accepts (shared fixture)", () => {
  for (const [model, efforts] of Object.entries(table.models)) assert.deepEqual([...effortsFor(model)], efforts, model);
  for (const [value, label] of Object.entries(table.labels)) assert.equal(EFFORT_LABELS[value], label);
  assert.ok(!effortsFor(SOL).includes("none"), "GPT-6.1 Sol has no None");
});

test("the effort is remembered per model, Medium by default, never an unsupported one", () => {
  const s = { ...DEFAULT_SETTINGS };
  assert.equal(effortFor(s, LUNA), "medium");
  assert.equal(effortFor(s, SOL), "medium");
  const luna = withEffort(s, LUNA, "none");
  assert.equal(effortFor(luna, LUNA), "none");
  assert.equal(effortFor(luna, SOL), "medium", "per model");
  assert.equal(withEffort(s, SOL, "none"), null, "refused for Sol");
  assert.equal(withEffort(s, LUNA, "ultra"), null);
  const sol = withEffort(luna, SOL, "max");
  assert.deepEqual(sol.chatEfforts, { [LUNA]: "none", [SOL]: "max" });
  // A value saved by hand that the model doesn't take falls back to Medium.
  assert.equal(effortFor({ ...s, chatEfforts: { [SOL]: "none" } }, SOL), "medium");
});

test("conversation ids are what Rust accepts; titles are one short line", () => {
  for (let i = 0; i < 20; i++) assert.match(newChatId(), /^c-[0-9a-f]{16}$/);
  assert.notEqual(newChatId(), newChatId());
  const msgs = [{ id: 1, role: "user", content: "  Plan my\ntrip to   Lisbon  " }, { id: 2, role: "assistant", content: "Sure" }];
  assert.equal(titleFor(msgs), "Plan my trip to Lisbon");
  assert.equal([...titleFor([{ id: 1, role: "user", content: "é".repeat(200) }])].length, 60);
  assert.equal(cleanTitle("  \n "), null);
  assert.equal(cleanTitle("New\tname"), "New name");
  const saved = toSaved("c-0123456789abcdef", "T", 5, [{ id: 7, role: "assistant", content: "x", model: SOL, effort: "max" }]);
  assert.deepEqual(saved.messages, [{ role: "assistant", content: "x", model: SOL, effort: "max" }], "island ids stay in the island");
  assert.deepEqual(fromSaved(saved, 40)[0], { role: "assistant", content: "x", model: SOL, effort: "max", id: 40 });
});

test("code is coloured without changing a character", () => {
  const code = 'fn main() {\n    let s = "hi // not a comment";\n    // done\n    return 42;\n}';
  const tokens = highlightTokens(code, "rust");
  assert.equal(tokens.map((t) => t.text).join(""), code);
  const kinds = (k) => tokens.filter((t) => t.kind === k).map((t) => t.text.trim());
  assert.ok(kinds("k").includes("fn") && kinds("k").includes("return"));
  assert.ok(kinds("s").includes('"hi // not a comment"'), "a comment marker inside a string stays a string");
  assert.ok(kinds("c").includes("// done"));
  assert.ok(kinds("n").includes("42"));
  assert.equal(highlightTokens("# x = 1", "python")[0].kind, "c");
  assert.notEqual(highlightTokens("# heading", "js")[0].kind, "c", "# is a comment only where it is one");
  assert.equal(highlightTokens('"unterminated', "js").map((t) => t.text).join(""), '"unterminated');
});

// ── The chat view on ChatGPT ─────────────────────────────────────────────────

let answers;
const plainInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await plainInvoke(cmd, args);
  if (cmd in answers) {
    const a = typeof answers[cmd] === "function" ? answers[cmd](args) : answers[cmd];
    if (a instanceof Error) throw a;
    return a;
  }
  return result;
};
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const CONNECTED = { connected: true, accountId: "test-client", email: "me@example.com", planEnabled: true, needsReconnect: false, showWelcome: false, manageUsageUrl: "https://chatgpt.com/#settings" };
const CATALOG = [
  { id: LUNA, label: "GPT-6 Luna", available: true, efforts: ["none", "low", "medium", "high", "xhigh", "max"] },
  { id: SOL, label: "GPT-6.1 Sol", available: true, efforts: ["low", "medium", "high", "xhigh", "max"] },
];

let view;
beforeEach(async () => {
  calls.length = 0;
  answers = { chatgpt_status: CONNECTED, chatgpt_models: CATALOG, chat_history_list: [] };
  State.settings = { ...DEFAULT_SETTINGS, chatProvider: "chatgpt", chatModels: {}, chatEfforts: {} };
  State.chatHistory = [];
  State.chatId = null;
  State.stateOverride = null;
  State.noteMessage = "";
  State.view = "prompt";
  State.droppedFile = null;
  view = buildPrompt(() => {});
  await flush();
  view.sync();
});

const $ = (cls) => view.el.querySelector(cls);
const options = () => $(".effort-select").find("OPTION").map((o) => [o.value ?? o.getAttribute("value"), o.textContent]);

test("not signed in: the picker offers Continue with ChatGPT, and nothing is asked of the API", async () => {
  answers.chatgpt_status = { ...CONNECTED, connected: false, email: null };
  $(".model-btn").fire("click");
  await flush();
  assert.equal($(".chatgpt-connect").textContent, "Continue with ChatGPT");
  assert.deepEqual(sent("chatgpt_models"), []);
  answers.chatgpt_connect = () => {
    // Rust's status changes with the sign-in.
    answers.chatgpt_status = CONNECTED;
    return CONNECTED;
  };
  $(".chatgpt-connect").fire("click");
  for (let i = 0; i < 6; i++) await flush();
  assert.equal(sent("chatgpt_connect").length, 1);
  assert.equal(sent("chatgpt_models").length, 1, "the catalog once signed in");
});

// The native command emits chatgpt-status on success, failure and cancellation.
// Exercise both deliveries, because the event and command use separate channels.
const SIGNED_OUT = { ...CONNECTED, connected: false, accountId: null, email: null };
async function beginSignIn(t, initial = SIGNED_OUT) {
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve(CONNECTED); await flush(); });
  answers.chatgpt_status = initial;
  answers.chatgpt_connect = () => pending.promise;
  $(".model-btn").fire("click");
  await flush();
  $(".chatgpt-connect").fire("click");
  await flush();
  return pending;
}

for (const order of ["before", "after"]) {
  for (const error of ["Sign-in failed. Try again.", "Sign-in cancelled."]) {
    test(`the initiating picker preserves ${error} when status arrives ${order} rejection`, async (t) => {
      const pending = await beginSignIn(t);
      if (order === "before") emit("chatgpt-status", SIGNED_OUT);
      pending.reject(new Error(error));
      await flush();
      if (order === "after") emit("chatgpt-status", SIGNED_OUT);
      await flush();
      assert.ok($(".picker").classList.contains("on"), "the failed attempt stays visible");
      assert.match($(".picker-list").textContent, new RegExp(error.replace(/[.]/g, "\\.")));
      assert.equal($(".chatgpt-connect").textContent, "Continue with ChatGPT", "retry remains available");
      assert.equal(sent("chatgpt_models").length, 0);
      answers.chatgpt_status = CONNECTED;
      answers.chatgpt_connect = CONNECTED;
      $(".chatgpt-connect").fire("click");
      await flush();
      assert.equal(view.el.find(".picker-model").length, 2, "retry can succeed");
    });
  }

  test(`a successful initiating login refreshes its catalog with status ${order} settlement`, async (t) => {
    const pending = await beginSignIn(t);
    answers.chatgpt_status = CONNECTED;
    if (order === "before") emit("chatgpt-status", CONNECTED);
    pending.resolve(CONNECTED);
    await flush();
    if (order === "after") emit("chatgpt-status", CONNECTED);
    await flush();
    assert.ok($(".picker").classList.contains("on"));
    assert.deepEqual(view.el.find(".picker-model").map((row) => row.disabled), [false, false]);
    assert.doesNotMatch($(".picker-list").textContent, /Finish signing in/);
  });
}

for (const order of ["before", "after"]) {
  test(`a first registration storage failure survives its newly issued account status ${order} rejection`, async (t) => {
    const pending = await beginSignIn(t);
    const partial = { ...SIGNED_OUT, accountId: "issued-client", needsReconnect: true, hasSavedCredentials: true };
    answers.chatgpt_status = partial;
    if (order === "before") emit("chatgpt-status", partial);
    pending.reject(new Error("Could not save ChatGPT credentials"));
    await flush();
    if (order === "after") emit("chatgpt-status", partial);
    assert.ok($(".picker").classList.contains("on"));
    assert.match($(".picker-list").textContent, /Could not save ChatGPT credentials/);
    assert.equal($(".chatgpt-connect").textContent, "Reconnect");
    assert.equal(sent("chatgpt_models").length, 0);
  });
}

test("a failed first registration binds its error to the issued account", async (t) => {
  const pending = await beginSignIn(t);
  const issued = { ...SIGNED_OUT, accountId: "issued-client", needsReconnect: true };
  emit("chatgpt-status", issued);
  pending.reject(new Error("Issued registration failed"));
  await flush();
  assert.match($(".picker-list").textContent, /Issued registration failed/);
  const selected = { ...issued, accountId: "another-client" };
  answers.chatgpt_status = selected;
  emit("chatgpt-status", selected);
  await flush();
  assert.equal($(".picker").classList.contains("on"), false, "another registration invalidates the old error");
  $(".model-btn").fire("click");
  await flush();
  assert.doesNotMatch($(".picker-list").textContent, /Issued registration failed/);
});

for (const order of ["before", "after"]) {
  test(`same-account recovery ${order} login rejection clears the stale error`, async (t) => {
    const pending = await beginSignIn(t, { ...CONNECTED, connected: false, needsReconnect: true });
    answers.chatgpt_status = CONNECTED;
    if (order === "before") emit("chatgpt-status", CONNECTED);
    pending.reject(new Error("Earlier sign-in failed"));
    await flush();
    if (order === "after") emit("chatgpt-status", CONNECTED);
    await flush();
    assert.ok($(".picker").classList.contains("on"));
    assert.doesNotMatch($(".picker-list").textContent, /Earlier sign-in failed/);
    assert.equal(view.el.find(".picker-model").length, 2);
  });
}

for (const order of ["before", "after"]) {
  test(`a reconnect started from a usable account clears its error after recovery, failed status ${order} rejection`, async (t) => {
    const pending = Promise.withResolvers();
    t.after(async () => { pending.resolve(CONNECTED); await flush(); });
    answers.chatgpt_models = new Error("Catalog unavailable; reconnect");
    $(".model-btn").fire("click");
    await flush();
    answers.chatgpt_connect = () => pending.promise;
    $(".chatgpt-connect").fire("click");
    await flush();
    const failed = { ...CONNECTED, connected: false, needsReconnect: true };
    if (order === "before") emit("chatgpt-status", failed);
    pending.reject(new Error("Reconnect storage failed"));
    await flush();
    if (order === "after") emit("chatgpt-status", failed);
    assert.match($(".picker-list").textContent, /Reconnect storage failed/);
    answers.chatgpt_models = CATALOG;
    answers.chatgpt_status = CONNECTED;
    emit("chatgpt-status", CONNECTED);
    await flush();
    assert.ok($(".picker").classList.contains("on"));
    assert.doesNotMatch($(".picker-list").textContent, /Reconnect storage failed/);
    assert.equal(view.el.find(".picker-model").length, 2);
  });
}

for (const outcome of ["success", "failure"]) {
  test(`a status after login fences an older catalog ${outcome}`, async (t) => {
    const pending = await beginSignIn(t);
    const oldCatalog = Promise.withResolvers();
    t.after(async () => { oldCatalog.resolve(CATALOG); await flush(); });
    answers.chatgpt_status = CONNECTED;
    answers.chatgpt_models = () => oldCatalog.promise;
    pending.resolve(CONNECTED);
    await flush();
    answers.chatgpt_models = [CATALOG[0], { ...CATALOG[1], available: false }];
    emit("chatgpt-status", CONNECTED);
    await flush();
    if (outcome === "success") oldCatalog.resolve(CATALOG);
    else oldCatalog.reject(new Error("Older catalog failed"));
    await flush();
    assert.ok($(".picker").classList.contains("on"));
    assert.equal(view.el.find(".picker-model")[1].disabled, true);
    assert.doesNotMatch($(".picker-list").textContent, /Older catalog failed/);
  });
}

for (const outcome of ["success", "failure"]) {
  test(`a login ${outcome} cannot overwrite a concurrent different account`, async (t) => {
    const pending = await beginSignIn(t);
    const newer = { ...CONNECTED, accountId: "other-client", email: "other@example.com" };
    answers.chatgpt_status = newer;
    answers.chatgpt_models = [CATALOG[0], { ...CATALOG[1], available: false }];
    emit("chatgpt-status", newer);
    if (outcome === "success") pending.resolve(CONNECTED);
    else pending.reject(new Error("Old login failed"));
    await flush();
    if (!$(".picker").classList.contains("on")) $(".model-btn").fire("click");
    await flush();
    assert.match($(".picker-list").textContent, /other@example.com/);
    assert.doesNotMatch($(".picker-list").textContent, /Old login failed/);
    assert.equal(view.el.find(".picker-model")[1].disabled, true);
  });

  test(`a closed login ${outcome} cannot replace the reopened picker`, async (t) => {
    const pending = await beginSignIn(t);
    $(".model-btn").fire("click");
    answers.chatgpt_status = CONNECTED;
    $(".model-btn").fire("click");
    await flush();
    const rows = view.el.find(".picker-model");
    if (outcome === "success") pending.resolve(SIGNED_OUT);
    else pending.reject(new Error("Closed login failed"));
    await flush();
    assert.ok($(".picker").classList.contains("on"));
    assert.deepEqual(view.el.find(".picker-model"), rows);
    assert.doesNotMatch($(".picker-list").textContent, /Closed login failed/);
  });
}

for (const provider of ["anthropic", "ollama"]) {
  for (const outcome of ["success", "failure"]) {
    test(`a ChatGPT status event preserves a pending ${provider} catalog ${outcome}`, async (t) => {
      const pending = Promise.withResolvers();
      t.after(async () => { pending.resolve([]); await flush(); });
      State.settings = { ...State.settings, chatProvider: provider };
      answers.secret_present = true;
      answers.chat_models = () => pending.promise;
      $(".model-btn").fire("click");
      await flush();
      assert.match($(".picker-list").textContent, /Loading models/);
      emit("chatgpt-status", SIGNED_OUT);
      if (outcome === "success") pending.resolve([{ id: "provider-model", label: "Provider model" }]);
      else pending.reject(new Error("Provider catalog unavailable"));
      await flush();
      assert.ok($(".picker").classList.contains("on"));
      assert.doesNotMatch($(".picker-list").textContent, /Loading models/);
      if (outcome === "success") assert.deepEqual(view.el.find(".picker-model").map((row) => row.textContent), ["Provider model"]);
      else assert.match($(".picker-list").textContent, /Provider catalog unavailable/);
    });
  }
}

test("both models are offered; one the account lacks is shown unavailable, never swapped", async () => {
  answers.chatgpt_models = [{ ...CATALOG[0], available: false }, CATALOG[1]];
  $(".model-btn").fire("click");
  await flush();
  await flush();
  const rows = view.el.find(".picker-model");
  assert.deepEqual(rows.map((r) => r.textContent.replace("Unavailable", " (unavailable)")), ["GPT-6 Luna (unavailable)", "GPT-6.1 Sol"]);
  assert.equal(rows[0].disabled, true);
  rows[0].fire("click");
  assert.equal(State.settings.chatModels.chatgpt, undefined, "an unavailable model can't be picked");
  rows[1].fire("click");
  assert.equal(State.settings.chatModels.chatgpt, SOL);
  assert.match(view.el.find(".picker-status")[0]?.textContent ?? "", /Using ChatGPT plan/);
});

test("the effort menu follows the model: six for Luna, five for Sol, and it is remembered", async () => {
  State.settings = { ...State.settings, chatModels: { chatgpt: LUNA } };
  view.sync();
  assert.equal($(".effort-select").hidden, false);
  assert.deepEqual(options().map(([v]) => v), ["none", "low", "medium", "high", "xhigh", "max"]);
  assert.deepEqual(options().map(([, label]) => label), ["None", "Low", "Medium", "High", "Extra High", "Max"]);
  assert.equal($(".effort-select").value, "medium", "Medium by default");
  $(".effort-select").value = "low";
  $(".effort-select").fire("change");
  assert.equal(State.settings.chatEfforts[LUNA], "low");
  assert.equal(sent("save_settings").at(-1).settings.chatEfforts[LUNA], "low");

  State.settings = { ...State.settings, chatModels: { chatgpt: SOL } };
  view.sync();
  assert.deepEqual(options().map(([v]) => v), ["low", "medium", "high", "xhigh", "max"], "no None for Sol");
  // Another provider: no effort menu.
  State.settings = { ...State.settings, chatProvider: "anthropic" };
  view.sync();
  assert.equal($(".effort-select").hidden, true);
});

test("each answer records which model and effort wrote it; switching keeps the conversation", async () => {
  State.settings = { ...State.settings, chatModels: { chatgpt: LUNA }, chatEfforts: { [LUNA]: "low" } };
  view.sync();
  answers.chat_send = () => ({ text: "first" });
  $(".chat-input").value = "hello";
  $(".send-btn").fire("click");
  await flush();
  State.settings = { ...State.settings, chatModels: { chatgpt: SOL }, chatEfforts: { [LUNA]: "low", [SOL]: "max" } };
  answers.chat_send = () => ({ text: "second" });
  $(".chat-input").value = "more";
  $(".send-btn").fire("click");
  await flush();
  const answersSeen = State.chatHistory.filter((m) => m.role === "assistant").map((m) => [m.model, m.effort, m.content]);
  assert.deepEqual(answersSeen, [[LUNA, "low", "first"], [SOL, "max", "second"]]);
  assert.equal(State.chatHistory.length, 4, "nothing erased by the switch");
  assert.equal(sent("chat_reset").length, 0);
  view.sync();
  assert.deepEqual(view.el.find(".reply-sig").map((e) => e.textContent), ["GPT-6 Luna · Low", "GPT-6.1 Sol · Max"]);
  // Saved after each answer, with the same id.
  const saves = sent("chat_history_save").map((a) => a.conversation);
  assert.equal(saves.length, 2);
  assert.match(saves[0].id, /^c-[0-9a-f]{16}$/);
  assert.equal(saves[1].id, saves[0].id);
  assert.equal(saves[1].title, "hello");
  assert.deepEqual(saves[1].messages.map((m) => m.role), ["user", "assistant", "user", "assistant"]);
});

test("Stop ends the answer: what arrived stays, marked, and the model never sees it again", async () => {
  let reject;
  answers.chat_send = () => new Promise((_, no) => (reject = no));
  answers.chat_stop = () => reject(new Error("Stopped."));
  $(".chat-input").value = "long one";
  $(".send-btn").fire("click");
  await flush();
  assert.ok($(".send-btn").classList.contains("stop"), "the send button is Stop while it answers");
  emit("chat-delta", "partial ans");
  $(".send-btn").fire("click");
  await flush();
  await flush();
  assert.equal(sent("chat_stop").length, 1);
  const last = State.chatHistory.at(-1);
  assert.deepEqual([last.role, last.content, last.stopped], ["assistant", "partial ans", true]);
  assert.equal(State.view, "prompt", "no error page for a Stop");
  assert.ok(!$(".send-btn").classList.contains("stop"));
});

test("Regenerate asks the last question again, after taking the last turn out of the model's history", async () => {
  answers.chat_send = () => ({ text: "one" });
  $(".chat-input").value = "why?";
  $(".send-btn").fire("click");
  await flush();
  view.sync();
  answers.chat_undo_last = true;
  answers.chat_send = () => ({ text: "two" });
  const regenerate = view.el.find(".md-copy").find((b) => b.title === "Regenerate" || String(b.getAttribute("aria-label")) === "Regenerate");
  regenerate.fire("click");
  await flush();
  await flush();
  assert.equal(sent("chat_undo_last").length, 1);
  assert.deepEqual(sent("chat_send").map((a) => a.query), ["why?", "why?"]);
  assert.deepEqual(State.chatHistory.map((m) => m.content), ["why?", "two"]);
});

test("the usage limit shows its notice with Manage usage, not an error page", async () => {
  answers.chat_send = () => {
    emit("chatgpt-usage-limit", null);
    return Promise.reject(new Error("Usage limit reached. Review your plan or this app's limit in ChatGPT settings."));
  };
  $(".chat-input").value = "hi";
  $(".send-btn").fire("click");
  await flush();
  view.sync();
  assert.equal(State.view, "prompt");
  assert.equal($(".chat-notice-title").textContent, "Usage limit reached");
  const manage = view.el.find(".picker-link").find((b) => b.textContent === "Manage usage");
  manage.fire("click");
  assert.deepEqual(sent("open_url").at(-1), { url: "https://chatgpt.com/#settings" });
});

const usageLimitReply = () => {
  emit("chatgpt-usage-limit", null);
  return Promise.reject(new Error("Usage limit reached. Review your plan or this app's limit in ChatGPT settings."));
};

async function sendQuestion(query = "try again") {
  $(".chat-input").value = query;
  $(".send-btn").fire("click");
  await flush();
  view.sync();
}

async function reachUsageLimit() {
  answers.chat_send = usageLimitReply;
  await sendQuestion("first question");
  assert.equal(State.view, "prompt");
  assert.equal($(".chat-notice-title").textContent, "Usage limit reached");
}

for (const [kind, message] of [
  ["connectivity", "Could not connect to ChatGPT. Check your connection and try again."],
  ["session", "ChatGPT didn't accept the sign-in. Reconnect in Settings."],
  ["model", "This model isn't available on your ChatGPT account."],
]) {
  test(`a usage limit cannot hide the next ${kind} error`, async () => {
    await reachUsageLimit();
    answers.chat_send = new Error(message);
    await sendQuestion();
    assert.equal(State.view, "note", "only this request's limit event may suppress its error");
    assert.equal(State.noteMessage, message);
    assert.equal($(".chat-notice-title"), null, "the old usage warning is cleared");
  });
}

test("a retry that reaches the usage limit again keeps Manage usage", async () => {
  await reachUsageLimit();
  await sendQuestion();
  assert.equal(State.view, "prompt");
  assert.equal(State.noteMessage, "");
  assert.equal($(".chat-notice-title").textContent, "Usage limit reached");
  assert.ok(view.el.find(".picker-link").some((button) => button.textContent === "Manage usage"));
});

test("a successful retry clears a usage limit and ignores a late limit event", async () => {
  await reachUsageLimit();
  answers.chat_send = { text: "Recovered" };
  await sendQuestion();
  emit("chatgpt-usage-limit", null);
  view.sync();
  assert.equal(State.view, "prompt");
  assert.equal(State.chatHistory.at(-1).content, "Recovered");
  assert.equal($(".chat-notice-title"), null, "a completed answer no longer owns usage events");
});

test("a retry clears the usage notice before its answer settles", async (t) => {
  await reachUsageLimit();
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve({ text: "Recovered" }); await flush(); });
  answers.chat_send = () => pending.promise;
  await sendQuestion();
  assert.ok($(".send-btn").classList.contains("stop"));
  assert.equal($(".chat-notice-title"), null, "the previous result cannot label an in-flight retry");
});

test("Stop after a usage limit keeps partial text and clears the old warning", async (t) => {
  await reachUsageLimit();
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve({ text: "Unused" }); await flush(); });
  answers.chat_send = () => pending.promise;
  answers.chat_stop = () => pending.reject(new Error("Stopped."));
  await sendQuestion();
  emit("chat-delta", "Partial retry");
  $(".send-btn").fire("click");
  await flush();
  view.sync();
  assert.equal(State.view, "prompt");
  assert.deepEqual([State.chatHistory.at(-1).content, State.chatHistory.at(-1).stopped], ["Partial retry", true]);
  assert.equal($(".chat-notice-title"), null);
});

test("regenerating a stopped retry after a usage limit shows its own error", async (t) => {
  await reachUsageLimit();
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve({ text: "Unused" }); await flush(); });
  answers.chat_send = () => pending.promise;
  answers.chat_stop = () => pending.reject(new Error("Stopped."));
  await sendQuestion();
  emit("chat-delta", "Partial retry");
  $(".send-btn").fire("click");
  await flush();
  view.sync();
  answers.chat_send = new Error("Regenerated request lost its connection.");
  const regenerate = view.el.find(".md-copy").find((button) => button.title === "Regenerate" || String(button.getAttribute("aria-label")) === "Regenerate");
  assert.ok(regenerate);
  regenerate.fire("click");
  await flush();
  view.sync();
  assert.equal(State.view, "note");
  assert.equal(State.noteMessage, "Regenerated request lost its connection.");
  assert.equal($(".chat-notice-title"), null);
});

for (const transition of ["new conversation", "provider round trip", "account switch"]) {
  test(`a usage limit cannot hide a later error after ${transition}`, async () => {
    await reachUsageLimit();
    if (transition === "new conversation") newConversation();
    else if (transition === "provider round trip") {
      State.settings = { ...State.settings, chatProvider: "anthropic" };
      view.sync();
      State.settings = { ...State.settings, chatProvider: "chatgpt" };
    } else {
      const account = { ...CONNECTED, accountId: "new-account" };
      answers.chatgpt_status = account;
      emit("chatgpt-status", account);
    }
    answers.chat_send = new Error("This request failed for a different reason.");
    await sendQuestion();
    assert.equal(State.view, "note");
    assert.equal(State.noteMessage, "This request failed for a different reason.");
    assert.equal($(".chat-notice-title"), null);
  });
}

test("a late ChatGPT usage event cannot attach to another provider's answer", async (t) => {
  answers.chat_send = { text: "A completed ChatGPT answer" };
  await sendQuestion();
  State.settings = { ...State.settings, chatProvider: "anthropic" };
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve({ text: "Unused" }); await flush(); });
  answers.chat_send = () => pending.promise;
  await sendQuestion();
  // The request still belongs to Anthropic if settings change while IPC waits.
  State.settings = { ...State.settings, chatProvider: "chatgpt" };
  emit("chatgpt-usage-limit", null);
  pending.reject(new Error("Anthropic connection failed."));
  await flush();
  assert.equal(State.view, "note");
  assert.equal(State.noteMessage, "Anthropic connection failed.");
  State.settings = { ...State.settings, chatProvider: "chatgpt" };
  State.view = "prompt";
  view.sync();
  assert.equal($(".chat-notice-title"), null, "a different provider cannot acquire a ChatGPT usage warning");
});

test("a usage event after Stop cannot label the cancelled request as limited", async (t) => {
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve({ text: "Unused" }); await flush(); });
  answers.chat_send = () => pending.promise;
  await sendQuestion();
  emit("chat-delta", "Partial answer");
  $(".send-btn").fire("click");
  await flush();
  emit("chatgpt-usage-limit", null);
  pending.reject(new Error("Stopped."));
  await flush();
  view.sync();
  assert.equal(State.view, "prompt");
  assert.equal(State.chatHistory.at(-1).stopped, true);
  assert.equal($(".chat-notice-title"), null);
});

test("usage events belong to the captured ChatGPT provider if settings change mid-answer", async (t) => {
  const pending = Promise.withResolvers();
  t.after(async () => { pending.resolve({ text: "Unused" }); await flush(); });
  answers.chat_send = () => pending.promise;
  await sendQuestion();
  State.settings = { ...State.settings, chatProvider: "anthropic" };
  emit("chatgpt-usage-limit", null);
  pending.reject(new Error("Usage limit reached."));
  await flush();
  assert.equal(State.view, "prompt", "the in-flight request still belongs to ChatGPT");
  State.settings = { ...State.settings, chatProvider: "chatgpt" };
  view.sync();
  assert.equal($(".chat-notice-title").textContent, "Usage limit reached");
});

test("the first sign-in shows the plan notice once; Got it records it", async () => {
  answers.chatgpt_status = { ...CONNECTED, showWelcome: true };
  view = buildPrompt(() => {});
  await flush();
  view.sync();
  assert.equal($(".chat-notice-title").textContent, "You're using your ChatGPT plan");
  answers.chatgpt_status = CONNECTED;
  view.el.find(".chatgpt-connect").find((b) => b.textContent === "Got it").fire("click");
  await flush();
  await flush();
  view.sync();
  assert.equal(sent("chatgpt_welcomed").length, 1);
  assert.equal($(".chat-notice-title"), null);
});

test("welcome buttons stay bound to the registration displayed when they were rendered", async () => {
  answers.chatgpt_status = { ...CONNECTED, showWelcome: true };
  view = buildPrompt(() => {});
  await flush();
  view.sync();
  const oldButton = view.el.find(".chatgpt-connect").find((b) => b.textContent === "Got it");
  const newer = { ...CONNECTED, accountId: "other-client", showWelcome: true };
  answers.chatgpt_status = newer;
  emit("chatgpt-status", newer);
  view.sync();
  const newButton = view.el.find(".chatgpt-connect").find((b) => b.textContent === "Got it");
  assert.notEqual(newButton, oldButton, "same-kind welcome notices must refresh for a different account");
  oldButton.fire("click");
  newButton.fire("click");
  await flush();
  assert.deepEqual(sent("chatgpt_welcomed"), [{ clientId: "test-client" }, { clientId: "other-client" }]);
});

test("a delayed initial account status cannot overwrite a newer registration event", async () => {
  const pending = Promise.withResolvers();
  answers.chatgpt_status = () => pending.promise;
  view = buildPrompt(() => {});
  await flush();
  const newer = { ...CONNECTED, accountId: "other-client", showWelcome: true };
  emit("chatgpt-status", newer);
  view.sync();
  pending.resolve({ ...CONNECTED, showWelcome: true });
  await flush();
  view.sync();
  answers.chatgpt_status = newer;
  view.el.find(".chatgpt-connect").find((b) => b.textContent === "Got it").fire("click");
  await flush();
  assert.deepEqual(sent("chatgpt_welcomed"), [{ clientId: "other-client" }]);
});

test("same-account status invalidation closes a pending picker instead of stranding its spinner", async () => {
  const pending = Promise.withResolvers();
  answers.chatgpt_models = () => pending.promise;
  $(".model-btn").fire("click");
  await flush();
  assert.ok($(".picker").classList.contains("on"));
  emit("chatgpt-status", CONNECTED);
  pending.resolve(CATALOG);
  await flush();
  assert.equal($(".picker").classList.contains("on"), false);
  answers.chatgpt_models = CATALOG;
  $(".model-btn").fire("click");
  await flush();
  assert.equal(view.el.find(".picker-model").length, 2, "reopening can fetch a fresh catalog");
});

test("reopening after switching accounts clears old model choices before awaiting status", async () => {
  $(".model-btn").fire("click");
  await flush();
  assert.equal(view.el.find(".picker-model").length, 2);
  const newer = { ...CONNECTED, accountId: "other-client" };
  emit("chatgpt-status", newer);
  const pending = Promise.withResolvers();
  answers.chatgpt_status = () => pending.promise;
  answers.chatgpt_models = [CATALOG[0], { ...CATALOG[1], available: false }];
  $(".model-btn").fire("click");
  assert.ok($(".picker").classList.contains("on"));
  assert.equal(view.el.find(".picker-model").length, 0, "no previous-account choice while status is pending");
  pending.resolve(newer);
  await flush();
  const rows = view.el.find(".picker-model");
  assert.equal(rows.length, 2);
  assert.equal(rows[1].disabled, true);
  rows[1].fire("click");
  assert.equal(State.settings.chatModels.chatgpt, undefined);
});

for (const outcome of ["success", "failure"]) {
  test(`a delayed old-account catalog ${outcome} cannot replace the new account's choices`, async () => {
    const pending = Promise.withResolvers();
    answers.chatgpt_models = () => pending.promise;
    $(".model-btn").fire("click");
    await flush();
    const newer = { ...CONNECTED, accountId: "other-client" };
    answers.chatgpt_status = newer;
    answers.chatgpt_models = [CATALOG[0], { ...CATALOG[1], available: false }];
    emit("chatgpt-status", newer);
    $(".model-btn").fire("click");
    await flush();
    if (outcome === "success") pending.resolve(CATALOG);
    else pending.reject(new Error("old-account catalog failed"));
    await flush();
    const rows = view.el.find(".picker-model");
    assert.equal(rows.length, 2);
    assert.equal(rows[1].disabled, true);
    rows[1].fire("click");
    assert.equal(State.settings.chatModels.chatgpt, undefined);
    assert.equal($(".picker-list").find(".chatgpt-connect").length, 0, "a stale error must not replace the current picker");
  });
}

test("a saved conversation reopens from the history with its signatures", async () => {
  answers.chat_history_list = [{ id: "c-00000000000000aa", title: "Lisbon trip", updatedAt: Date.now(), count: 2 }];
  answers.chat_history_open = {
    id: "c-00000000000000aa", title: "Lisbon trip", createdAt: 1, updatedAt: 2,
    messages: [{ role: "user", content: "Plan it" }, { role: "assistant", content: "Day 1…", model: SOL, effort: "high" }],
  };
  $(".history-btn").fire("click");
  await flush();
  const row = view.el.find(".history-row")[0];
  assert.match(row.textContent, /Lisbon trip/);
  row.fire("click");
  await flush();
  view.sync();
  assert.deepEqual(sent("chat_history_open"), [{ id: "c-00000000000000aa" }]);
  assert.equal(State.chatId, "c-00000000000000aa");
  assert.deepEqual(view.el.find(".reply-sig").map((e) => e.textContent), ["GPT-6.1 Sol · High"]);
});

test("the island never asks for, nor gets, a token", () => {
  const asked = calls.map(([cmd]) => cmd);
  assert.ok(!asked.some((c) => /token|secret_get/.test(c)), asked.join(","));
});

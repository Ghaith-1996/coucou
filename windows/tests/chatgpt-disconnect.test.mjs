// Settings and the real bridge, with only Tauri/native effects replaced.
// No OAuth requests or system credential reads/writes are made by these tests.
import { beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { installFakeDom } from "./fakedom.mjs";
import { calls, emit, internals, sent } from "./tauri.mjs";

installFakeDom();
const { Bridge } = await import("../src/core/bridge.ts");
const { chatgptSection } = await import("../src/settings/chatgpt.ts");
const { setLanguage } = await import("../src/i18n/i18n.ts");

const CONNECTED = {
  connected: true, hasSavedCredentials: true, email: "me@example.com", planEnabled: true,
  needsReconnect: false, showWelcome: false, manageUsageUrl: "https://chatgpt.com/#settings",
};
const SIGNED_OUT = { ...CONNECTED, connected: false, hasSavedCredentials: false, needsReconnect: true };
const ORPHANED_ID = { ...SIGNED_OUT, hasSavedCredentials: true };
const LOCAL_WARNING = "Couldn't remove all saved ChatGPT credentials from this device. Try Disconnect again.";
const REMOTE_WARNING = "Remote revocation was not confirmed. You can also disconnect Coucou in ChatGPT's settings.";

let answers;
const plainInvoke = internals.invoke;
internals.invoke = async (cmd, args) => {
  const result = await plainInvoke(cmd, args);
  if (!(cmd in answers)) return result;
  const value = typeof answers[cmd] === "function" ? answers[cmd](args) : answers[cmd];
  if (value instanceof Error) throw value;
  return value;
};
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));
const button = (section, text) => section.find("BUTTON").find((b) => b.textContent === text);
const message = (section) => section.children[2].textContent;
async function section() {
  const el = chatgptSection();
  await flush();
  return el;
}
async function disconnect(el) {
  const b = button(el, "Disconnect");
  assert.ok(b, "Disconnect remains available while credentials need cleanup");
  assert.equal(b.getAttribute("disabled"), null, "Disconnect can be retried");
  b.fire("click");
  await flush();
  await flush();
}

beforeEach(() => {
  calls.length = 0;
  answers = { chatgpt_status: CONNECTED };
  setLanguage("en");
});

test("the disconnect bridge preserves IPC errors instead of returning a false success", async () => {
  answers.chatgpt_disconnect = new Error("Native command unavailable");
  await assert.rejects(Bridge.chatgptDisconnect(), /Native command unavailable/);
});

for (const revocationConfirmed of [true, false, null]) {
  for (const localCleanupSucceeded of [true, false]) {
    test(`disconnect reports remote=${revocationConfirmed}, local cleanup=${localCleanupSucceeded} independently`, async () => {
      const el = await section();
      answers.chatgpt_disconnect = () => {
        answers.chatgpt_status = localCleanupSucceeded ? SIGNED_OUT : ORPHANED_ID;
        emit("chatgpt-status", answers.chatgpt_status);
        return { revocationConfirmed, localCleanupSucceeded };
      };
      await disconnect(el);
      assert.equal(message(el).includes(LOCAL_WARNING), !localCleanupSucceeded);
      assert.equal(message(el).includes(REMOTE_WARNING), revocationConfirmed === false);
      assert.equal(!!button(el, "Disconnect"), !localCleanupSucceeded);
      assert.ok(!message(el).includes("Signed out here"), "never claims local sign-out when cleanup failed");
      assert.equal(button(el, "Reconnect").getAttribute("disabled"), null);
    });
  }
}

test("an orphaned ID token can be cleaned up after Settings is recreated", async () => {
  let el = await section();
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = ORPHANED_ID;
    return { revocationConfirmed: true, localCleanupSucceeded: false };
  };
  await disconnect(el);
  el = await section();
  assert.ok(button(el, "Disconnect"), "the native saved-credentials status survives a new section");
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = SIGNED_OUT;
    return { revocationConfirmed: null, localCleanupSucceeded: true };
  };
  await disconnect(el);
  assert.equal(button(el, "Disconnect"), undefined);
  assert.equal(message(el), "Your ChatGPT session ended. Reconnect in Settings.");
  assert.equal(sent("chatgpt_disconnect").length, 2);
});

test("local-only retries preserve an earlier unconfirmed remote revocation", async () => {
  const el = await section();
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = ORPHANED_ID;
    return { revocationConfirmed: false, localCleanupSucceeded: false };
  };
  await disconnect(el);
  answers.chatgpt_disconnect = { revocationConfirmed: null, localCleanupSucceeded: false };
  await disconnect(el);
  assert.ok(message(el).includes(LOCAL_WARNING));
  assert.ok(message(el).includes(REMOTE_WARNING));
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = SIGNED_OUT;
    return { revocationConfirmed: null, localCleanupSucceeded: true };
  };
  await disconnect(el);
  assert.equal(message(el), REMOTE_WARNING, "local success does not erase the remaining remote warning");
  assert.equal(button(el, "Disconnect"), undefined);
  assert.equal(sent("chatgpt_disconnect").length, 3);
});

test("an IPC failure leaves the section retryable and displays the actual error", async () => {
  const el = await section();
  answers.chatgpt_disconnect = new Error("Native command unavailable");
  await disconnect(el);
  assert.equal(message(el), "Native command unavailable");
  assert.equal(button(el, "Disconnect").getAttribute("disabled"), null);
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = SIGNED_OUT;
    return { revocationConfirmed: true, localCleanupSucceeded: true };
  };
  await disconnect(el);
  assert.equal(button(el, "Disconnect"), undefined);
  assert.equal(message(el), "Your ChatGPT session ended. Reconnect in Settings.");
});

test("a failed retry keeps both warnings and re-enables the buttons", async () => {
  const el = await section();
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = ORPHANED_ID;
    return { revocationConfirmed: false, localCleanupSucceeded: false };
  };
  await disconnect(el);
  answers.chatgpt_disconnect = new Error("Native command unavailable");
  await disconnect(el);
  assert.ok(message(el).includes("Native command unavailable"));
  assert.ok(message(el).includes(LOCAL_WARNING));
  assert.ok(message(el).includes(REMOTE_WARNING));
  assert.equal(button(el, "Disconnect").getAttribute("disabled"), null);
});

test("a missing follow-up status does not hide an unsuccessful cleanup retry", async () => {
  const el = await section();
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = null;
    return { revocationConfirmed: true, localCleanupSucceeded: false };
  };
  await disconnect(el);
  assert.ok(message(el).includes(LOCAL_WARNING));
  assert.equal(button(el, "Disconnect").getAttribute("disabled"), null);
});

test("a successful cleanup still signs out locally when the follow-up status is missing", async () => {
  const el = await section();
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = null;
    return { revocationConfirmed: true, localCleanupSucceeded: true };
  };
  await disconnect(el);
  assert.equal(button(el, "Disconnect"), undefined);
  assert.ok(button(el, "Continue with ChatGPT") || button(el, "Reconnect"));
});

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
  accountId: "test-client", canSwitchAccounts: false, savedAccounts: [], hasPendingAccount: false,
  connected: true, hasSavedCredentials: true, email: "me@example.com", planEnabled: true,
  needsReconnect: false, showWelcome: false, manageUsageUrl: "https://chatgpt.com/#settings",
};
const SIGNED_OUT = { ...CONNECTED, connected: false, hasSavedCredentials: false, needsReconnect: true, canSwitchAccounts: true };
const NEEDS_CLEANUP = { ...SIGNED_OUT, canSwitchAccounts: false };
const ORPHANED_ID = { ...NEEDS_CLEANUP, hasSavedCredentials: true };
const LOCAL_WARNING = "Couldn't finish disconnecting ChatGPT on this device. Try Disconnect again.";
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


test("an account-file cleanup failure stays retryable after Settings is recreated", async () => {
  let el = await section();
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = NEEDS_CLEANUP;
    return { revocationConfirmed: true, localCleanupSucceeded: false };
  };
  await disconnect(el);
  assert.ok(message(el).includes(LOCAL_WARNING));
  el = await section();
  assert.ok(button(el, "Disconnect"), "retained identity without credentials still needs explicit cleanup");
  assert.equal(button(el, "Use a different ChatGPT account"), undefined);
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = SIGNED_OUT;
    return { revocationConfirmed: null, localCleanupSucceeded: true };
  };
  await disconnect(el);
  assert.equal(button(el, "Disconnect"), undefined);
  assert.ok(button(el, "Use a different ChatGPT account"));
});

test("adding another registration is available only after confirmed explicit disconnect", async () => {
  for (const status of [CONNECTED, NEEDS_CLEANUP, ORPHANED_ID]) {
    answers.chatgpt_status = status;
    assert.equal(button(await section(), "Use a different ChatGPT account"), undefined);
  }
  answers.chatgpt_status = SIGNED_OUT;
  const el = await section();
  answers.chatgpt_connect = { ...CONNECTED, accountId: "new-client", email: "other@example.com" };
  button(el, "Use a different ChatGPT account").fire("click");
  await flush();
  assert.deepEqual(sent("chatgpt_connect"), [{ newAccount: true }]);
  assert.equal(message(el), "Connected as other@example.com");
});

test("a failed new registration exposes the persisted retry after recreating Settings", async () => {
  answers.chatgpt_status = SIGNED_OUT;
  let el = await section();
  answers.chatgpt_connect = () => {
    answers.chatgpt_status = { ...SIGNED_OUT, hasPendingAccount: true };
    throw new Error("ChatGPT sign-in failed.");
  };
  button(el, "Use a different ChatGPT account").fire("click");
  await flush();
  await flush();
  assert.equal(message(el), "ChatGPT sign-in failed.");
  assert.ok(button(el, "Finish connecting new ChatGPT account"));
  el = await section();
  assert.ok(button(el, "Finish connecting new ChatGPT account"));
  assert.ok(button(el, "Reconnect"), "the original account remains available");
});

test("a saved registration can be selected again without automatically granting access", async () => {
  answers.chatgpt_status = { ...SIGNED_OUT, savedAccounts: [{ id: "previous-client", email: "previous@example.com" }] };
  const el = await section();
  const select = el.find("SELECT")[0];
  assert.ok(select);
  assert.ok(select.find("OPTION").some((option) => option.textContent.includes("previous@example.com") && option.textContent.includes("previous-client")));
  answers.chatgpt_select_account = { ...SIGNED_OUT, accountId: "previous-client", email: "previous@example.com" };
  select.value = "previous-client";
  select.fire("change");
  await flush();
  assert.deepEqual(sent("chatgpt_select_account"), [{ clientId: "previous-client" }]);
  assert.equal(sent("chatgpt_connect").length, 0);
  assert.ok(button(el, "Reconnect"));
});

test("the initial status reply cannot restore stale connected state after disconnect", async () => {
  let resolveInitial;
  answers.chatgpt_status = new Promise((resolve) => { resolveInitial = resolve; });
  const el = chatgptSection();
  await flush();
  emit("chatgpt-status", CONNECTED);
  answers.chatgpt_disconnect = () => {
    answers.chatgpt_status = SIGNED_OUT;
    return { revocationConfirmed: true, localCleanupSucceeded: true };
  };
  await disconnect(el);
  resolveInitial(CONNECTED);
  await flush();
  assert.equal(button(el, "Disconnect"), undefined);
  assert.ok(button(el, "Use a different ChatGPT account"));
});

test("welcome acknowledgement identifies the displayed registration and is disabled while disconnecting", async () => {
  answers.chatgpt_status = { ...CONNECTED, showWelcome: true };
  const el = await section();
  button(el, "Got it").fire("click");
  await flush();
  assert.deepEqual(sent("chatgpt_welcomed"), [{ clientId: "test-client" }]);
  let finish;
  answers.chatgpt_disconnect = new Promise((resolve) => { finish = resolve; });
  button(el, "Disconnect").fire("click");
  await flush();
  assert.notEqual(button(el, "Got it").getAttribute("disabled"), null);
  answers.chatgpt_status = SIGNED_OUT;
  finish({ revocationConfirmed: true, localCleanupSucceeded: true });
  await flush();
});

test("a credential-write failure displays the persisted new account as disconnected and retryable", async () => {
  answers.chatgpt_status = SIGNED_OUT;
  const el = await section();
  answers.chatgpt_connect = () => {
    answers.chatgpt_status = {
      ...NEEDS_CLEANUP, accountId: "new-client", email: "new@example.com", planEnabled: false,
      savedAccounts: [{ id: "test-client", email: "me@example.com" }],
    };
    throw new Error("Keystore unavailable");
  };
  button(el, "Use a different ChatGPT account").fire("click");
  await flush();
  await flush();
  assert.equal(message(el), "Keystore unavailable");
  const selected = el.find(".chatgpt-account")[0].textContent;
  assert.ok(selected.includes("new-client") && selected.includes("new@example.com"));
  assert.ok(!selected.includes("test-client") && !selected.includes("me@example.com"));
  assert.ok(button(el, "Reconnect"));
  assert.ok(button(el, "Disconnect"), "explicit cleanup must be confirmed again before returning to a saved account");
  assert.equal(button(el, "Use a different ChatGPT account"), undefined);
  assert.equal(el.find("SELECT").length, 0);
});

test("failed saved-account selection leaves the previous state and exposes the error", async () => {
  answers.chatgpt_status = { ...SIGNED_OUT, savedAccounts: [{ id: "previous-client", email: "previous@example.com" }] };
  const el = await section();
  answers.chatgpt_select_account = new Error("Couldn't check saved ChatGPT credentials. Try again.");
  const select = el.find("SELECT")[0];
  select.value = "previous-client";
  select.fire("change");
  await flush();
  await flush();
  assert.equal(message(el), "Couldn't check saved ChatGPT credentials. Try again.");
  assert.equal(sent("chatgpt_connect").length, 0);
  assert.equal(button(el, "Reconnect").getAttribute("disabled"), null);
});

test("the selected registration stays visible while signed out and when an error overrides status", async () => {
  answers.chatgpt_status = { ...SIGNED_OUT, savedAccounts: [{ id: "previous-client", email: SIGNED_OUT.email }] };
  const el = await section();
  const label = () => el.find(".chatgpt-account")[0]?.textContent;
  assert.ok(label()?.includes("test-client") && label()?.includes("me@example.com"));
  answers.chatgpt_select_account = { ...SIGNED_OUT, accountId: "previous-client", savedAccounts: [{ id: "test-client", email: SIGNED_OUT.email }] };
  const select = el.find("SELECT")[0];
  select.value = "previous-client";
  select.fire("change");
  await flush();
  assert.ok(label().includes("previous-client"));
  assert.ok(!label().includes("test-client"), "equal emails do not collapse different registrations");
  answers.chatgpt_connect = new Error("Sign-in failed");
  answers.chatgpt_status = { ...answers.chatgpt_select_account };
  button(el, "Reconnect").fire("click");
  await flush();
  await flush();
  assert.equal(message(el), "Sign-in failed");
  assert.ok(label().includes("previous-client"));
});

function deferred() {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}
const NEWER_ACCOUNT = { ...CONNECTED, accountId: "newer-client", email: "newer@example.com" };
const selectedLabel = (el) => el.find(".chatgpt-account")[0].textContent;
function startOperation(el, operation) {
  if (operation === "connect") button(el, "Reconnect").fire("click");
  else if (operation === "disconnect") button(el, "Disconnect").fire("click");
  else {
    const select = el.find("SELECT")[0];
    select.value = "previous-client";
    select.fire("change");
  }
}
function operationInitialStatus(operation) {
  return operation === "disconnect" ? CONNECTED : {
    ...SIGNED_OUT, savedAccounts: [{ id: "previous-client", email: "previous@example.com" }],
  };
}
const commandFor = { connect: "chatgpt_connect", select: "chatgpt_select_account", disconnect: "chatgpt_disconnect" };

for (const operation of ["connect", "select", "disconnect"]) {
  for (const fails of [false, true]) {
    test(`a newer account event wins over a delayed ${operation} ${fails ? "failure" : "success"}`, async () => {
      answers.chatgpt_status = operationInitialStatus(operation);
      const el = await section();
      const pending = deferred();
      answers[commandFor[operation]] = pending.promise;
      startOperation(el, operation);
      await flush();
      emit("chatgpt-status", NEWER_ACCOUNT);
      answers.chatgpt_status = SIGNED_OUT;
      if (fails) pending.reject(new Error("Earlier operation failed"));
      else pending.resolve(operation === "disconnect"
        ? { revocationConfirmed: true, localCleanupSucceeded: true }
        : { ...CONNECTED, accountId: "previous-client" });
      await flush();
      await flush();
      assert.ok(selectedLabel(el).includes("newer-client"), "older operation cannot restore or sign out a newer account");
      assert.ok(selectedLabel(el).includes("newer@example.com"));
      assert.equal(button(el, "Disconnect").getAttribute("disabled"), null);
      assert.equal(button(el, "Reconnect"), undefined, "the newer account stays connected");
      if (fails) assert.ok(message(el).includes("Earlier operation failed"));
    });
  }
}

for (const operation of ["connect", "select", "disconnect"]) {
  test(`a newer event also wins while ${operation}'s follow-up status is pending`, async () => {
    answers.chatgpt_status = operationInitialStatus(operation);
    const el = await section();
    const pendingStatus = deferred();
    answers.chatgpt_status = pendingStatus.promise;
    answers[commandFor[operation]] = operation === "disconnect"
      ? { revocationConfirmed: false, localCleanupSucceeded: false }
      : new Error("Earlier operation failed");
    startOperation(el, operation);
    await flush();
    assert.equal(sent("chatgpt_status").length, 2, "the fallback read is in flight");
    emit("chatgpt-status", NEWER_ACCOUNT);
    pendingStatus.resolve(SIGNED_OUT);
    await flush();
    await flush();
    assert.ok(selectedLabel(el).includes("newer-client"));
    assert.equal(button(el, "Reconnect"), undefined);
    assert.equal(button(el, "Disconnect").getAttribute("disabled"), null);
    if (operation === "disconnect") {
      assert.ok(message(el).includes(LOCAL_WARNING));
      assert.ok(message(el).includes(REMOTE_WARNING));
    } else assert.ok(message(el).includes("Earlier operation failed"));
  });
}

for (const operation of ["models", "send"]) {
  for (const retainedCredential of [false, true]) {
    test(`expired session from ${operation} exposes recovery with retained credential=${retainedCredential}`, async () => {
      const el = await section();
      assert.equal(button(el, "Reconnect"), undefined);
      const expired = { ...CONNECTED, connected: false, needsReconnect: true, hasSavedCredentials: retainedCredential };
      const command = operation === "models" ? "chatgpt_models" : "chat_send";
      answers[command] = () => {
        answers.chatgpt_status = expired;
        emit("chatgpt-status", expired);
        throw new Error("Your ChatGPT session ended. Reconnect in Settings.");
      };
      await assert.rejects(operation === "models" ? Bridge.chatgptModels() : Bridge.chatSend("hello", null), /Reconnect in Settings/);
      assert.ok(button(el, "Reconnect"), "an already-open Settings section offers recovery");
      assert.ok(button(el, "Disconnect"), "explicit cleanup remains available");
      assert.equal(button(el, "Use a different ChatGPT account"), undefined, "expiration is not explicit Disconnect");
      assert.ok(selectedLabel(el).includes("test-client"));
      assert.equal(message(el), "Your ChatGPT session ended. Reconnect in Settings.");
      const reopened = await section();
      assert.ok(button(reopened, "Reconnect"), "recreated Settings uses the same native recovery state");
      answers.chatgpt_connect = () => {
        answers.chatgpt_status = CONNECTED;
        emit("chatgpt-status", CONNECTED);
        return CONNECTED;
      };
      button(reopened, "Reconnect").fire("click");
      await flush();
      assert.deepEqual(sent("chatgpt_connect").at(-1), { newAccount: false });
      assert.equal(button(reopened, "Reconnect"), undefined);
      assert.ok(selectedLabel(reopened).includes("test-client"));
    });
  }
}

// Settings → ChatGPT: sign in with a ChatGPT account so the chat can use the
// plan's usage (chatgpt.rs). Continue with ChatGPT, Disconnect and Reconnect;
// the window only ever sees a status, never a token.

import { Bridge, onEvent } from "../core/bridge";
import type { ChatGptStatus } from "../core/chatgpt";
import { h, clear } from "../views/dom";
import { statusDot } from "./parts";
import { t } from "../i18n/i18n";

const STRINGS = {
  get title() { return "ChatGPT"; },
  get hint() { return t("Chat on your ChatGPT plan instead of an API key: sign in, then pick ChatGPT above the chat box. Coucou never sees your password; it keeps only a sign-in token, in the system keychain, and nothing is billed to an API account."); },
  get connect() { return t("Continue with ChatGPT"); },
  get connecting() { return t("Finish signing in in your browser…"); },
  get reconnect() { return t("Reconnect"); },
  get disconnect() { return t("Disconnect"); },
  get differentAccount() { return t("Use a different ChatGPT account"); },
  get retryAccount() { return t("Finish connecting new ChatGPT account"); },
  get chooseAccount() { return t("Choose a saved ChatGPT account"); },
  get newAccountHint() { return t("Choose and authorize the new account in your browser. Saved accounts remain available here."); },
  get manageUsage() { return t("Manage usage"); },
  get notConnected() { return t("Not connected."); },
  get sessionEnded() { return t("Your ChatGPT session ended. Reconnect in Settings."); },
  get planOff() { return t("Plan usage wasn't allowed at sign-in."); },
  get notRevoked() { return t("Remote revocation was not confirmed. You can also disconnect Coucou in ChatGPT's settings."); },
  get cleanupFailed() { return t("Couldn't finish disconnecting ChatGPT on this device. Try Disconnect again."); },
  get welcomeTitle() { return t("You're using your ChatGPT plan"); },
  get welcomeBody() { return t("Eligible AI requests in Coucou use the usage included in your ChatGPT plan or credits balance."); },
  get gotIt() { return t("Got it"); },
  connectedAs: (email: string) => t("Connected as {email}", { email }),
  selectedAccount: (account: string) => t("Selected ChatGPT account: {account}", { account }),
};

export function chatgptSection(): HTMLElement {
  const dot = statusDot(false);
  const line = h("div", { class: "hint" });
  const identity = h("div", { class: "hint chatgpt-account", style: "overflow-wrap:anywhere" });
  const actions = h("div", { class: "row", style: "gap:8px;flex-wrap:wrap" });
  const welcome = h("div");
  let status: ChatGptStatus | null = null;
  let busy = false;
  let note = "";
  let cleanupFailed = false;
  let revocationUnconfirmed = false;
  let statusVersion = 0;

  function draw() {
    const s = status;
    identity.textContent = s?.accountId ? STRINGS.selectedAccount(`${s.email ?? "ChatGPT"} (${s.accountId})`) : "";
    dot.style.background = s?.connected && s.planEnabled ? "#22c55e" : s?.connected || s?.needsReconnect ? "#f5a524" : "#f4505e";
    line.textContent = [note, cleanupFailed ? STRINGS.cleanupFailed : "", revocationUnconfirmed ? STRINGS.notRevoked : ""].filter(Boolean).join(" ")
      || (busy ? STRINGS.connecting
        : !s?.connected ? (s?.needsReconnect ? STRINGS.sessionEnded : STRINGS.notConnected)
        : !s.planEnabled ? STRINGS.planOff
        : s.email ? STRINGS.connectedAs(s.email) : "");
    clear(actions);
    const button = (text: string, onclick: () => void, danger = false) =>
      h("button", { text, onclick, class: danger ? "danger" : "", disabled: busy });
    if (!s?.connected) {
      actions.append(button(s?.needsReconnect ? STRINGS.reconnect : STRINGS.connect, () => void connect()));
    } else {
      if (!s.planEnabled) actions.append(button(STRINGS.reconnect, () => void connect()));
    }
    if (s?.connected || s?.hasSavedCredentials || (s?.accountId && !s.canSwitchAccounts) || cleanupFailed) {
      actions.append(button(STRINGS.disconnect, () => void disconnect(), true));
    }
    if (s?.canSwitchAccounts) {
      if (s.savedAccounts?.length) {
        const select = h("select", { disabled: busy, "aria-label": STRINGS.chooseAccount });
        select.append(h("option", { value: "", text: STRINGS.chooseAccount }));
        for (const account of s.savedAccounts) {
          select.append(h("option", { value: account.id, text: `${account.email ?? "ChatGPT"} (${account.id})` }));
        }
        select.addEventListener("change", () => { if (select.value) void selectAccount(select.value); });
        actions.append(select);
      }
      actions.append(button(s.hasPendingAccount ? STRINGS.retryAccount : STRINGS.differentAccount, () => void connect(true)));
      actions.append(h("div", { class: "hint", text: STRINGS.newAccountHint }));
    }
    if (s?.connected) {
      actions.append(h("button", { text: STRINGS.manageUsage, onclick: () => void Bridge.openUrl(s.manageUsageUrl) }));
    }
    clear(welcome);
    // The first sign-in with plan usage: once, never again.
    if (s?.showWelcome) {
      welcome.append(h("div", { class: "hint", style: "margin-top:8px" },
        h("strong", { text: STRINGS.welcomeTitle }),
        h("div", { text: STRINGS.welcomeBody }),
        h("button", { text: STRINGS.gotIt, disabled: busy, style: "margin-top:6px", onclick: () => { if (!busy) void Bridge.chatgptWelcomed(s.accountId ?? null); } }),
      ));
    }
  }

  async function refreshStatus(version: number) {
    // A native event may already describe a later operation in another
    // window. Neither a queued reply nor a read started earlier can replace it.
    if (version !== statusVersion) return;
    const next = await Bridge.chatgptStatus();
    if (version === statusVersion) status = next ?? status;
  }

  async function connect(newAccount = false) {
    if (busy) return;
    const version = ++statusVersion;
    busy = true;
    note = "";
    draw();
    try {
      const next = await Bridge.chatgptConnect(newAccount);
      if (version === statusVersion) status = next;
      cleanupFailed = false;
      revocationUnconfirmed = false;
    } catch (err) {
      note = String(err).replace(/^Error:\s*/, "");
      // A newly issued registration survives a failed exchange, and a
      // credential-store failure may have persisted a validated identity.
      await refreshStatus(version);
    }
    busy = false;
    draw();
  }

  async function selectAccount(clientId: string) {
    if (busy) return;
    const version = ++statusVersion;
    busy = true;
    note = "";
    draw();
    try {
      const next = await Bridge.chatgptSelectAccount(clientId);
      if (version === statusVersion) status = next;
    } catch (err) {
      note = String(err).replace(/^Error:\s*/, "");
      await refreshStatus(version);
    } finally {
      busy = false;
      draw();
    }
  }

  async function disconnect() {
    if (busy) return;
    const version = ++statusVersion;
    busy = true;
    note = "";
    draw();
    try {
      const result = await Bridge.chatgptDisconnect();
      cleanupFailed = !result.localCleanupSucceeded;
      // An ID-token-only retry cannot confirm an earlier remote revocation.
      if (result.revocationConfirmed !== null) revocationUnconfirmed = !result.revocationConfirmed;
      if (version === statusVersion && result.localCleanupSucceeded && status) {
        status = { ...status, connected: false, hasSavedCredentials: false, showWelcome: false, canSwitchAccounts: true };
      }
      await refreshStatus(version);
    } catch (err) {
      note = String(err).replace(/^Error:\s*/, "");
    } finally {
      busy = false;
      draw();
    }
  }

  const initialVersion = statusVersion;
  void Bridge.chatgptStatus().then((s) => {
    if (initialVersion !== statusVersion) return;
    status = s ?? null;
    draw();
  });
  void onEvent<ChatGptStatus>("chatgpt-status", (s) => {
    ++statusVersion;
    status = s;
    draw();
  });
  draw();

  return h(
    "section",
    {},
    h("h2", {}, dot, h("span", { text: STRINGS.title })),
    h("div", { class: "hint", text: STRINGS.hint }),
    line,
    identity,
    actions,
    welcome,
  );
}

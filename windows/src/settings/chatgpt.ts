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
  get manageUsage() { return t("Manage usage"); },
  get notConnected() { return t("Not connected."); },
  get sessionEnded() { return t("Your ChatGPT session ended. Reconnect in Settings."); },
  get planOff() { return t("Plan usage wasn't allowed at sign-in."); },
  get notRevoked() { return t("Signed out here, but remote revocation was not confirmed. You can also disconnect Coucou in ChatGPT's settings."); },
  get welcomeTitle() { return t("You're using your ChatGPT plan"); },
  get welcomeBody() { return t("Eligible AI requests in Coucou use the usage included in your ChatGPT plan or credits balance."); },
  get gotIt() { return t("Got it"); },
  connectedAs: (email: string) => t("Connected as {email}", { email }),
};

export function chatgptSection(): HTMLElement {
  const dot = statusDot(false);
  const line = h("div", { class: "hint" });
  const actions = h("div", { class: "row", style: "gap:8px;flex-wrap:wrap" });
  const welcome = h("div");
  let status: ChatGptStatus | null = null;
  let busy = false;
  let note = "";

  function draw() {
    const s = status;
    dot.style.background = s?.connected && s.planEnabled ? "#22c55e" : s?.connected || s?.needsReconnect ? "#f5a524" : "#f4505e";
    line.textContent = note
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
      actions.append(button(STRINGS.disconnect, () => void disconnect(), true));
      actions.append(h("button", { text: STRINGS.manageUsage, onclick: () => void Bridge.openUrl(s.manageUsageUrl) }));
    }
    clear(welcome);
    // The first sign-in with plan usage: once, never again.
    if (s?.showWelcome) {
      welcome.append(h("div", { class: "hint", style: "margin-top:8px" },
        h("strong", { text: STRINGS.welcomeTitle }),
        h("div", { text: STRINGS.welcomeBody }),
        h("button", { text: STRINGS.gotIt, style: "margin-top:6px", onclick: () => void Bridge.chatgptWelcomed() }),
      ));
    }
  }

  async function connect() {
    busy = true;
    note = "";
    draw();
    try {
      status = await Bridge.chatgptConnect();
    } catch (err) {
      note = String(err).replace(/^Error:\s*/, "");
    }
    busy = false;
    draw();
  }

  async function disconnect() {
    busy = true;
    draw();
    const confirmed = await Bridge.chatgptDisconnect();
    note = confirmed ? "" : STRINGS.notRevoked;
    status = (await Bridge.chatgptStatus()) ?? null;
    busy = false;
    draw();
  }

  void Bridge.chatgptStatus().then((s) => {
    status = s ?? null;
    draw();
  });
  void onEvent<ChatGptStatus>("chatgpt-status", (s) => {
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
    actions,
    welcome,
  );
}

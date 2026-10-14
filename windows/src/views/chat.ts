// Chat view — DOM port of PromptView / ChatBubble / TypingDotsView /
// ModelPickerView from IslandViewContent.swift.
//
// Answers are rendered as Markdown (markdown.ts). A streaming provider (a
// local model, ChatGPT) sends `chat-delta` events with the text visible so
// far. The model name above the text field opens the picker: provider chips,
// then the models of the chosen provider, asked for only once it is picked.
//
// ChatGPT (core/chatgpt.ts, chatgpt.rs) signs in with the user's account
// instead of a key: the picker says so and offers "Continue with ChatGPT".
// Its reasoning effort sits next to the model name, remembered per model;
// each answer says which model and effort wrote it. Stop ends any answer
// under way. Conversations are saved (chat_store.rs) and reopened from the
// clock button; the last answer can be regenerated and every answer copied.

import { h, svg, clear } from "./dom";
import { ICONS } from "./icons";
import { copyButton, renderMarkdown } from "./markdown";
import { Bridge, onEvent, type ChatContext, type ConversationSummary, type ModelInfo } from "../core/bridge";
import {
  activeModel, pickModel, providerDef, visibleProviders, withModel, type ProviderDef,
} from "../core/providers";
import {
  CHATGPT, EFFORT_LABELS, effortFor, effortsFor, modelLabel, withEffort,
  type ChatGptModel, type ChatGptStatus,
} from "../core/chatgpt";
import { cleanTitle, fromSaved, newChatId, titleFor, toSaved, whenLabel } from "../core/conversations";
import { Sound } from "../core/sound";
import { State, type ChatMessage } from "../core/state";
import { HOST_OS } from "../core/pills";
import { voiceShortcut } from "../assistant/voice";
import type { ViewHost } from "./views";
import { N_, t, tl } from "../i18n/i18n";

const STRINGS = {
  placeholderFirst: N_("Ask me anything…"),
  placeholderNext: N_("Continue…"),
  send: N_("Send"),
  stop: N_("Stop"),
  voice: N_("Start voice capture"),
  switchModel: N_("Switch provider or model"),
  noModel: N_("Choose a model"),
  loading: N_("Loading models…"),
  noKey: N_("No API key — add it in Settings."),
  openSettings: N_("Open Settings"),
  connect: N_("Continue with ChatGPT"),
  connecting: N_("Finish signing in in your browser…"),
  signInFirst: N_("Sign in with your ChatGPT account to chat on your plan."),
  reconnect: N_("Reconnect"),
  planOff: N_("Plan usage wasn't allowed at sign-in."),
  unavailable: N_("Unavailable"),
  usingPlan: N_("Using ChatGPT plan"),
  manageUsage: N_("Manage usage"),
  effort: N_("Reasoning effort"),
  history: N_("Conversations"),
  newChat: N_("New chat"),
  noHistory: N_("No saved conversations yet."),
  rename: N_("Rename"),
  delete: N_("Delete"),
  confirmDelete: N_("Delete?"),
  regenerate: N_("Regenerate"),
  regenerateFailed: N_("Could not regenerate this answer. Please try again."),
  notSaved: N_("Conversation not saved"),
  saveFailed: N_("Your latest messages are only available here. Copy them before leaving this conversation or closing Coucou. {error}"),
  historyFull: N_("The 400-message storage limit was reached."),
  restoredTitle: N_("Reopened as text only"),
  restoredBody: N_("Original attachments are unavailable in this chat. Drop the file again to start a new chat with it."),
  stopped: N_("Stopped"),
  welcomeTitle: N_("You're using your ChatGPT plan"),
  welcomeBody: N_("Eligible AI requests in Coucou use the usage included in your ChatGPT plan or credits balance."),
  gotIt: N_("Got it"),
  limitTitle: N_("Usage limit reached"),
  limitBody: N_("Review your plan or this app's limit in ChatGPT settings."),
};

let nextId = 1;

/** The ChatGPT account as Rust last said, and its catalog once fetched. */
let chatgpt: ChatGptStatus | null = null;
let catalog: ChatGptModel[] | null = null;
// Native account events supersede reads already in flight in either window.
let chatgptRevision = 0;

function refreshChatGpt(then?: () => void) {
  const revision = chatgptRevision;
  void Bridge.chatgptStatus().then((s) => {
    if (revision !== chatgptRevision) return;
    chatgpt = s ?? null;
    then?.();
    State.notify();
  });
}

/** How an answer is signed: "GPT-6.1 Sol · Max", "claude-opus-5". */
function signature(m: ChatMessage): string {
  if (!m.model) return "";
  const name = catalog?.find((c) => c.id === m.model)?.label ?? modelLabel(m.model);
  const effort = m.effort && m.effort in EFFORT_LABELS ? ` · ${t(EFFORT_LABELS[m.effort as keyof typeof EFFORT_LABELS])}` : "";
  return `${name}${effort}`;
}

function typingDots(): HTMLElement {
  return h(
    "div",
    { class: "chat-row" },
    h("div", { class: "typing" }, h("i"), h("i"), h("i")),
  );
}

/** The coloured chip showing what the question is about (a dropped file). */
function contextChip(label: string): HTMLElement {
  const chip = h("div", { class: "chip" }, h("i", { class: "chip-dot" }), h("span", { text: label }));
  requestAnimationFrame(() => chip.classList.add("settled"));
  return chip;
}

function saveSettings() {
  void Bridge.saveSettings(State.settings);
}

function openUrl(url: string) {
  void Bridge.openUrl(url);
}

// ── Model picker ──────────────────────────────────────────────────────────────

interface Picker {
  el: HTMLElement;
  open(): void;
  close(): void;
  readonly isOpen: boolean;
}

/** Provider chips, then the chosen provider's models — ModelPickerView. */
function buildPicker(onChange: () => void): Picker {
  const chips = h("div", { class: "picker-chips" });
  const list = h("div", { class: "picker-list" });
  const el = h("div", { class: "picker" }, chips, h("div", { class: "picker-rule" }), list);

  /** Models already asked for, by provider; a model server is asked again each time. */
  const cache = new Map<string, ModelInfo[]>();
  let isOpen = false;
  let request = 0;

  function drawChips() {
    clear(chips);
    for (const p of visibleProviders(State.settings)) {
      const on = p.id === State.settings.chatProvider;
      const chip = h(
        "button",
        { class: on ? "picker-chip on" : "picker-chip", style: `--accent:${p.accent}` },
        h("i", { class: "picker-dot" }),
        h("span", { text: t(p.name) }),
      );
      chip.addEventListener("click", () => {
        if (p.id === State.settings.chatProvider) return;
        State.settings = { ...State.settings, chatProvider: p.id };
        saveSettings();
        Sound.play("pop");
        drawChips();
        void loadModels();
        onChange();
      });
      chips.append(chip);
    }
  }

  function status(text: string, withSettings = false) {
    clear(list);
    const line = h("div", { class: "picker-status", text });
    if (withSettings) {
      line.append(
        h("button", {
          class: "picker-link",
          text: tl(STRINGS.openSettings),
          onclick: () => void Bridge.openSettingsWindow(),
        }),
      );
    }
    list.append(line);
  }

  function drawModels(p: ProviderDef, models: ModelInfo[]) {
    clear(list);
    const current = activeModel(State.settings);
    for (const m of models) {
      const on = m.id === current;
      const row = h(
        "button",
        { class: on ? "picker-model on" : "picker-model", style: `--accent:${p.accent}`, title: m.id },
        h("span", { class: "picker-model-name", text: m.label }),
        on ? svg(ICONS.check, 11, { stroke: 2.2 }) : null,
      );
      row.addEventListener("click", () => {
        State.settings = withModel(State.settings, p.id, m.id);
        saveSettings();
        Sound.play("blip");
        close();
      });
      list.append(row);
    }
    list.querySelector(".picker-model.on")?.scrollIntoView({ block: "nearest" });
  }

  // ── ChatGPT: an account, not a key ──────────────────────────────────────────

  /** "Using ChatGPT plan · Manage usage", and who is signed in. */
  function planLine(s: ChatGptStatus): HTMLElement {
    return h(
      "div",
      { class: "picker-status plan" },
      h("span", { text: s.email ? `${t(STRINGS.usingPlan)} · ${s.email}` : t(STRINGS.usingPlan) }),
      h("button", { class: "picker-link", text: tl(STRINGS.manageUsage), onclick: () => openUrl(s.manageUsageUrl) }),
    );
  }

  function signInPrompt(p: ProviderDef, s: ChatGptStatus | null, error?: string) {
    clear(list);
    const again = s?.needsReconnect || (s?.connected && !s.planEnabled);
    const line = h("div", { class: "picker-status" }, h("span", {
      text: error ?? (s?.connected && !s.planEnabled ? t(STRINGS.planOff) : t(STRINGS.signInFirst)),
    }));
    const button = h("button", {
      class: "chatgpt-connect",
      text: tl(again ? STRINGS.reconnect : STRINGS.connect),
      onclick: () => void connect(p),
    });
    list.append(line, button);
  }

  async function connect(p: ProviderDef) {
    const ticket = ++request;
    const revision = chatgptRevision;
    status(t(STRINGS.connecting));
    try {
      const connected = await Bridge.chatgptConnect();
      if (revision === chatgptRevision) chatgpt = connected;
      Sound.play("approve");
      if (ticket === request) await loadChatGpt(p);
    } catch (err) {
      if (ticket === request) signInPrompt(p, chatgpt, String(err).replace(/^Error:\s*/, ""));
    }
    State.notify();
  }

  function drawChatGpt(p: ProviderDef, s: ChatGptStatus, models: ChatGptModel[]) {
    clear(list);
    list.append(planLine(s));
    const current = activeModel(State.settings);
    for (const m of models) {
      const on = m.id === current;
      const row = h(
        "button",
        { class: on ? "picker-model on" : "picker-model", style: `--accent:${p.accent}`, title: m.id },
        h("span", { class: "picker-model-name", text: m.label }),
        m.available ? null : h("span", { class: "picker-tag", text: t(STRINGS.unavailable) }),
        on ? svg(ICONS.check, 11, { stroke: 2.2 }) : null,
      ) as HTMLButtonElement;
      // Never swapped for another one: a model the account lacks can't be picked.
      row.disabled = !m.available;
      row.addEventListener("click", () => {
        if (!m.available) return;
        State.settings = withModel(State.settings, p.id, m.id);
        saveSettings();
        Sound.play("blip");
        close();
      });
      list.append(row);
    }
  }

  async function loadChatGpt(p: ProviderDef) {
    const ticket = ++request;
    const revision = chatgptRevision;
    // A reopened picker must not expose a previous account's enabled choices
    // while the native status request is still pending.
    status(t(STRINGS.loading));
    const s = await Bridge.chatgptStatus();
    if (ticket !== request || revision !== chatgptRevision) return;
    chatgpt = s ?? null;
    if (!s?.connected || !s.planEnabled) {
      signInPrompt(p, s ?? null);
      return;
    }
    if (catalog) drawChatGpt(p, s, catalog);
    else status(t(STRINGS.loading));
    try {
      const models = await Bridge.chatgptModels();
      if (ticket !== request || revision !== chatgptRevision) return;
      catalog = models;
      drawChatGpt(p, s, models);
      onChange();
    } catch (err) {
      if (ticket === request && revision === chatgptRevision) signInPrompt(p, s, String(err).replace(/^Error:\s*/, ""));
    }
  }

  async function loadModels() {
    const p = providerDef(State.settings.chatProvider);
    if (p.signIn) {
      await loadChatGpt(p);
      return;
    }
    const ticket = ++request;
    const cached = p.urlField ? undefined : cache.get(p.id);
    if (cached) {
      drawModels(p, cached);
      return;
    }
    // Nothing is asked of a provider that has no key yet.
    if (p.key && !(await Bridge.secretPresent(p.key))) {
      if (ticket === request) status(t(STRINGS.noKey), true);
      return;
    }
    if (ticket !== request) return;
    status(t(STRINGS.loading));
    try {
      const models = await Bridge.chatModels(p.id);
      if (ticket !== request) return;
      if (!p.urlField) cache.set(p.id, models);
      const keep = pickModel(p, models.map((m) => m.id), activeModel(State.settings));
      if (keep && keep !== activeModel(State.settings)) {
        State.settings = withModel(State.settings, p.id, keep);
        saveSettings();
        onChange();
      }
      drawModels(p, models);
    } catch (err) {
      if (ticket === request) status(String(err).replace(/^Error:\s*/, ""), Boolean(p.key));
    }
  }

  function open() {
    isOpen = true;
    el.classList.add("on");
    drawChips();
    onChange();
    void loadModels();
  }

  function close() {
    isOpen = false;
    request++;
    el.classList.remove("on");
    onChange();
  }

  return {
    el,
    open,
    close,
    get isOpen() {
      return isOpen;
    },
  };
}

// ── Conversation history ──────────────────────────────────────────────────────

/** When the conversation on screen was first saved. */
let chatCreatedAt = 0;
/** Its generated or explicitly renamed title, kept when more messages arrive. */
let chatTitle = "";
/** New/open invalidates old saves; regeneration stays in the same conversation. */
let chatSession = 0;
/** Only the latest save may change the active conversation's warning. */
let saveAttempt = 0;
let saveFailure: { id: string; error: string } | null = null;
/** Disk history has no attachment bytes, including records saved by older versions. */
let reopenedChatId: string | null = null;

/** Starts a fresh conversation: nothing on screen, nothing in the model's history. */
export function newConversation() {
  chatSession++;
  State.chatHistory = [];
  State.chatId = null;
  State.droppedFile = null;
  State.promptContext = null;
  saveFailure = null;
  reopenedChatId = null;
  void Bridge.chatReset();
  State.notify();
}

/** Saves the conversation on screen (after an answer, a Stop or a rename). */
function saveConversation() {
  const messages = State.chatHistory;
  if (messages.length === 0) return;
  if (!State.chatId) {
    State.chatId = newChatId();
    chatCreatedAt = Date.now();
    chatTitle = titleFor(messages);
  }
  const id = State.chatId;
  const attempt = ++saveAttempt;
  const session = chatSession;
  const isCurrent = () => attempt === saveAttempt && session === chatSession && State.chatId === id;
  void Bridge.chatHistorySave(toSaved(id, chatTitle, chatCreatedAt || Date.now(), messages)).then(() => {
    if (!isCurrent()) return;
    saveFailure = null;
    State.notify();
  }).catch((err) => {
    if (!isCurrent()) return;
    saveFailure = { id, error: String(err).replace(/^Error:\s*/, "") };
    Sound.play("error");
    State.notify();
  });
}

interface HistoryPanel {
  readonly isOpening: boolean;
  el: HTMLElement;
  open(): void;
  close(): void;
  readonly isOpen: boolean;
}

function buildHistory(onChange: () => void, onOpened: () => void): HistoryPanel {
  const list = h("div", { class: "picker-list" });
  const fresh = h("button", { class: "chatgpt-connect history-new", text: tl(STRINGS.newChat) });
  const el = h("div", { class: "picker history" }, h("div", { class: "history-head" }, h("span", { class: "history-title", text: tl(STRINGS.history) }), fresh), list);
  let isOpen = false;
  let armedDelete: string | null = null;
  let armedButton: HTMLButtonElement | null = null;
  const pendingDeletes = new Set<string>();
  const deleteFailures = new Map<string, string>();
  let summaries: ConversationSummary[] = [];
  let listRequest = 0;
  let navigationAttempt = 0;
  let panelSession = 0;
  let pendingOpens = 0;
  // Successful queued opens advance this shared snapshot in selection order.
  // A reset/file drop replaces it, invalidating the whole older group.
  let openingContext: { messages: ChatMessage[]; session: number } | null = null;

  function disarmDelete() {
    armedDelete = null;
    armedButton?.replaceChildren(svg(ICONS.trash, 11));
    armedButton = null;
  }

  fresh.addEventListener("click", () => {
    newConversation();
    Sound.play("blip");
    close();
  });

  function row(s: ConversationSummary): HTMLElement {
    const name = h("span", { class: "picker-model-name", text: s.title });
    const when = h("span", { class: "picker-tag", text: whenLabel(s.updatedAt) });
    const rename = h("button", { class: "history-act", title: tl(STRINGS.rename), "aria-label": tl(STRINGS.rename) }, svg(ICONS.pencil, 11));
    const del = h("button", { class: "history-act", title: tl(STRINGS.delete), "aria-label": tl(STRINGS.delete) }, svg(ICONS.trash, 11));
    const line = h("div", { class: State.chatId === s.id ? "picker-model on history-row" : "picker-model history-row", role: "button", tabindex: "0" }, name, when, rename, del);

    rename.disabled = del.disabled = pendingDeletes.has(s.id);
    line.addEventListener("click", () => void open_(s.id));
    rename.addEventListener("click", (e) => {
      e.stopPropagation();
      if (pendingDeletes.has(s.id)) return;
      const input = h("input", { class: "history-rename", value: s.title, maxlength: "120" }) as HTMLInputElement;
      name.replaceWith(input);
      input.focus();
      input.select();
      const done = async (keep: boolean) => {
        const title = keep ? cleanTitle(input.value) : null;
        if (title && title !== s.title) {
          await Bridge.chatHistoryRename(s.id, title).then(() => {
            if (State.chatId === s.id) chatTitle = title;
          }).catch(() => {});
        }
        void draw();
      };
      input.addEventListener("click", (ev) => ev.stopPropagation());
      input.addEventListener("keydown", (ev) => {
        ev.stopPropagation();
        if (ev.key === "Enter") void done(true);
        else if (ev.key === "Escape") void done(false);
      });
      input.addEventListener("blur", () => void done(true));
    });
    del.addEventListener("click", async (e) => {
      e.stopPropagation();
      if (pendingDeletes.has(s.id)) return;
      // A second click deletes: one slip of the mouse loses nothing.
      if (armedDelete !== s.id) {
        disarmDelete();
        armedDelete = s.id;
        armedButton = del;
        del.replaceChildren(h("span", { class: "history-confirm", text: t(STRINGS.confirmDelete) }));
        return;
      }
      const active = State.chatId === s.id;
      const messages = State.chatHistory;
      const count = messages.length;
      const session = chatSession;
      const navigation = navigationAttempt;
      pendingDeletes.add(s.id);
      deleteFailures.delete(s.id);
      render();
      try {
        await Bridge.chatHistoryDelete(s.id);
      } catch (err) {
        pendingDeletes.delete(s.id);
        deleteFailures.set(s.id, String(err).replace(/^Error:\s*/, ""));
        render();
        if (isOpen) Sound.play("error");
        return;
      }
      pendingDeletes.delete(s.id);
      deleteFailures.delete(s.id);
      // Only the untouched conversation that was confirmed may be cleared.
      // Navigation, file drops and new messages can all happen during IPC.
      if (active && State.chatId === s.id && chatSession === session && navigationAttempt === navigation
        && State.chatHistory === messages && messages.length === count) newConversation();
      summaries = summaries.filter((item) => item.id !== s.id);
      render();
      if (isOpen) Sound.play("pop");
      void draw();
    });
    return line;
  }

  function render() {
    disarmDelete();
    clear(list);
    if (!summaries.length) {
      list.append(h("div", { class: "picker-status", text: t(STRINGS.noHistory) }));
      return;
    }
    for (const summary of summaries) {
      list.append(row(summary));
      const error = deleteFailures.get(summary.id);
      if (error) list.append(h("div", { class: "picker-status history-error", role: "alert", text: error }));
    }
  }

  async function draw() {
    const request = ++listRequest;
    const items = await Bridge.chatHistoryList();
    if (request !== listRequest || items === null) return;
    summaries = items;
    render();
  }

  async function open_(id: string) {
    if (pendingDeletes.has(id)) return;
    const attempt = ++navigationAttempt;
    const panel = panelSession;
    if (!openingContext || openingContext.messages !== State.chatHistory || openingContext.session !== chatSession) {
      openingContext = { messages: State.chatHistory, session: chatSession };
    }
    const context = openingContext;
    const isCurrent = () => context.messages === State.chatHistory && context.session === chatSession;
    pendingOpens++;
    onChange();
    State.notify();
    try {
      const saved = await Bridge.chatHistoryOpen(id);
      if (!isCurrent()) return;
      chatSession++;
      State.chatHistory = fromSaved(saved, nextId);
      context.messages = State.chatHistory;
      context.session = chatSession;
      nextId += saved.messages.length;
      State.chatId = saved.id;
      reopenedChatId = saved.id;
      chatCreatedAt = saved.createdAt;
      chatTitle = saved.title;
      saveFailure = null;
      State.droppedFile = null;
      State.promptContext = null;
      Sound.play("blip");
      if (attempt === navigationAttempt) close();
      onOpened();
    } catch (err) {
      if (!isCurrent() || attempt !== navigationAttempt || panel !== panelSession || !isOpen) return;
      clear(list);
      list.append(h("div", { class: "picker-status", text: String(err).replace(/^Error:\s*/, "") }));
    } finally {
      pendingOpens--;
      if (!pendingOpens) openingContext = null;
      onChange();
      State.notify();
    }
  }

  function open() {
    panelSession++;
    isOpen = true;
    disarmDelete();
    el.classList.add("on");
    onChange();
    void draw();
  }

  function close() {
    panelSession++;
    isOpen = false;
    disarmDelete();
    el.classList.remove("on");
    onChange();
  }

  return {
    el,
    open,
    close,
    get isOpen() {
      return isOpen;
    },
    get isOpening() {
      return pendingOpens > 0;
    },
  };
}

// ── Drafts ────────────────────────────────────────────────────────────────────

/** The chat's text field, once the view is built. */
let chatInput: HTMLInputElement | null = null;
/** The next focus leaves the caret after a draft instead of selecting it. */
let keepDraft = false;

/**
 * Puts `text` in the chat field without sending it — the assistant's
 * `chat-draft` action. True only when the field really holds it afterwards:
 * that is the action's evidence.
 */
export function applyChatDraft(text: string): boolean {
  if (!chatInput || chatInput.disabled) return false;
  chatInput.value = text;
  keepDraft = true;
  return chatInput.value === text;
}

// ── View ──────────────────────────────────────────────────────────────────────

export function buildPrompt(onHeightChange: () => void): ViewHost {
  const chipRow = h("div", { class: "chip-row" });
  const log = h("div", { class: "chat-log" });
  const input = h("input", {
    type: "text",
    class: "chat-input",
    placeholder: t(STRINGS.placeholderFirst),
    spellcheck: "false",
  }) as HTMLInputElement;
  chatInput = input;
  const send = h("button", { class: "send-btn", title: tl(STRINGS.send), "aria-label": tl(STRINGS.send) }, svg(ICONS.arrowUp, 11));
  const voice = HOST_OS === "windows" ? h("button", {
    type: "button",
    class: "voice-btn",
    title: tl(STRINGS.voice),
    "aria-label": tl(STRINGS.voice),
  }, svg(ICONS.mic, 15, { stroke: 1.8 })) : null;
  const bar = h("div", { class: "chat-bar" }, input, voice, send);

  const modelDot = h("i", { class: "model-dot" });
  const modelName = h("span", { class: "model-name" });
  const modelBtn = h(
    "button",
    { class: "model-btn", title: tl(STRINGS.switchModel) },
    modelDot,
    modelName,
    svg(ICONS.chevronUpDown, 9, { stroke: 2 }),
  );
  // ChatGPT's reasoning effort, for the model picked: a native menu, so the
  // keyboard and screen readers know it.
  const effort = h("select", { class: "effort-select", title: tl(STRINGS.effort), "aria-label": tl(STRINGS.effort) }) as HTMLSelectElement;
  const historyBtn = h("button", { class: "history-btn", title: tl(STRINGS.history), "aria-label": tl(STRINGS.history) }, svg(ICONS.clock, 11, { stroke: 1.8 }));
  const modelRow = h("div", { class: "model-row" }, historyBtn, h("span", { class: "model-spacer" }), effort, modelBtn);

  const notice = h("div", { class: "chat-notice-slot" });
  const restoredNotice = h("div", { class: "chat-notice-slot" });
  const body = h("div", { class: "chat-body" });
  const picker = buildPicker(() => {
    body.classList.toggle("picking", picker.isOpen || history.isOpen);
    drawModelButton();
  });
  const history = buildHistory(
    () => {
      body.classList.toggle("picking", picker.isOpen || history.isOpen);
      drawModelButton();
    },
    () => {
      renderedCount = -1;
      State.notify();
      onHeightChange();
      input.focus();
    },
  );
  body.append(chipRow, notice, restoredNotice, log, picker.el, history.el, modelRow, bar);

  const el = h("div", { class: "view" }, h("div", { class: "card wash chat-card" }, body));
  (el.querySelector(".card") as HTMLElement).style.setProperty("--wash", "rgba(99,102,241,0.5)");

  let sending = false;
  let undoPending = false;
  let stopRequested = false;
  // Reserve the composer before the native microphone startup can yield.
  let startingVoice = false;
  let renderedCount = -1;
  // A streaming provider answers token by token: where its text so far is shown.
  let live: HTMLElement | null = null;
  let liveText = "";
  /** A file drop or reset can replace the transcript while IPC is pending. */
  let answerIsCurrent: (() => boolean) | null = null;
  /** ChatGPT said its usage limit is reached: the notice offers "Manage usage". */
  let usageLimit = false;
  let effortKey = "";

  function onChatGpt(): boolean {
    return providerDef(State.settings.chatProvider).id === CHATGPT;
  }

  function drawEffort() {
    const show = onChatGpt();
    effort.hidden = !show;
    if (!show) return;
    const model = activeModel(State.settings);
    const current = effortFor(State.settings, model);
    const key = `${model}|${current}|${effortsFor(model).join(",")}`;
    if (key !== effortKey) {
      effortKey = key;
      effort.replaceChildren(...effortsFor(model).map((e) => h("option", { value: e, text: t(EFFORT_LABELS[e]) })));
      effort.value = current;
    }
    effort.disabled = sending;
  }

  effort.addEventListener("change", () => {
    const next = withEffort(State.settings, activeModel(State.settings), effort.value);
    if (!next) return;
    State.settings = next;
    saveSettings();
    Sound.play("blip");
    drawEffort();
  });

  function drawModelButton() {
    const p = providerDef(State.settings.chatProvider);
    const model = activeModel(State.settings);
    modelDot.style.background = p.accent;
    modelName.textContent = (p.signIn ? catalog?.find((c) => c.id === model)?.label ?? modelLabel(model) : model) || t(STRINGS.noModel);
    modelBtn.classList.toggle("open", picker.isOpen);
    historyBtn.classList.toggle("open", history.isOpen);
    modelBtn.disabled = sending;
    historyBtn.disabled = sending;
    if (voice) voice.disabled = sending || startingVoice || history.isOpening;
    // While an answer comes, the send button is Stop.
    send.disabled = startingVoice || undoPending || history.isOpening;
    send.classList.toggle("stop", sending);
    const label = t(sending ? STRINGS.stop : STRINGS.send);
    send.title = label;
    send.setAttribute("aria-label", label);
    send.replaceChildren(svg(sending ? ICONS.stopSquare : ICONS.arrowUp, sending ? 9 : 11));
    drawEffort();
  }

  /** Save failures for every provider, or ChatGPT's welcome and usage limit. */
  function drawNotice() {
    const restored = State.chatId !== null && State.chatId === reopenedChatId ? State.chatId : "";
    if (restoredNotice.dataset.chat !== restored) {
      restoredNotice.dataset.chat = restored;
      clear(restoredNotice);
      if (restored) restoredNotice.append(h("div", { class: "chat-notice chat-restored", role: "status" },
        h("div", { class: "chat-notice-title", text: tl(STRINGS.restoredTitle) }),
        h("div", { class: "chat-notice-body", text: tl(STRINGS.restoredBody) }),
      ));
      onHeightChange();
    }
    const failure = saveFailure?.id === State.chatId ? saveFailure : null;
    let want = "";
    if (failure) want = `save:${failure.error}`;
    else if (onChatGpt() && usageLimit) want = "limit";
    else if (onChatGpt() && chatgpt?.showWelcome) want = "welcome";
    const noticeKey = want === "welcome" ? `${want}:${chatgpt?.accountId ?? ""}` : want;
    if (notice.dataset.kind === noticeKey) return;
    notice.dataset.kind = noticeKey;
    clear(notice);
    if (!want) return;
    if (failure) {
      const error = failure.error === "too many messages" ? t(STRINGS.historyFull) : failure.error;
      notice.append(h("div", { class: "chat-notice warn", role: "alert" },
        h("div", { class: "chat-notice-title", text: t(STRINGS.notSaved) }),
        h("div", { class: "chat-notice-body", text: t(STRINGS.saveFailed, { error }) }),
      ));
      onHeightChange();
      return;
    }
    const manage = h("button", {
      class: "picker-link",
      text: tl(STRINGS.manageUsage),
      onclick: () => openUrl(chatgpt?.manageUsageUrl ?? "https://chatgpt.com/#settings"),
    });
    if (want === "limit") {
      notice.append(h("div", { class: "chat-notice warn" },
        h("div", { class: "chat-notice-title", text: t(STRINGS.limitTitle) }),
        h("div", { class: "chat-notice-body", text: t(STRINGS.limitBody) }),
        h("div", { class: "chat-notice-actions" }, manage),
      ));
    } else {
      const accountId = chatgpt?.accountId ?? null;
      const ok = h("button", {
        class: "chatgpt-connect",
        text: tl(STRINGS.gotIt),
        onclick: () => void Bridge.chatgptWelcomed(accountId).then(() => refreshChatGpt(drawNotice)),
      });
      notice.append(h("div", { class: "chat-notice" },
        h("div", { class: "chat-notice-title", text: t(STRINGS.welcomeTitle) }),
        h("div", { class: "chat-notice-body", text: t(STRINGS.welcomeBody) }),
        h("div", { class: "chat-notice-actions" }, ok, manage),
      ));
    }
    onHeightChange();
  }

  /** One message: a user bubble, or an answer with its signature, Copy and (last) Regenerate. */
  function bubble(message: ChatMessage, last: boolean): HTMLElement {
    if (message.role === "user") {
      return h("div", { class: "chat-row user" }, h("div", { class: "bubble", text: message.content }));
    }
    const reply = h("div", { class: "reply" });
    renderMarkdown(reply, message.content);
    const meta = h("div", { class: "reply-meta" });
    const sig = signature(message);
    if (sig) meta.append(h("span", { class: "reply-sig", text: sig }));
    if (message.stopped) meta.append(h("span", { class: "reply-stopped", text: t(STRINGS.stopped) }));
    meta.append(copyButton(message.content));
    if (last && !sending) {
      const again = h("button", { class: "md-copy", title: tl(STRINGS.regenerate), "aria-label": tl(STRINGS.regenerate) }, svg(ICONS.arrowClockwise, 11, { stroke: 2 }));
      again.addEventListener("click", () => void regenerate());
      meta.append(again);
    }
    return h("div", { class: "chat-row" }, h("div", { class: "reply-wrap" }, reply, meta));
  }

  modelBtn.addEventListener("click", () => {
    if (sending) return;
    if (history.isOpen) history.close();
    if (picker.isOpen) picker.close();
    else picker.open();
  });
  historyBtn.addEventListener("click", () => {
    if (sending) return;
    if (picker.isOpen) picker.close();
    if (history.isOpen) history.close();
    else history.open();
  });

  void onEvent<string>("chat-delta", (text) => {
    if (!sending || undoPending || !answerIsCurrent?.() || !text) return; // no current answer yet: the dots stay
    liveText = text;
    if (!live) {
      live = h("div", { class: "reply" });
      log.querySelector(".typing")?.parentElement?.remove();
      log.append(h("div", { class: "chat-row" }, live));
    }
    renderMarkdown(live, text);
    log.scrollTop = log.scrollHeight;
  });
  void onEvent<null>("chatgpt-usage-limit", () => {
    if (!answerIsCurrent?.()) return;
    usageLimit = true;
    drawNotice();
  });
  void onEvent<ChatGptStatus>("chatgpt-status", (s) => {
    chatgptRevision++;
    chatgpt = s;
    catalog = null; // another account, or none: its catalog is asked again
    // Every status event invalidates pending catalog work, including a welcome
    // acknowledgement on the same account. Don't leave its spinner stranded.
    if (onChatGpt() && picker.isOpen) picker.close();
    State.notify();
  });

  async function ask(query: string) {
    if (picker.isOpen) picker.close();
    if (history.isOpen) history.close();
    sending = true;
    stopRequested = false;
    liveText = "";
    const messages = State.chatHistory;
    const session = chatSession;
    const isCurrent = () => State.chatHistory === messages && chatSession === session;
    answerIsCurrent = isCurrent;
    drawModelButton();
    Sound.play("send");

    // Who answers, as picked now: recorded with the answer.
    const p = providerDef(State.settings.chatProvider);
    const model = activeModel(State.settings);
    const answeredBy = {
      provider: p.id,
      model,
      ...(p.id === CHATGPT ? { effort: effortFor(State.settings, model) } : {}),
    };

    State.chatHistory.push({ id: nextId++, role: "user", content: query });
    State.stateOverride = "thinking";
    State.notify();
    onHeightChange();

    const file = State.droppedFile;
    // Stop/failure can leave visible messages without a committed native turn.
    // Rust decides whether this candidate belongs on its first completed turn.
    const context: ChatContext | null = file ? { kind: "file", name: file.name, path: file.path } : null;

    try {
      const reply = await Bridge.chatSend(query, context);
      if (!isCurrent()) return;
      State.chatHistory.push({ id: nextId++, role: "assistant", content: reply.text, ...answeredBy });
      State.stateOverride = null;
      usageLimit = false;
      Sound.play("finish");
      saveConversation();
    } catch (err) {
      if (!isCurrent()) return;
      State.stateOverride = null;
      if (stopRequested) {
        // What arrived before Stop stays on screen, marked; the model never sees it again.
        if (liveText) State.chatHistory.push({ id: nextId++, role: "assistant", content: liveText, stopped: true, ...answeredBy });
        saveConversation();
      } else if (usageLimit && p.id === CHATGPT) {
        drawNotice();
        Sound.play("error");
      } else {
        State.noteMessage = String(err).replace(/^Error:\s*/, "");
        State.view = "note";
        Sound.play("error");
      }
    } finally {
      sending = false;
      stopRequested = false;
      // A reset can replace the transcript while this request still owns the
      // thinking state. Release that state, but preserve any newer override.
      if (State.stateOverride === "thinking") State.stateOverride = null;
      live = null;
      answerIsCurrent = null;
      renderedCount = -1; // the finished answer replaces the streamed one
      drawModelButton();
      State.notify();
      onHeightChange();
      input.focus();
    }
  }

  async function submit() {
    const query = input.value.trim();
    if (!query || sending || startingVoice || history.isOpening) return;
    input.value = "";
    await ask(query);
  }

  /** The last answer again, with the model and effort picked now. */
  async function regenerate() {
    if (sending || startingVoice || history.isOpening) return;
    const messages = State.chatHistory;
    const answer = messages[messages.length - 1];
    const question = messages[messages.length - 2];
    if (answer?.role !== "assistant" || question?.role !== "user") return;
    // Reserve every entry point before native undo yields. It is an atomic
    // preparation, not an answer to Stop; keep the visible turn until success.
    sending = true;
    undoPending = true;
    State.stateOverride = "thinking";
    drawModelButton();
    State.notify();
    let undone = false;
    try {
      // A stopped answer never entered the model's history.
      undone = !!answer.stopped || (await Bridge.chatUndoLast()) === true;
    } catch {
      // A rejected command must not discard the displayed answer either.
    }
    undoPending = false;
    if (!undone || State.chatHistory !== messages) {
      sending = false;
      State.stateOverride = null;
      renderedCount = -1;
      if (!undone && State.chatHistory === messages && State.view === "prompt") {
        State.noteMessage = t(STRINGS.regenerateFailed);
        State.view = "note";
        Sound.play("error");
      }
      drawModelButton();
      State.notify();
      onHeightChange();
      return;
    }
    State.chatHistory = messages.slice(0, -2);
    await ask(question.content);
  }

  send.addEventListener("click", () => {
    if (undoPending) return;
    if (sending) {
      stopRequested = true;
      void Bridge.chatStop();
      return;
    }
    void submit();
  });
  voice?.addEventListener("click", async () => {
    if (sending || startingVoice || history.isOpening) return;
    startingVoice = true;
    if (picker.isOpen) picker.close();
    drawModelButton();
    try {
      await voiceShortcut();
    } catch {
      // Keep the draft and any newer permission card in place on failure.
      Sound.play("error");
    } finally {
      startingVoice = false;
      drawModelButton();
    }
  });
  input.addEventListener("keydown", (e) => {
    const key = (e as KeyboardEvent).key;
    if (key === "Enter") {
      e.preventDefault();
      void submit();
    } else if (key === "Escape" && (picker.isOpen || history.isOpen)) {
      e.preventDefault();
      if (picker.isOpen) picker.close();
      if (history.isOpen) history.close();
    } else if (key === "Escape" && sending && !undoPending) {
      e.preventDefault();
      stopRequested = true;
      void Bridge.chatStop();
    }
    e.stopPropagation(); // Escape closes the island, not the chat
  });

  refreshChatGpt();

  return {
    el,
    sync() {
      const file = State.droppedFile;
      const wantChip = file?.name ?? "";
      if (chipRow.dataset.label !== wantChip) {
        chipRow.dataset.label = wantChip;
        clear(chipRow);
        if (wantChip) chipRow.append(contextChip(wantChip));
      }

      const thinking = State.stateOverride === "thinking";
      const count = State.chatHistory.length + (thinking ? 0.5 : 0);
      if (count !== renderedCount && !live) {
        renderedCount = count;
        clear(log);
        const messages = State.chatHistory;
        messages.forEach((m, i) => log.append(bubble(m, i === messages.length - 1)));
        if (thinking) log.append(typingDots());
        log.scrollTop = log.scrollHeight;
      }

      // Leaving the chat folds the picker and the history away.
      if (State.view !== "prompt" && picker.isOpen) picker.close();
      if (State.view !== "prompt" && history.isOpen) history.close();
      drawModelButton();
      drawNotice();

      input.placeholder = t(State.chatHistory.length === 0 ? STRINGS.placeholderFirst : STRINGS.placeholderNext);
      input.disabled = sending || history.isOpening;
    },
    focus() {
      input.focus();
      if (keepDraft) {
        keepDraft = false;
        input.setSelectionRange(input.value.length, input.value.length);
      } else {
        input.select();
      }
    },
  };
}

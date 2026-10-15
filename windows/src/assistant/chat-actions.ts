// The chat's action cards: when ChatGPT wants to do something on the PC
// (assistant/tools.rs), Rust shows what exactly, and waits. "Do it" hands
// Rust's grant back; "No" says no. A card goes away when Rust stops waiting
// (answered, two minutes, Stop). With "Do it without asking" on, Rust runs
// some actions without a card and only reports what came of them: those
// are notes here, never buttons.

import { Bridge, onEvent } from "../core/bridge";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import { ACTION_KINDS, type ActionKind, type AppGrant } from "./contracts";

export interface ActionCard {
  /** The answer that owns this card; never an authorization grant. */
  requestId: string;
  actionId: string;
  kind: ActionKind;
  /** The tab's title, or the app's name. */
  label: string;
  /** The browser, or "app". */
  detail: string;
  grant: AppGrant;
  /** True once answered, until Rust closes it. */
  answered?: boolean;
}

/** An action Rust ran without a card, and what came of it. */
export interface AutoNote {
  kind: ActionKind;
  /** The app's name, the tab's title, the page's site or the memory's text. */
  label: string;
  outcome: "succeeded" | "failed" | "cancelled" | "unknown";
}

export const ChatActions = {
  cards: [] as ActionCard[],
  /** Read-only tools used during the current answer ("browser-tabs"). */
  used: [] as string[],
  /** Actions that ran without a card during the current answer. */
  auto: [] as AutoNote[],
};

/** Correlation only: this never grants permission for an action. */
interface ToolUsed {
  requestId: string | null;
  tool: string;
  kind?: unknown;
  label?: unknown;
  outcome?: unknown;
}

const OUTCOMES: readonly AutoNote["outcome"][] = ["succeeded", "failed", "cancelled", "unknown"];

function autoNote(event: ToolUsed): AutoNote | null {
  const { kind, label, outcome } = event;
  if (!ACTION_KINDS.some((entry) => entry.kind === kind) || typeof label !== "string"
    || !OUTCOMES.includes(outcome as AutoNote["outcome"])) return null;
  return { kind: kind as ActionKind, label, outcome: outcome as AutoNote["outcome"] };
}

let toolNotesFor: { requestId: string; messages: typeof State.chatHistory } | null = null;

function ownsRequest(requestId: unknown): boolean {
  return toolNotesFor !== null && requestId === toolNotesFor.requestId
    && toolNotesFor.messages === State.chatHistory;
}

function ownsCard(card: ActionCard): boolean {
  return ownsRequest(card.requestId) && ChatActions.cards.includes(card);
}

/** Malformed IPC payloads never become a confirmation button. Rust checks the grant itself. */
function isActionCard(raw: unknown): raw is ActionCard {
  if (typeof raw !== "object" || raw === null) return false;
  const card = raw as Partial<ActionCard>;
  const grant = card.grant;
  return typeof card.requestId === "string" && !!card.requestId.trim()
    && typeof card.actionId === "string" && !!card.actionId.trim()
    && ACTION_KINDS.some(({ kind }) => kind === card.kind)
    && typeof card.label === "string" && typeof card.detail === "string"
    && typeof grant === "object" && grant !== null
    && grant.actionId === card.actionId
    && typeof grant.digest === "string" && /^[0-9a-f]{64}$/i.test(grant.digest)
    && Number.isSafeInteger(grant.expiresAtMs) && grant.expiresAtMs > 0;
}

/** Change ownership before notifying: a synchronous reset must stay retired. */
function replaceRequest(next: typeof toolNotesFor, forgetNotes: boolean) {
  const changed = ChatActions.cards.length > 0
    || (forgetNotes && (ChatActions.used.length > 0 || ChatActions.auto.length > 0));
  toolNotesFor = next;
  ChatActions.cards = [];
  if (forgetNotes) {
    ChatActions.used = [];
    ChatActions.auto = [];
  }
  if (changed) State.notify();
}

/** A fresh answer owns its notes, even when an older IPC event arrives late. */
export function beginToolNotes(spokenRequestId?: string): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  const requestId = spokenRequestId ?? [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  replaceRequest({ requestId, messages: State.chatHistory }, true);
  return requestId;
}

export function answerCard(actionId: string, yes: boolean) {
  const card = ChatActions.cards.find((c) => c.actionId === actionId);
  if (!card || card.answered || !ownsCard(card)) return;
  card.answered = true;
  State.notify();
  // A subscriber may stop/reset the answer or close this card while redrawing.
  if (!ownsCard(card)) return;
  Sound.play(yes ? "approve" : "blip");
  void Bridge.chatActionAnswer(actionId, yes ? card.grant : null).catch(() => {
    // Rust stopped waiting meanwhile: the card goes.
    close(actionId);
  });
}

function close(actionId: string) {
  const before = ChatActions.cards.length;
  ChatActions.cards = ChatActions.cards.filter((c) => c.actionId !== actionId);
  if (ChatActions.cards.length !== before) State.notify();
}

/** Stop/completion retires cards and late events, keeping tools already used visible. */
export function retireActionCards() {
  replaceRequest(null, false);
}

/** A new question or a new chat retires both its cards and its tool notes. */
export function clearToolNotes() {
  replaceRequest(null, true);
}

export function registerChatActions() {
  void onEvent<unknown>("chat-action", (card) => {
    if (!isActionCard(card) || !ownsRequest(card.requestId)
      || ChatActions.cards.some((c) => c.actionId === card.actionId)) return;
    ChatActions.cards.push({ ...card, answered: false });
    Sound.play("tick");
    State.notify();
  });
  void onEvent<string>("chat-action-closed", close);
  void onEvent<ToolUsed>("chat-tool-used", (event) => {
    if (!ownsRequest(event?.requestId)) return;
    if (event.tool === "auto-action") {
      const note = autoNote(event);
      if (!note) return;
      ChatActions.auto.push(note);
    } else if (event.tool === "browser-tabs") {
      if (!ChatActions.used.includes(event.tool)) ChatActions.used.push(event.tool);
    } else {
      return;
    }
    State.notify();
  });
}

/** Tests only. */
export function resetChatActionsForTests() {
  toolNotesFor = null;
  ChatActions.cards = [];
  ChatActions.used = [];
  ChatActions.auto = [];
}

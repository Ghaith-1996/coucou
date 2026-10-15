// The chat's action cards: when ChatGPT wants to do something on the PC
// (assistant/tools.rs), Rust shows what exactly, and waits. "Do it" hands
// Rust's grant back; "No" says no. Nothing runs without the click, and a
// card goes away when Rust stops waiting (answered, two minutes, Stop).

import { Bridge, onEvent } from "../core/bridge";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import type { ActionKind, AppGrant } from "./contracts";

export interface ActionCard {
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

export const ChatActions = {
  cards: [] as ActionCard[],
  /** Read-only tools used during the current answer ("browser-tabs"). */
  used: [] as string[],
};

/** Correlation only: this never grants permission for an action. */
interface ToolUsed {
  requestId: string | null;
  tool: string;
}

let toolNotesFor: { requestId: string; messages: typeof State.chatHistory } | null = null;

/** A fresh answer owns its notes, even when an older IPC event arrives late. */
export function beginToolNotes(): string {
  clearToolNotes();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  const requestId = [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  toolNotesFor = { requestId, messages: State.chatHistory };
  return requestId;
}

export function answerCard(actionId: string, yes: boolean) {
  const card = ChatActions.cards.find((c) => c.actionId === actionId);
  if (!card || card.answered) return;
  card.answered = true;
  State.notify();
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

/** A new question or a new chat: what the last answer used is forgotten. */
export function clearToolNotes() {
  toolNotesFor = null;
  if (!ChatActions.used.length) return;
  ChatActions.used = [];
  State.notify();
}

export function registerChatActions() {
  void onEvent<ActionCard>("chat-action", (card) => {
    if (!card?.actionId || ChatActions.cards.some((c) => c.actionId === card.actionId)) return;
    ChatActions.cards.push({ ...card, answered: false });
    Sound.play("tick");
    State.notify();
  });
  void onEvent<string>("chat-action-closed", close);
  void onEvent<ToolUsed>("chat-tool-used", (event) => {
    if (!toolNotesFor || event?.requestId !== toolNotesFor.requestId
      || toolNotesFor.messages !== State.chatHistory || event.tool !== "browser-tabs") return;
    if (!ChatActions.used.includes(event.tool)) ChatActions.used.push(event.tool);
    State.notify();
  });
}

/** Tests only. */
export function resetChatActionsForTests() {
  toolNotesFor = null;
  ChatActions.cards = [];
  ChatActions.used = [];
}

// The chat's saved conversations, the island's side of chat_store.rs: ids,
// titles and the record Rust checks and writes. Pure, testable without a
// webview.

import type { SavedConversation } from "./bridge";
import type { ChatMessage } from "./state";

/** `c-` and 16 hex digits: the only id Rust accepts (it names a file). */
export function newChatId(): string {
  const bytes = new Uint8Array(8);
  crypto.getRandomValues(bytes);
  return `c-${[...bytes].map((b) => b.toString(16).padStart(2, "0")).join("")}`;
}

const TITLE_CHARS = 60;

/** The first question, on one line, cut short: what the history list shows. */
export function titleFor(messages: readonly ChatMessage[]): string {
  const first = messages.find((m) => m.role === "user")?.content ?? "";
  const line = first.replace(/[\u0000-\u001f\u007f]+/g, " ").replace(/\s+/g, " ").trim();
  if (!line) return "Chat";
  return [...line].length > TITLE_CHARS ? `${[...line].slice(0, TITLE_CHARS - 1).join("")}…` : line;
}

/** A title the user typed, as Rust will accept it, or null. */
export function cleanTitle(raw: string): string | null {
  const line = raw.replace(/[\u0000-\u001f\u007f]+/g, " ").replace(/\s+/g, " ").trim();
  if (!line) return null;
  return [...line].slice(0, 120).join("");
}

/** The record to save; ids of the island's messages stay in the island. */
export function toSaved(id: string, title: string, createdAt: number, messages: readonly ChatMessage[]): SavedConversation {
  return {
    id,
    title,
    createdAt,
    updatedAt: Date.now(),
    messages: messages.map(({ id: _id, ...rest }) => rest),
  };
}

/** A reopened conversation's messages, numbered from `nextId`. */
export function fromSaved(saved: SavedConversation, nextId: number): ChatMessage[] {
  return saved.messages.map((m, i) => ({ ...m, id: nextId + i }));
}

/** "Today 14:05", or the date, for the history list. */
export function whenLabel(ms: number, now = Date.now(), locale?: string): string {
  const d = new Date(ms);
  const sameDay = new Date(now).toDateString() === d.toDateString();
  return sameDay
    ? d.toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleDateString(locale, { day: "numeric", month: "short" });
}

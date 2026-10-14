// The voice card: Mochi listening with a live level, then the transcript to
// use or cancel. A new view in the existing card style (views.ts helpers);
// nothing that already ships changes.

import { h } from "../views/dom";
import { btn, card, stack, type ViewHost } from "../views/views";
import { State } from "../core/state";
import { washRGBA, type Wash } from "../core/layout";
import { N_, t, tl } from "../i18n/i18n";
import { Voice, voiceCancel, voiceCopy, voiceDo, voiceInsert, voiceOpen, voiceStop, voiceUse } from "./voice";
import type { InsertRefusal, VoiceError } from "./contracts";

const ERRORS: Record<VoiceError, string> = {
  "no-microphone": N_("Microphone unavailable"),
  "microphone-denied": N_(
    "Windows blocks the microphone for desktop apps. Allow it in Settings → Privacy & security → Microphone.",
  ),
  silence: N_("Nothing was heard."),
  provider: N_("Speech recognition failed."),
};

// Why the words didn't go into the other app's field. Nothing was typed.
const REFUSALS: Record<InsertRefusal, string> = {
  "focus-moved": N_("You switched windows, so nothing was typed. Go back to the field, then Insert again."),
  gone: N_("That field is gone. Nothing was typed."),
  protected: N_("That's a password field: Coucou never types there."),
  "read-only": N_("That field is read-only."),
  unsupported: N_("This field doesn't take dictated text. Copy it, or use it in chat."),
  busy: N_("The app didn't answer. Nothing was typed."),
};

function clock(ms: number): string {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

export function buildVoice(): ViewHost {
  const title = h("div", { class: "title" });
  const detail = h("div", { class: "sub" });
  const fill = h("i");
  const meter = h("div", { class: "voice-meter", role: "meter", "aria-valuemin": "0", "aria-valuemax": "1" }, fill);
  const actions = h("div", { class: "actions" });
  // A washed card, coloured by state in sync().
  const box = card("soft", stack(116, 16, title, meter, detail, actions));
  const el = h("div", { class: "view voice" }, box);

  const buttons = {
    stop: btn(tl("Stop"), "primary", voiceStop),
    use: btn(tl("Use in chat"), "primary", () => void voiceUse(), "↵"),
    // Next to a pill command, the chat is the other choice.
    useToo: btn(tl("Use in chat"), "secondary", () => void voiceUse()),
    // The pill command itself; its label is the change ("Add GitHub").
    act: btn("", "primary", () => void voiceDo(), "↵"),
    // "Open <app>", named as the Start menu names it.
    open: btn("", "primary", () => void voiceOpen(), "↵"),
    // "Insert into <app>": dictation into the field the capture started in.
    insert: btn("", "primary", () => void voiceInsert(), "↵"),
    copy: btn(tl("Copy"), "secondary", () => void voiceCopy()),
    cancel: btn(tl("Cancel"), "secondary", voiceCancel, "Esc"),
    close: btn(tl("Close"), "secondary", voiceCancel, "Esc"),
  };
  let shownButtons = "";

  // The island has the keyboard while the card is up (voice.ts takes it).
  window.addEventListener("keydown", (e) => {
    if (State.view !== "voice" || State.mode !== "expanded" || !Voice.event) return;
    if (e.key === "Enter" && Voice.event.state === "preview") {
      e.preventDefault();
      const app = Voice.app;
      const insert = Voice.insert;
      // While the app or the field is being looked up, or used, Enter waits.
      if (app?.status === "finding" || app?.status === "opening") return;
      if (insert?.status === "finding" || insert?.status === "inserting") return;
      void (app?.status === "ready" ? voiceOpen()
        : insert?.status === "ready" ? voiceInsert()
        : Voice.command?.ok ? voiceDo() : voiceUse());
    } else if (e.key === "Escape") {
      e.preventDefault();
      voiceCancel();
    }
  });

  return {
    el,
    sync() {
      const event = Voice.event;
      const state = event?.state ?? "idle";
      const wash: Wash = state === "error" ? "red" : state === "listening" ? "indigo" : "soft";
      box.style.setProperty("--wash", washRGBA(wash));
      el.dataset.state = state;

      meter.hidden = state !== "listening";
      const level = event?.level ?? 0;
      fill.style.width = `${Math.round(level * 100)}%`;
      meter.setAttribute("aria-valuenow", String(level));

      switch (state) {
        case "listening":
          title.textContent = t("Listening…");
          detail.textContent = `${clock(event?.elapsedMs ?? 0)} / 0:30`;
          break;
        case "processing":
          title.textContent = t("Transcribing…");
          detail.textContent = clock(event?.elapsedMs ?? 0);
          break;
        case "preview": {
          title.textContent = event?.transcript ?? "";
          const command = Voice.command;
          const app = Voice.app;
          const insert = Voice.insert;
          detail.textContent = event?.simulated
            ? t("Simulated transcript — real speech recognition comes next.")
            : app?.status === "finding" ? t("Looking for {name}…", { name: app.name })
            : app?.status === "missing" ? t("No app called “{name}” in the Start menu.", { name: app.name })
            : command && !command.ok ? command.reason
            : insert?.status === "refused" ? t(REFUSALS[insert.reason])
            : insert?.status === "ready" && insert.reason ? t(REFUSALS[insert.reason])
            : "";
          break;
        }
        case "error":
          title.textContent = t(ERRORS[event?.error ?? "provider"]);
          detail.textContent = "";
          break;
        default:
          title.textContent = "";
          detail.textContent = "";
      }

      const command = state === "preview" && Voice.command?.ok ? Voice.command : null;
      if (command) buttons.act.querySelector("span")!.textContent = command.label;
      const app = state === "preview" ? Voice.app : null;
      const offered = app?.status === "ready" || app?.status === "opening" ? app : null;
      if (offered) buttons.open.querySelector("span")!.textContent = t("Open {name}", { name: offered.label });
      (buttons.open as HTMLButtonElement).disabled = app?.status === "opening";
      const insert = state === "preview" ? Voice.insert : null;
      const field = insert?.status === "ready" || insert?.status === "inserting" ? insert : null;
      if (field) buttons.insert.querySelector("span")!.textContent = t("Insert into {name}", { name: field.label });
      (buttons.insert as HTMLButtonElement).disabled = insert?.status === "inserting";
      const want =
        state === "listening" ? "stop,cancel"
        : state === "processing" ? "cancel"
        : state === "preview"
          ? offered ? "open,useToo,cancel"
          : app?.status === "finding" ? "useToo,cancel"
          : field ? "insert,useToo,cancel"
          : insert?.status === "refused" && insert.reason !== "protected" ? "use,copy,cancel"
          : command ? "act,useToo,cancel" : "use,cancel"
        : "close";
      if (want !== shownButtons) {
        shownButtons = want;
        actions.replaceChildren(...want.split(",").map((k) => buttons[k as keyof typeof buttons]));
      }
    },
  };
}

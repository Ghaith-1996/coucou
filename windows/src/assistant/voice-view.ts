// The voice card: Mochi listening with a live level, then the transcript to
// use or cancel. A new view in the existing card style (views.ts helpers);
// nothing that already ships changes.

import { h } from "../views/dom";
import { btn, card, stack, type ViewHost } from "../views/views";
import { State } from "../core/state";
import { washRGBA, type Wash } from "../core/layout";
import { N_, t, tl } from "../i18n/i18n";
import { Voice, voiceCancel, voiceStop, voiceUse } from "./voice";
import type { VoiceError } from "./contracts";

const ERRORS: Record<VoiceError, string> = {
  "no-microphone": N_("Microphone unavailable"),
  "microphone-denied": N_(
    "Windows blocks the microphone for desktop apps. Allow it in Settings → Privacy & security → Microphone.",
  ),
  silence: N_("Nothing was heard."),
  provider: N_("Speech recognition failed."),
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
    cancel: btn(tl("Cancel"), "secondary", voiceCancel, "Esc"),
    close: btn(tl("Close"), "secondary", voiceCancel, "Esc"),
  };
  let shownButtons = "";

  // The island has the keyboard while the card is up (voice.ts takes it).
  window.addEventListener("keydown", (e) => {
    if (State.view !== "voice" || State.mode !== "expanded" || !Voice.event) return;
    if (e.key === "Enter" && Voice.event.state === "preview") {
      e.preventDefault();
      void voiceUse();
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
        case "preview":
          title.textContent = event?.transcript ?? "";
          detail.textContent = event?.simulated ? t("Simulated transcript — real speech recognition comes next.") : "";
          break;
        case "error":
          title.textContent = t(ERRORS[event?.error ?? "provider"]);
          detail.textContent = "";
          break;
        default:
          title.textContent = "";
          detail.textContent = "";
      }

      const want =
        state === "listening" ? "stop,cancel" : state === "processing" ? "cancel" : state === "preview" ? "use,cancel" : "close";
      if (want !== shownButtons) {
        shownButtons = want;
        actions.replaceChildren(...want.split(",").map((k) => buttons[k as keyof typeof buttons]));
      }
    },
  };
}

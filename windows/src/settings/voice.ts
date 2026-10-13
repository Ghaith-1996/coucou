// Settings → Voice: the speech models for the assistant's voice capture.
//
// Speech becomes text on this computer with whisper.cpp. Its models are the
// one thing that has to be downloaded, and only from this section, by a click,
// from the address shown. Rust checks each against a pinned size and SHA-256
// before using it (src-tauri/src/assistant/voice/model.rs). Several can be on
// disk; one is used.

import { Bridge, onEvent, type VoiceModel, type VoiceModelStatus } from "../core/bridge";
import { h, clear } from "../views/dom";
import { N_, t } from "../i18n/i18n";
import { statusDot } from "./parts";

/** What each model is good for, measured on a 6-core desktop CPU (docs/hybrid/P1-MODELS.md). */
const ABOUT: Record<string, string> = {
  "base": N_("Fastest. Fine for short commands, makes more mistakes."),
  "large-v3-turbo-q5_0": N_("Much more accurate, but much slower without a graphics card."),
  "large-v3-q5_0": N_("The most accurate, and the slowest."),
};

function size(bytes: number): string {
  return bytes >= 1_000_000_000 ? `${(bytes / 1_000_000_000).toFixed(2)} GB` : `${Math.round(bytes / 1_000_000)} MB`;
}

export function voiceSection(initial: VoiceModelStatus | null): HTMLElement | null {
  if (!initial?.supported) return null;
  let status = initial;
  let error: string | null = null;
  const head = h("h2");
  const body = h("div", { style: "display:flex;flex-direction:column;gap:12px" });
  const section = h("section", {}, head, body);

  const inUse = () => status?.models.find((m) => m.id === status?.selected && m.installed) ?? null;

  function row(model: VoiceModel): HTMLElement {
    const busy = status?.downloading;
    const selected = inUse()?.id === model.id;
    const actions = h("div", { class: "row" });
    if (busy === model.id) {
      const percent = model.size ? Math.floor(((status?.received ?? 0) / model.size) * 100) : 0;
      actions.append(
        h("span", { text: t("Downloading… {percent}%", { percent }) }),
        h("button", { onclick: () => void Bridge.voiceModelCancel() }, t("Cancel")),
      );
    } else if (!model.installed) {
      actions.append(h("button", {
        class: "primary",
        disabled: busy != null,
        onclick: () => void download(model.id),
      }, t("Download ({size})", { size: size(model.size) })));
    } else {
      if (selected) {
        actions.append(h("span", { class: "hint", text: t("In use") }));
      } else {
        actions.append(h("button", { class: "primary", onclick: () => void run(() => Bridge.voiceModelSelect(model.id)) }, t("Use this model")));
      }
      actions.append(h("button", { class: "danger", onclick: () => void run(() => Bridge.voiceModelDelete(model.id)) }, t("Delete the model")));
    }
    return h("div", { style: "display:flex;flex-direction:column;gap:6px" },
      h("div", { class: "row" },
        statusDot(model.installed),
        h("label", { text: model.label }),
        h("span", { class: "path", text: `${model.file} · ${size(model.size)}` }),
      ),
      h("div", { class: "hint", text: t(ABOUT[model.id] ?? "") }),
      h("div", { class: "row" }, h("span", { class: "path", text: model.source })),
      actions,
    );
  }

  function draw() {
    clear(head);
    head.append(statusDot(inUse() != null), h("span", { text: t("Voice") }));
    clear(body);
    body.append(
      h("div", {
        class: "hint",
        text: t("Speech recognition runs on this computer with Whisper. Its model is downloaded once, from the address below. No audio or text leaves your computer."),
      }),
      h("div", {
        class: "hint",
        text: t("Turn on “Talk to Mochi” in Shortcuts to use the microphone (Ctrl+Alt+V)."),
      }),
    );
    if (!status) return;
    body.append(h("div", {
      class: inUse() ? "notice ok" : "hint",
      text: inUse()
        ? t("Model installed. Captures are transcribed on this computer.")
        : t("Without the model, the voice card shows a simulated transcript."),
    }));
    for (const model of status.models) body.append(row(model));
    if (error) body.append(h("div", { class: "notice err", text: t("Could not download: {error}", { error }) }));
  }

  async function run(step: () => Promise<VoiceModelStatus>) {
    error = null;
    try {
      status = await step();
    } catch (err) {
      error = String(err).replace(/^Error:\s*/, "");
      status = (await Bridge.voiceModelStatus()) ?? status;
    }
    draw();
  }

  async function download(id: string) {
    if (status) status = { ...status, downloading: id, received: 0 };
    draw();
    await run(() => Bridge.voiceModelDownload(id));
  }

  void onEvent<VoiceModelStatus>("voice-model", (next) => {
    status = next;
    draw();
  });
  draw();
  return section;
}

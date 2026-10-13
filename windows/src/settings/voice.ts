// Settings → Voice: the speech model for the assistant's voice capture.
//
// Speech becomes text on this computer with whisper.cpp. Its model is the one
// thing that has to be downloaded, and only from this section, by a click,
// from the address shown. Rust checks it against a pinned size and SHA-256
// before using it (src-tauri/src/assistant/voice/model.rs).

import { Bridge, onEvent, type VoiceModelStatus } from "../core/bridge";
import { h, clear } from "../views/dom";
import { t } from "../i18n/i18n";
import { statusDot } from "./parts";

function megabytes(bytes: number): string {
  return `${Math.round(bytes / 1_000_000)} MB`;
}

export function voiceSection(initial: VoiceModelStatus | null): HTMLElement | null {
  if (!initial?.supported) return null;
  let status = initial;
  let error: string | null = null;
  const head = h("h2");
  const body = h("div", { style: "display:flex;flex-direction:column;gap:12px" });
  const section = h("section", {}, head, body);

  function draw() {
    clear(head);
    head.append(statusDot(status?.installed ?? false), h("span", { text: t("Voice") }));
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
    body.append(
      h("div", { class: "row" },
        h("label", { text: t("Model") }),
        h("span", { class: "path", text: `ggml-base.bin · ${megabytes(status.size)}` }),
        statusDot(status.installed),
      ),
      h("div", { class: "row" },
        h("label", { text: "URL" }),
        h("span", { class: "path", text: status.source }),
      ),
    );

    if (status.downloading) {
      const percent = status.size ? Math.floor((status.received / status.size) * 100) : 0;
      body.append(
        h("div", { class: "row" },
          h("span", { text: t("Downloading… {percent}%", { percent }) }),
          h("button", { onclick: () => void Bridge.voiceModelCancel() }, t("Cancel")),
        ),
      );
    } else if (status.installed) {
      body.append(
        h("div", { class: "notice ok", text: t("Model installed. Captures are transcribed on this computer.") }),
        h("div", { class: "row" },
          h("button", { class: "danger", onclick: () => void remove() }, t("Delete the model")),
        ),
      );
    } else {
      body.append(
        h("div", { class: "hint", text: t("Without the model, the voice card shows a simulated transcript.") }),
        h("div", { class: "row" },
          h("button", { class: "primary", onclick: () => void download() },
            t("Download the model ({size})", { size: megabytes(status.size) })),
        ),
      );
    }
    if (error) body.append(h("div", { class: "notice err", text: t("Could not download: {error}", { error }) }));
  }

  async function download() {
    error = null;
    if (status) status = { ...status, downloading: true, received: 0 };
    draw();
    try {
      status = await Bridge.voiceModelDownload();
    } catch (err) {
      error = String(err).replace(/^Error:\s*/, "");
      status = (await Bridge.voiceModelStatus()) ?? status;
    }
    draw();
  }

  async function remove() {
    error = null;
    try {
      status = await Bridge.voiceModelDelete();
    } catch (err) {
      error = String(err).replace(/^Error:\s*/, "");
    }
    draw();
  }

  void onEvent<VoiceModelStatus>("voice-model", (next) => {
    status = next;
    draw();
  });
  draw();
  return section;
}

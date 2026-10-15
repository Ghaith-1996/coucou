// The region card: the picture the user picked, where it will go, and a way
// to black out parts of it before it does. Dragging on the picture adds a
// black box; clicking one removes it.

import { h } from "../views/dom";
import { btn, card, stack, type ViewHost } from "../views/views";
import { State } from "../core/state";
import { providerDef, activeModel } from "../core/providers";
import { t, tl } from "../i18n/i18n";
import { Region, addMask, onRegionReset, regionAsk, regionCancel, removeMask } from "./region";

/** The model the chat will send it to, as the picker names it. */
function destination(): string {
  const provider = providerDef(State.settings.chatProvider);
  const model = activeModel(State.settings);
  const name = t(provider.name);
  return model ? `${name} · ${model}` : name;
}

export function buildRegion(): ViewHost {
  const title = h("div", { class: "title", text: t("Ask about this region") });
  const img = h("img", { class: "region-img", alt: "", draggable: "false" }) as HTMLImageElement;
  const masks = h("div", { class: "region-masks" });
  const draft = h("div", { class: "region-mask draft" });
  draft.hidden = true;
  const frame = h("div", { class: "region-frame" }, img, masks, draft);
  const detail = h("div", { class: "sub region-sub" });
  const ask = btn(tl("Ask about it"), "primary", () => void regionAsk(), "↵");
  const cancel = btn(tl("Cancel"), "secondary", regionCancel, "Esc");
  const actions = h("div", { class: "actions" }, ask, cancel);
  const box = card("soft", stack(18, 16, title, frame, detail, actions));
  const el = h("div", { class: "view region" }, box);

  let shownImage = "";
  let shownMasks = "";

  /** Frame (CSS) pixels → picture (physical) pixels. */
  function toPicture(x: number, y: number) {
    const r = img.getBoundingClientRect();
    const p = Region.preview!;
    return {
      x: ((x - r.left) / Math.max(1, r.width)) * p.width,
      y: ((y - r.top) / Math.max(1, r.height)) * p.height,
    };
  }

  let start: { x: number; y: number } | null = null;
  onRegionReset(() => {
    shownImage = "";
    shownMasks = "";
    img.removeAttribute("src");
    masks.replaceChildren();
    start = null;
    draft.hidden = true;
  });
  frame.addEventListener("pointerdown", (e) => {
    if (Region.status !== "preview" || e.button !== 0) return;
    if ((e.target as HTMLElement).classList.contains("region-mask")) return;
    start = { x: e.clientX, y: e.clientY };
    frame.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  frame.addEventListener("pointermove", (e) => {
    if (!start) return;
    const f = frame.getBoundingClientRect();
    Object.assign(draft.style, {
      left: `${Math.min(start.x, e.clientX) - f.left}px`,
      top: `${Math.min(start.y, e.clientY) - f.top}px`,
      width: `${Math.abs(e.clientX - start.x)}px`,
      height: `${Math.abs(e.clientY - start.y)}px`,
    });
    draft.hidden = false;
  });
  frame.addEventListener("pointerup", (e) => {
    if (!start || !Region.preview) return;
    const a = toPicture(start.x, start.y);
    const b = toPicture(e.clientX, e.clientY);
    start = null;
    draft.hidden = true;
    addMask({ x: Math.min(a.x, b.x), y: Math.min(a.y, b.y), width: Math.abs(a.x - b.x), height: Math.abs(a.y - b.y) });
  });

  window.addEventListener("keydown", (e) => {
    if (State.view !== "region" || State.mode !== "expanded" || Region.status !== "preview") return;
    if (e.key === "Enter") {
      e.preventDefault();
      void regionAsk();
    } else if (e.key === "Escape") {
      e.preventDefault();
      regionCancel();
    }
  });

  return {
    el,
    sync() {
      const p = Region.preview;
      if ((p?.image ?? "") !== shownImage) {
        shownImage = p?.image ?? "";
        if (shownImage) img.setAttribute("src", shownImage);
        else img.removeAttribute("src");
      }
      const key = JSON.stringify(Region.masks);
      if (key !== shownMasks && p) {
        shownMasks = key;
        masks.replaceChildren(...Region.masks.map((m, i) => {
          const box = h("button", {
            class: "region-mask",
            title: t("Remove this black box"),
            "aria-label": t("Remove this black box"),
            onclick: () => removeMask(i),
          });
          Object.assign(box.style, {
            left: `${(m.x / p.width) * 100}%`,
            top: `${(m.y / p.height) * 100}%`,
            width: `${(m.width / p.width) * 100}%`,
            height: `${(m.height / p.height) * 100}%`,
          });
          return box;
        }));
      }
      detail.textContent = t(
        "Drag over anything private to black it out. Nothing is sent until you ask your question, to {model}.",
        { model: destination() },
      );
      (ask as HTMLButtonElement).disabled = Region.status !== "preview";
      (cancel as HTMLButtonElement).disabled = Region.status === "attaching";
    },
  };
}

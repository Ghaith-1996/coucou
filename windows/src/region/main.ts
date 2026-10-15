// A region overlay (region.html): one per monitor, dimmed, where the user
// drags around what they want to ask about. Letting go sends the rectangle,
// in this monitor's physical pixels, to Rust, which closes every overlay and
// copies that rectangle once (assistant/region). Escape or a right click
// cancels on every monitor.

import { Bridge } from "../core/bridge";
import { applyDocumentLanguage, resolveLanguage, setLanguage, systemLanguages, t } from "../i18n/i18n";
import { between, isRegion, physical, type Point } from "./select";

const dim = document.getElementById("dim")!;
const sel = document.getElementById("sel")!;
const size = document.getElementById("size")!;
const hint = document.getElementById("hint")!;

let session: string | null = null;
let start: Point | null = null;
let done = false;

function cancel() {
  if (done || !session) return;
  done = true;
  void Bridge.regionCancel(session);
}

function draw(a: Point, b: Point) {
  const css = between(a, b);
  const real = physical(css, window.devicePixelRatio);
  dim.classList.add("selecting");
  Object.assign(sel.style, {
    display: "block",
    left: `${css.x}px`,
    top: `${css.y}px`,
    width: `${css.width}px`,
    height: `${css.height}px`,
  });
  size.textContent = `${real.width} × ${real.height}`;
}

function reset() {
  start = null;
  sel.style.display = "none";
  dim.classList.remove("selecting");
}

window.addEventListener("pointerdown", (e) => {
  if (done) return;
  if (e.button === 2) {
    cancel();
    return;
  }
  if (e.button !== 0) return;
  start = { x: e.clientX, y: e.clientY };
  (e.target as Element).setPointerCapture?.(e.pointerId);
});

window.addEventListener("pointermove", (e) => {
  if (start && !done) draw(start, { x: e.clientX, y: e.clientY });
});

window.addEventListener("pointerup", (e) => {
  if (!start || done || !session) return;
  const rect = physical(between(start, { x: e.clientX, y: e.clientY }), window.devicePixelRatio);
  if (!isRegion(rect)) {
    // A click: try again.
    reset();
    return;
  }
  done = true;
  hint.hidden = true;
  void Bridge.regionPick(session, rect).catch(() => {
    done = false;
    reset();
  });
});

window.addEventListener("contextmenu", (e) => e.preventDefault());
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.preventDefault();
    cancel();
  }
});

async function main() {
  const info = await Bridge.regionOverlayInfo();
  if (!info) {
    // A stale overlay: nothing to pick for.
    hint.hidden = true;
    return;
  }
  session = info.sessionId;
  setLanguage(resolveLanguage(info.language, systemLanguages()));
  applyDocumentLanguage();
  hint.textContent = t("Drag around what you want to ask about. Esc to cancel.");
  window.focus();
}

void main();

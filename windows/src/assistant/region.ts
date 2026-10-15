// "Ask about a screen region" on the island: the chat's button starts a pick
// (Rust puts an overlay on every monitor), the picked picture comes back as a
// `region-preview` with Rust's grant, and the region card (region-view.ts)
// shows it with where it will go. The user may black out parts of it. "Ask
// about it" hands the grant and the masks back; Rust saves the masked PNG
// into the inbox and the chat opens with it attached, to be sent with the
// first question. Cancel before confirmation drops it. Nothing is captured before the user lets
// go of a rectangle, and nothing leaves the PC before they press Send.

import { Bridge, onEvent } from "../core/bridge";
import type { IslandViewName } from "../core/layout";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import type { RegionEnded, RegionPreview, RegionRect } from "./contracts";

/** What the region card needs from the island. */
export interface RegionHost {
  alert(view: IslandViewName): void;
  collapse(): void;
  takeKeyboard(): void;
  setPinned(on: boolean): void;
  /** Opens a fresh chat with this file attached, unsent. */
  chatWithFile(file: { name: string; path: string }): void;
}

/** At most this many masks (region.rs MAX_MASKS). */
export const MAX_MASKS = 64;

let host: RegionHost | null = null;
let pinnedByRegion = false;
let userPinned = false;
/** The card was seen on screen for the current picture. */
let shown = false;
/** Invalidate pending native starts when this flow is canceled or replaced. */
let generation = 0;
let startup: { previews: Map<string, RegionPreview>; endings: Map<string, RegionEnded> } | null = null;
const resetListeners = new Set<() => void>();

/** Permanent views must release pictures even when the island stops syncing them. */
export function onRegionReset(clear: () => void) {
  resetListeners.add(clear);
}

export const Region = {
  /** "picking" while the overlays are up, "preview" while the card shows. */
  status: "idle" as "idle" | "starting" | "picking" | "preview" | "attaching",
  session: null as string | null,
  preview: null as RegionPreview | null,
  /** Picture pixels (physical), in the order they were drawn. */
  masks: [] as RegionRect[],
};

/** The chat's button: the overlays come up on every monitor. */
export async function startRegion() {
  if (State.pendingApproval || (Region.status !== "idle" && Region.status !== "preview")) return;
  if (Region.session) void Bridge.regionCancel(Region.session);
  end();
  const own = generation;
  const pending = { previews: new Map<string, RegionPreview>(), endings: new Map<string, RegionEnded>() };
  startup = pending;
  Region.status = "starting";
  State.notify();
  try {
    const session = await Bridge.regionStart();
    if (generation !== own) {
      void Bridge.regionCancel(session);
      return;
    }
    startup = null;
    Region.session = session;
    Region.status = "picking";
    for (const id of pending.previews.keys()) if (id !== session) void Bridge.regionCancel(id);
    const ending = pending.endings.get(session);
    const preview = pending.previews.get(session);
    if (ending) ended(ending);
    else if (preview) accept(preview);
  } catch {
    if (generation !== own) return;
    end();
    Sound.play("error");
  }
  State.notify();
}

/** A mask, clipped to the picture; tiny ones (a click) are ignored. */
export function addMask(mask: RegionRect): boolean {
  const p = Region.preview;
  if (Region.status !== "preview" || !p || Region.masks.length >= MAX_MASKS) return false;
  const x0 = Math.max(0, Math.round(mask.x));
  const y0 = Math.max(0, Math.round(mask.y));
  const x1 = Math.min(p.width, Math.round(mask.x + mask.width));
  const y1 = Math.min(p.height, Math.round(mask.y + mask.height));
  if (x1 - x0 < 2 || y1 - y0 < 2) return false;
  Region.masks.push({ x: x0, y: y0, width: x1 - x0, height: y1 - y0 });
  State.notify();
  return true;
}

export function removeMask(index: number) {
  if (Region.status !== "preview" || index < 0 || index >= Region.masks.length) return;
  Region.masks.splice(index, 1);
  State.notify();
}

/** "Ask about it": Rust checks the grant, masks the picture and saves it; the chat opens with it. */
export async function regionAsk() {
  const p = Region.preview;
  if (Region.status !== "preview" || !p || !host) return;
  Region.status = "attaching";
  State.notify();
  try {
    const { file } = await Bridge.regionAttach(p.sessionId, p.grant, Region.masks);
    if (Region.preview !== p) return;
    end();
    Sound.play("approve");
    host.chatWithFile({ name: file.name, path: file.path });
  } catch {
    if (Region.preview !== p) return;
    end();
    if (State.view === "region" && !State.pendingApproval) host.collapse();
    Sound.play("error");
  }
}

/** Cancel or Escape before confirmation: the picture is dropped. */
export function regionCancel() {
  // Confirmation commits the write. Do not pretend to cancel it while Rust
  // owns the pixels: its successful file must still reach the chat.
  if (Region.status === "attaching") return;
  const session = Region.session;
  if (session) void Bridge.regionCancel(session);
  const shown = Region.status === "preview";
  end();
  if (shown && State.view === "region" && !State.pendingApproval) host?.collapse();
}

function accept(preview: RegionPreview) {
  if (startup && Region.status === "starting") {
    startup.previews.set(preview.sessionId, preview);
    return;
  }
  // Only the pick this page started.
  if (State.pendingApproval || preview.sessionId !== Region.session || (Region.status !== "picking" && Region.status !== "starting")) {
    void Bridge.regionCancel(preview.sessionId);
    return;
  }
  Region.status = "preview";
  Region.preview = preview;
  Region.masks = [];
  if (!pinnedByRegion) {
    userPinned = State.isPinned;
    pinnedByRegion = true;
    host?.setPinned(true);
  }
  if (Region.preview !== preview) return;
  Sound.play("finish");
  host?.alert("region");
  host?.takeKeyboard();
  State.notify();
}

function ended(event: RegionEnded) {
  if (startup && Region.status === "starting") {
    startup.endings.set(event.sessionId, event);
    return;
  }
  if (event.sessionId !== Region.session || (Region.status !== "picking" && Region.status !== "starting")) return;
  end();
  if (event.reason === "failed") Sound.play("error");
}

function reset() {
  generation++;
  if (startup) {
    for (const id of startup.previews.keys()) void Bridge.regionCancel(id);
    startup.previews.clear();
    startup.endings.clear();
  }
  startup = null;
  shown = false;
  Region.status = "idle";
  Region.session = null;
  Region.preview = null;
  Region.masks = [];
  for (const clear of resetListeners) clear();
}

function end() {
  reset();
  if (pinnedByRegion) {
    pinnedByRegion = false;
    if (!userPinned && !State.pendingApproval) host?.setPinned(false);
  }
  State.notify();
}

/** An agent's card taking the island, or the card going away, drops an
 *  unconfirmed picture: it is only kept while the user is looking at it. */
function guard() {
  if (Region.status === "idle" || Region.status === "attaching") return;
  if (State.pendingApproval) {
    regionCancel();
    return;
  }
  if (Region.status !== "preview") return;
  const onScreen = State.view === "region" && State.mode === "expanded";
  if (onScreen) shown = true;
  else if (shown) regionCancel();
}

export function registerRegion(islandHost: RegionHost) {
  host = islandHost;
  void onEvent<RegionPreview>("region-preview", accept);
  void onEvent<RegionEnded>("region-ended", ended);
  State.subscribe(guard);
}

/** Tests only. */
export function resetRegionForTests() {
  reset();
  pinnedByRegion = false;
  userPinned = false;
  host = null;
}

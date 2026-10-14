// "Ask about a screen region" on the island: the chat's button starts a pick
// (Rust puts an overlay on every monitor), the picked picture comes back as a
// `region-preview` with Rust's grant, and the region card (region-view.ts)
// shows it with where it will go. The user may black out parts of it. "Ask
// about it" hands the grant and the masks back; Rust saves the masked PNG
// into the inbox and the chat opens with it attached, to be sent with the
// first question. Cancel drops it. Nothing is captured before the user lets
// go of a rectangle, and nothing leaves the PC before they press Send.

import { Bridge, onEvent } from "../core/bridge";
import type { IslandViewName } from "../core/layout";
import { Sound } from "../core/sound";
import { State } from "../core/state";
import type { RegionPreview, RegionRect } from "./contracts";

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
  if (Region.status !== "idle" && Region.status !== "preview") return;
  if (Region.session) void Bridge.regionCancel(Region.session);
  reset();
  Region.status = "starting";
  try {
    Region.session = await Bridge.regionStart();
    if (Region.status === "starting") Region.status = "picking";
  } catch {
    reset();
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
    host.collapse();
    Sound.play("error");
  }
}

/** Cancel or Escape on the card: the picture is dropped. */
export function regionCancel() {
  const session = Region.session;
  if (session) void Bridge.regionCancel(session);
  const shown = Region.status === "preview" || Region.status === "attaching";
  end();
  if (shown && State.view === "region") host?.collapse();
}

function accept(preview: RegionPreview) {
  // Only the pick this page started.
  if (preview.sessionId !== Region.session || (Region.status !== "picking" && Region.status !== "starting")) {
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
  Sound.play("finish");
  host?.alert("region");
  host?.takeKeyboard();
  State.notify();
}

function ended(how: string) {
  if (Region.status !== "picking" && Region.status !== "starting") return;
  end();
  if (how === "failed") Sound.play("error");
}

function reset() {
  shown = false;
  Region.status = "idle";
  Region.session = null;
  Region.preview = null;
  Region.masks = [];
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
  if (Region.status !== "preview") return;
  if (State.pendingApproval) {
    regionCancel();
    return;
  }
  const onScreen = State.view === "region" && State.mode === "expanded";
  if (onScreen) shown = true;
  else if (shown) regionCancel();
}

export function registerRegion(islandHost: RegionHost) {
  host = islandHost;
  void onEvent<RegionPreview>("region-preview", accept);
  void onEvent<string>("region-ended", ended);
  State.subscribe(guard);
}

/** Tests only. */
export function resetRegionForTests() {
  reset();
  pinnedByRegion = false;
  userPinned = false;
  host = null;
}

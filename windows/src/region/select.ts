// The rectangle the user drags on a region overlay, from CSS pixels to the
// monitor's physical pixels (region.rs). Each overlay covers one monitor, so
// its own devicePixelRatio is that monitor's scale.

export interface Point {
  x: number;
  y: number;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Matches region.rs MIN_SIDE: smaller is a click, not a region. */
export const MIN_SIDE = 8;

/** The rectangle between two corners, in CSS pixels, whichever way it was dragged. */
export function between(a: Point, b: Point): Rect {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    width: Math.abs(a.x - b.x),
    height: Math.abs(a.y - b.y),
  };
}

/** CSS pixels → physical pixels of the monitor, outward to whole pixels. */
export function physical(css: Rect, ratio: number): Rect {
  const r = ratio > 0 && Number.isFinite(ratio) ? ratio : 1;
  const x0 = Math.floor(css.x * r);
  const y0 = Math.floor(css.y * r);
  const x1 = Math.ceil((css.x + css.width) * r);
  const y1 = Math.ceil((css.y + css.height) * r);
  return { x: Math.max(0, x0), y: Math.max(0, y0), width: Math.max(0, x1 - Math.max(0, x0)), height: Math.max(0, y1 - Math.max(0, y0)) };
}

/** Big enough to be a region, in physical pixels. */
export function isRegion(rect: Rect): boolean {
  return rect.width >= MIN_SIDE && rect.height >= MIN_SIDE;
}

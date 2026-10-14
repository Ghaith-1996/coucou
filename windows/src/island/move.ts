// A captured Linux pointer gesture. The compositor never takes over the drag,
// so pointerup/cancel remain ours, including a release during the begin IPC.
export interface MovePoint { x: number; y: number }
export interface MoveInfo extends MovePoint { mode: "layer" | "window"; scale: number }
export interface MovePointer {
  clientX: number; clientY: number; screenX: number; screenY: number;
  /** Was this event measured after the layer surface expanded? */
  overlay: boolean;
}
interface MoveAdapter {
  begin(): Promise<MoveInfo | null>;
  move(point: MovePoint): Promise<unknown>;
  end(point: MovePoint | null): Promise<unknown>;
  paint(point: MovePoint | null, info: MoveInfo | null): void;
}
interface Gesture {
  press: MovePointer;
  latest: MovePointer;
  info: MoveInfo | null;
  ended: boolean;
  cancelled: boolean;
  sent: MovePoint | null;
  flushing: boolean;
}
export class IslandMove {
  private gesture: Gesture | null = null;
  private adapter: MoveAdapter;
  constructor(adapter: MoveAdapter) { this.adapter = adapter; }
  get active() { return this.gesture != null; }

  start(press: MovePointer, latest: MovePointer): boolean {
    if (this.gesture) return false;
    const g: Gesture = { press, latest, info: null, ended: false, cancelled: false, sent: null, flushing: false };
    this.gesture = g;
    void this.adapter.begin().then(info => {
      if (!info) { this.clear(g); return; }
      g.info = info;
      this.paint();
      void this.flush(g);
    }).catch(async () => {
      try { await this.adapter.end(null); } catch { /* The window may have closed. */ }
      this.clear(g);
    });
    return true;
  }

  update(sample: MovePointer) {
    const g = this.gesture;
    if (!g || g.ended) return;
    g.latest = sample;
    this.paint();
    void this.flush(g);
  }

  finish(sample: MovePointer) {
    const g = this.gesture;
    if (!g || g.ended) return;
    g.latest = sample;
    g.ended = true;
    void this.flush(g);
  }

  cancel() {
    const g = this.gesture;
    if (!g || g.ended) return;
    g.ended = g.cancelled = true;
    void this.flush(g);
  }

  /** Resize may apply the full-screen surface after the begin reply. */
  paint() {
    const g = this.gesture;
    if (g?.info?.mode === "layer") this.adapter.paint(this.position(g), g.info);
  }

  private position(g: Gesture): MovePoint {
    const info = g.info!;
    const e = g.latest, p = g.press;
    if (info.mode === "window") return {
      x: info.x + (e.screenX - p.screenX) * info.scale,
      y: info.y + (e.screenY - p.screenY) * info.scale,
    };
    return {
      x: (e.overlay ? 0 : info.x) + e.clientX - p.clientX,
      y: (e.overlay ? 0 : info.y) + e.clientY - p.clientY,
    };
  }

  // At most one move IPC in flight. Release waits for it, then sends the final
  // point, so an older move cannot undo a drop or the following drag.
  private async flush(g: Gesture) {
    if (!g.info || g.flushing) return;
    g.flushing = true;
    try {
      while (this.gesture === g) {
        const at = this.position(g);
        if (g.ended) {
          await this.adapter.end(g.cancelled ? null : at);
          this.clear(g);
          return;
        }
        if (g.info.mode === "layer" || (g.sent?.x === at.x && g.sent.y === at.y)) return;
        g.sent = at;
        await this.adapter.move(at);
      }
    } catch {
      // Failure restores the original bounded surface, never saves a partial move.
      try { await this.adapter.end(null); } catch { /* The window may have closed. */ }
      this.clear(g);
    } finally { g.flushing = false; }
  }

  private clear(g: Gesture) {
    if (this.gesture !== g) return;
    this.gesture = null;
    this.adapter.paint(null, null);
  }
}

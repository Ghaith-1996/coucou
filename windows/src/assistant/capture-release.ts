// Cancellation acceptance is not microphone release. Keep a capture-scoped
// reservation until native acknowledgement or its terminal event, even when
// transport fails. A later user retry can reconcile a lost terminal event.
import { Bridge } from "../core/bridge";
import { State } from "../core/state";
import type { VoiceEvent } from "./contracts";

class CaptureRelease {
  private pending: { id: string; running: boolean; failed: boolean } | null = null;
  get busy() { return this.pending !== null; }
  get failed() { return this.pending?.failed === true; }

  begin(id: string, event?: VoiceEvent) {
    if (event?.state === "idle" || event?.state === "error") return;
    if (this.pending?.id !== id) this.pending = { id, running: false, failed: false };
    void this.retry();
  }

  async retry() {
    const pending = this.pending;
    if (!pending || pending.running) return;
    pending.running = true;
    const wasFailed = pending.failed;
    pending.failed = false;
    if (wasFailed) State.notify();
    try {
      for (let attempt = 0; attempt < 3 && this.pending === pending; attempt++) {
        try {
          const released = await Bridge.voiceCancel(pending.id);
          if (this.pending !== pending) return;
          if (released === true) {
            this.pending = null;
            State.notify();
          }
          // false only accepts cancellation: the native listening thread must
          // still close its microphone. An explicit Retry can reconcile later.
          return;
        } catch {
          if (this.pending !== pending) return;
        }
      }
      if (this.pending === pending) pending.failed = true;
    } finally {
      if (this.pending === pending) {
        pending.running = false;
        if (pending.failed) State.notify();
      }
    }
  }

  accept(event: VoiceEvent): boolean {
    if (event.captureId !== this.pending?.id) return false;
    if (event.state === "idle" || event.state === "error") {
      this.pending = null;
      State.notify();
    }
    return true;
  }

  reset() { this.pending = null; }
}

export const voiceRelease = new CaptureRelease();
export const audioRelease = new CaptureRelease();
export const captureReleasePending = () => voiceRelease.busy || audioRelease.busy;
export const captureReleaseFailed = () => voiceRelease.failed || audioRelease.failed;
/** True means this action retried release, and must not start another capture yet. */
export function retryCaptureRelease(onlyFailed = false): boolean {
  if (!captureReleasePending()) return false;
  if (!onlyFailed || voiceRelease.failed) void voiceRelease.retry();
  if (!onlyFailed || audioRelease.failed) void audioRelease.retry();
  return true;
}

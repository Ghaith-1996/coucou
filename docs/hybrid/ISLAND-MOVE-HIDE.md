# Island: move it anywhere, hide it

Asked by the owner: the island no longer has to stay at the top of the screen,
and a button hides it.

## Move

- Press on the compact island, or on the open island's header (anywhere that
  isn't a button), and drag. Past the usual drag threshold, Windows' own move
  loop takes the window (`start_dragging`). Mochi is not a handle: dragging
  him still puts him on the desktop.
- When the button comes up, the cursor poll (`island.rs`, `finish_move`)
  saves where it landed as `settings.islandSpot`: logical pixels from that
  display's top-left. The display it was dropped on becomes the island's
  display.
- The open island must fit below the pill, so the spot is kept whole on its
  display (the 720×320 panel never leaves it).
- **Dropped within ~80 px of the top centre, it docks there again**, as a
  notch. The tray's **Put the island back at the top** does the same.
- Away from the top, the island has round corners all over. When idle it stays
  a small pill instead of retracting into the top edge, since there is no edge
  to find it at.
- `islandSpot` belongs to Rust. Saving settings from a webview keeps the stored
  value, as for the desktop Mochi's spot.

## Linux movement

Linux uses the same handles, saved `islandSpot`, clamping and docking zone, but
keeps the pointer gesture in the page rather than handing it to a native move
loop. Pointer capture supplies an explicit release through
`island_drag_begin` / `island_drag_move` / `island_drag_end`; the Linux display
poll never guesses that a drag ended from its unavailable global button state.

- **X11 / XWayland:** pointer desktop coordinates drive the window position.
  Release saves the final point and selects its display. Outstanding move calls
  complete before the final drop, so a late move cannot undo it.
- **Wayland with gtk-layer-shell:** the island is anchored top-left and placed
  with monitor-relative logical margins, including when floating or collapsed.
  During a drag its surface temporarily covers the selected output; the page
  moves the normal-size island inside that stationary surface. Release folds
  the surface back and saves/docks on that same output. Dragging to a different
  output is not supported in this mode; choose it in Settings. The selected
  output remains authoritative on mixed-DPI layouts, where GDK's reported
  physical monitor rectangles can overlap.
- **Native Wayland without layer-shell:** absolute saved placement is not
  supported, so no unfinishable native drag is started. Coucou's existing
  GNOME default prefers XWayland when available; an explicit opt-out keeps this
  limitation. No global pointer hook, dependency or security-setting change is
  introduced.

A release received before the begin reply is retained and completed once.
Pointer cancellation, lost capture, Escape delivered to the island, or a page
blur cancel without saving. Hide, collapse, a display preference change, docking
from the tray and page reload also restore the bounded window and input region.
Invalid coordinates are rejected before integer conversion. Windows retains
its existing native-drag/button-poll path.

## Hide

- **–** in the island's header: the window leaves the screen (hidden, cursor
  poll parked, nothing to hover).
- It comes back with the tray's **Open Coucou** (or any tray item that opens
  the island), the **Toggle the island** shortcut if it is enabled
  (Ctrl+Alt+N, off by default), or **any alert**.
- **Safety:** an agent's permission request is an alert, so it brings the
  island back. It is never left waiting unseen behind the Hide button. The
  button isn't offered while a card waits, and hiding never answers anything.
- Not kept across restarts: Coucou starts visible.

## Verification

| Check | Result |
|---|---|
| Rust tests | placement (`panel_origin`: top centre, scaling, clamping on every edge, a display left of the primary) and drop (`spot_after_move`: saved as shown, docking near the top centre, not in a top corner, a 200 % display) |
| Settings tests | `islandSpot` round-trips; a malformed one falls back to the top centre |
| Live, release GPU build | Hide button (UI Automation) → window gone (screenshot) |
| Live | Island hidden → a permission request through the real `coucou-hook.exe` → the island comes back with the card, without the Hide button → **Deny** → the relay printed `deny` |
| Live | The owner dragged the island; `islandSpot` saved (630, 105); the open island floats there with round corners (screenshot) |
| Not verified | Dragging across two displays with different scales; docking back by dragging (unit-tested only) |

### Linux correction checks (2026-10-13)

- Passed: all 454 frontend tests, including eight captured-drag lifecycle tests
  (normal and early release, cancellation, failed begin/move, repeated gestures,
  X11 coordinates and in-flight move ordering), TypeScript checking, Vite build,
  and whitespace/diff checks
- Added for native CI: layer logical-coordinate/scaling/clamping/docking cases,
  extreme-coordinate rejection, and the guard against an untracked Linux native
  drag; existing Windows drag-release and persistence geometry tests remain
- Not run in the editing environment: Rust compilation/tests (no Rust toolchain),
  real GTK/WebKit pointer capture, layer-shell resize/remap behavior, X11
  multi-display dragging, restart persistence or live hide/cancel recovery
- The earlier live verification above was Windows verification; it is not
  evidence of a live Linux run

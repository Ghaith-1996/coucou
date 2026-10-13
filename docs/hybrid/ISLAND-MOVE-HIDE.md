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

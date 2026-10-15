# Hybrid assistant — P2: ask about a screen region (`ask-region`)

"Can you see what I highlighted?" The user picks a part of the screen, sees
exactly what will be sent and where, can black out parts of it, and asks
the chat about it. Nothing is captured before they let go of a rectangle,
and nothing leaves the PC before they press Send.

## Flow

1. **Start**: the viewfinder button next to the chat's microphone
   (`region_start`, from the island only). Rust lists the monitors and opens
   one overlay window per monitor (`region-0`, `region-1`…, `region.html`),
   placed at the monitor's physical rectangle. Each page dims its monitor and
   says "Drag around what you want to ask about. Esc to cancel."
   - One overlay per monitor keeps mixed DPI simple: a page lives on one
     monitor and converts its CSS pixels with its own `devicePixelRatio`.
   - The overlays are transparent and dimmed; they show the live screen, not
     a screenshot.
2. **Pick** (`region_pick`, from an overlay only). The user drags a
   rectangle; letting go sends it in that monitor's physical pixels. A click
   (under 8 px) is ignored, and the user can try again. Escape or a right
   click cancels on every monitor (`region_cancel`).
   - Rust clamps the rectangle to the monitor, closes every overlay, keeps
     Coucou's own windows (island, desktop Mochi) out of the capture with
     `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`, waits 220 ms for the
     overlays to leave the screen, and copies **that rectangle once** with
     GDI (`BitBlt`, `CAPTUREBLT` so menus and tooltips are included).
   - The picture stays in memory. Rust keeps the `ask-region` action
     (`{ monitor, x, y, width, height }`; the policy says **ask**) and sends
     the island a preview (PNG, at most 720 px) with the grant.
3. **Confirm** (the region card). The island shows the picture, its
   destination ("Nothing is sent until you ask your question, to <provider ·
   model>"), **Ask about it** (Enter) and **Cancel** (Esc).
   - Dragging on the picture adds a black box; clicking one removes it
     (at most 64).
   - The picture is dropped if the user cancels, leaves the card, or an
     agent's permission card takes the island.
4. **Attach** (`region_attach`, from the island only). Rust checks the grant
   against the action it kept (`policy::check_grant`). The picture is used
   once, whatever the answer. Rust blacks out the masks on the full-size
   picture, saves it as `Screen region <date> <time>.png` in Coucou's inbox
   (`files::store`, never overwriting) and returns it.
   - Receipt `succeeded`, evidence `inbox:png`. The log has the action id,
     the size and the number of masks, never the content.
5. **Ask**: the chat opens fresh with the picture attached, exactly like a
   dropped image. It is sent with the first question to the provider in the
   picker, and the inbox sweep deletes it after a week.

## Contract change

`ask-region` was reserved; it is now available.
- `AskRegionArgs` loses `question`: the question is typed in the chat
  afterwards, and the action is only the picture.
- `x` and `y` must be non-negative (they are relative to the monitor), and
  the monitor name may not contain control characters.
- The registry is shared with the TS side. The shared fixture covers the
  `ask` decision, a region that carries a question, and an empty or
  off-screen region.

## Verification

- Rust: clamping, masks (clipped), shrinking (averaged), the PNG, the file
  name, and the session (one picture per pick, used once, forged grant,
  stale session, cancel, and the action's exact arguments). All 551 app
  tests and 30 hook tests pass.
- Live, opt-in (`--ignored`): a real 64 × 48 rectangle of the screen is
  copied.
- JS:
  - the overlay's rectangle (direction, scale rounding outward, clicks);
  - the island's flow (only its own pick, masks clipped, attach only on
    the click, cancel, leaving the card, agent card, refused attach,
    overlays ending);
  - `npm test` passes, except the 5 known `cmd.exe` tests that fail only
    on the dev machine.

Not verified yet: the whole flow by hand in the running app, on one and
on two monitors with different scales.

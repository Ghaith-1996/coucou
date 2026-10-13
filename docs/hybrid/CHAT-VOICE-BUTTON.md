# Chat microphone button (Windows)

The chat composer now has a microphone button immediately before its Send
arrow. Clicking it starts the existing voice capture flow even when the
`voiceCapture` keyboard shortcut is disabled. The shortcut remains optional.

The button calls the same `voiceShortcut` controller as the keyboard shortcut:
listening with a level meter, Stop, a transcript preview, then Use in chat,
confirm a supported command, or Cancel. A chat draft is never sent automatically.
An agent's waiting permission or question takes priority, without being answered
by voice. The button is disabled while the chat is waiting for a model reply.

The control has a translated tooltip and accessible name, keyboard activation
and a visible focus ring. The existing composer layout and Send button are
preserved. Settings → Voice now explains both ways to start a capture.

## Verification (2026-10-13)

- Regression tests failed before the button existed, then passed: button to
  voice controller to unsent draft, waiting-agent priority, and capture blocked
  during a pending chat reply.
- `npm test`: 853 passed. `npx tsc --noEmit`: passed.
- `npx vite build`: passed.
- `scripts\cargo-vs.cmd test --workspace --locked`, from `windows/`: 502 app
  tests and 30 relay tests passed; 7 opt-in app tests ignored.
- Headless Edge/Playwright at `http://127.0.0.1:1421`: the real chat and voice
  views rendered at 640 px and 560 px card widths without overflow, a blank
  page, an error overlay or console errors. Keyboard activation, Stop →
  preview → Use in chat and waiting-agent priority passed. Native microphone
  and provider IPC were simulated; no audio was recorded or chat sent.

The browser harness and screenshots are local QA artifacts under
`D:\cv\voice-button-qa`, outside the repository. A live microphone capture
through this button is not part of these checks.

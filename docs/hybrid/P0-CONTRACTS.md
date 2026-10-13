# Hybrid assistant — contracts v1 (P0)

These are the shapes the desktop assistant is built on. They are fixed before any
feature uses them. Rust is the authority (`windows/src-tauri/src/assistant/`) and
TypeScript mirrors it (`windows/src/assistant/contracts.ts`). Both test suites read
`windows/tests/fixtures/assistant-contracts.json`, so the two sides can't drift.

## Two domains, never converted

| | AgentPermission (existing) | AssistantAction (new) |
|---|---|---|
| Comes from | a coding agent's hook, over the user's pipe | a voice capture, later routines/dictation |
| Answered by | a human click on the agent's card only | the policy below, plus user confirmation for `ask` |
| No answer / timeout | terminal takes over, never an approval | refused / `cancelled`, never run late |
| Code | `pipe.rs`, `approval_*` commands | `assistant/`, `src/assistant/` |

Enforced by:
- `domain: "desktop-assistant"` is a one-value enum, so any other domain fails to parse.
- Both suites scan the assistant's sources and fail if they mention `pipe::`, `Pending`, `Reply::`, `approval_*`, `approvalDecision`/`Answer`/`Ack`/`Decline` or `PermissionRequest`. A planted violation was checked to fail each test.

## AssistantAction

`{ v, domain, id, kind, args, intentId, expiresAtMs }`. Unknown top-level fields fail
to parse.

| kind | status | args (closed schema, unknown fields refused) | decision |
|---|---|---|---|
| `chat-draft` | **available** | `{ text }` 1–4000 chars, no control chars except line breaks/tabs | `allow`: it stays in Coucou's chat field and is never sent |
| `insert-text` | reserved (P2) | `{ text, targetWindow, targetPid }` | `ask` |
| `launch-app` | reserved (P2) | `{ appId }` `[a-z0-9._-]{1,64}`, an id from the user's list | `ask` |
| `ask-region` | reserved (P2) | `{ question, monitor, x, y, width, height }` | `ask` |

Policy order: version → expiry (and at most 5 min ahead) → availability → args.
Anything that fails is `deny` with an `ErrorCode`. Reserved kinds are always denied,
and their args may still change before they become available.

**Deviation from the audit's sketch:** `expiresAt` is `expiresAtMs` (Unix ms), not
an ISO string. Same meaning, no date parsing on either side.

## Grants: a confirmation bound to one action

For `ask`, the user confirms the exact action shown. The grant holds the action id,
the expiry and a SHA-256 over domain/version/id/expiry/**normalized** args. Normalized
means the checked args re-serialized, so key order doesn't matter. `check_grant`
re-runs the whole policy at execution time and recomputes the digest, so a changed
text, target, id or expiry, or an action that expired while the user was reading,
is refused (`grant-mismatch` / `expired`). The grant is a binding, not a secret:
Rust holds the pending actions itself.

## Receipts

`{ actionId, outcome: succeeded|failed|cancelled|unknown, evidenceRef?, errorCode? }`.
`succeeded` can only be built with non-empty evidence. After `unknown`, an action is
retried only if its kind is idempotent (today only `chat-draft`).

## Voice

`assistant-voice` events: `{ v, captureId, state, level?, elapsedMs?, transcript?, simulated?, error? }`.
States: `idle, listening, processing, preview, speaking, error`. **`preview` was
added to the audit's list**: a transcript waits there for the user to use it or
cancel. `simulated: true` marks text from the simulated provider. The island drops
any event that isn't exactly this shape (`parseVoiceEvent`), including unknown fields.

## Verification

| Command | Result |
|---|---|
| `cargo test --workspace --locked` | app 423 pass (412 + 11 new), hook 30 pass, no warnings |
| `npx tsc --noEmit` | clean |
| `npm test` | 402 pass (397 + 5 new) |
| plant `approvalDecision` in `contracts.ts` / `pipe::` in `policy.rs` | each wall test fails as expected; reverted |

Status: unit-tested. Nothing in the running app uses these yet. The voice slice
(next PR) is the first consumer.

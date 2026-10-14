# Hybrid assistant — audio mode (talk with the chat)

A spoken conversation with the chat, hands-free, like ChatGPT's voice mode,
built from local parts. ChatGPT's own voice mode can't be used: Sign in with
ChatGPT for other apps does not support audio input or transcription
("Preview limitations" on developers.openai.com/siwc), and ChatGPT's private
backend is off limits.

## Loop

`listening → (you speak, then pause) → thinking → speaking → listening…`

- **Start / end**: the sound-wave button in the chat bar. **End**, Escape,
  leaving the chat, an agent's permission card, or 12 s without speech ends
  it ("Audio mode ended: nothing was heard."). The island stays pinned while
  it runs.
- **Listening** (`assistant_voice_hands_free`): a normal capture (the
  microphone is open only during it, 30 s at most, audio kept in memory)
  that ends by itself (`voice::Endpoint`, on 50 ms windows):
  - speech is a window at least 3× the room's noise and never under RMS
    0.012;
  - the noise is learned from the quiet windows, and taken for at most
    0.03, so a bad estimate can't hide speech;
  - at least 300 ms of speech, then 1.1 s of pause, ends the turn.
- **Sending** (`assistant_voice_converse`): the transcript goes to the chat
  as your message, without the preview card. Turning audio mode on is the
  choice to talk to the chat, and that is all it reaches: no pill command,
  no app, no field, no agent permission. It is not an assistant action. A
  simulated transcript is never sent.
- **Speaking** (`speech_say`, `assistant/speech.rs`): the answer is read
  aloud as it streams.
  - `speakable.ts` cuts it at sentence, list and paragraph boundaries (at
    least 24 readable characters, never inside an open code block) and
    turns markdown into plain words. Code and addresses are skipped; a link
    is read by its text.
  - Each chunk is synthesized by Windows' voices (WinRT `SpeechSynthesizer`,
    about 0.2 s a sentence) while the previous one plays.
  - The voice follows the reply's language (French or English, from common
    words): a voice of that language if Windows has one, the default one
    otherwise. Windows ships English voices; French voices come with
    Windows' French speech pack (Settings → Time & language → Speech).
- **Interrupting**: the microphone is closed while Coucou speaks, so it
  never hears itself. Tapping the status ("Speaking — tap to interrupt")
  stops the voice and listens again.

## Verification

- Rust (all 569 app tests and 30 hook tests pass):
  - end-of-speech: pause after speech, a cough, a noisy room, odd sample
    sizes;
  - a hands-free capture stops by itself, or ends in `silence`;
  - `converse` sends a real transcript once and never a simulated one;
  - French/English detection and voice choice.
- Live (opt-in): Windows reads a sentence into a WAV. Voices on this PC:
  David, Zira, Mark (en-US).
- JS:
  - `speakable` (markup, code, links, chunks while streaming, open code
    blocks, lists);
  - the loop (a turn read aloud sentence by sentence then listening again,
    silence, agent card, leaving the chat, interrupt, End, a failed answer,
    a refused transcript, other captures' events).

## OpenAI's voice when the sign-in allows it

When the chat is on ChatGPT, each chunk is first asked of OpenAI's voice
(`POST /v1/audio/speech`, `gpt-4o-mini-tts`, voice `marin`, MP3, with the
same access token as the chat; never an API key).
- Sign in with ChatGPT doesn't document speech out, so it is tried, not
  assumed.
- The first refusal (HTTP 400/401/403/404) is logged
  (`chatgpt: speech not available on this sign-in`) and not asked again
  until reconnecting or switching accounts. Other failures fall back for that
  chunk only.
- Windows' voices read whatever OpenAI's voice doesn't.
- The log says `speech: ChatGPT's voice in use` the first time it works.

If it is refused, the next step is Piper (local, free neural voices).

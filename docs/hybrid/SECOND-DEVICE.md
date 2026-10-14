# Running the hybrid build on another Windows PC

`hybrid/all` is an integration branch: `hybrid/main` with every open hybrid
PR merged in. These are #15 (ChatGPT chat), #16 (dictation), #17 (French and
English only), #18 (screen region), #19 (audio mode) and #20 (chat actions),
including the review fixes pushed to them up to 2026-10-14.

It is for trying everything together, on any PC. It is **not** merged
anywhere: `hybrid/main` only gets the PRs, one by one, after review. When
those PRs change, `hybrid/all` is rebuilt from them.

## Get it

```
git clone https://github.com/Ghaith-1996/coucou.git
cd coucou
git checkout hybrid/all
```

Or, in an existing clone: `git fetch origin` then `git checkout hybrid/all`
(the first time), and `git pull` afterwards.

## Build it (once per PC)

Needs:
- **Rust** (rustup, stable);
- **Node.js 20+**;
- **Visual Studio 2022** or its Build Tools, with "Desktop development with
  C++", "C++ CMake tools for Windows" and "C++ Clang Compiler for Windows";
- for the graphics-card build, the **Vulkan SDK**
  (`winget install KhronosGroup.VulkanSDK`).

```
cd windows
npm ci
scripts\build-exe.cmd gpu
```

The exe lands in `<drive>:\cv\release\coucou.exe`, on the same drive as the
checkout (a short folder, because the Vulkan build passes Windows' path
limit otherwise). Use `scripts\build-exe.cmd` without `gpu` for a CPU-only
build. Start it by double-clicking it.

## What the code does not bring

None of this is in git: it belongs to each PC or to the account.

| What | On the new PC |
|---|---|
| Speech models (Whisper) | Settings → Voice → download one again. large-v3-turbo is 547 MB, base 141 MB. Or copy `%LOCALAPPDATA%\Coucou\models` from the other PC |
| ChatGPT sign-in | Settings → ChatGPT → Continue with ChatGPT. The tokens stay in that PC's Windows Credential Manager and are never copied; each PC is its own sign-in |
| Settings, saved conversations, screen regions | Local to each PC (`%APPDATA%\Coucou`, `%LOCALAPPDATA%\Coucou`). Copy those folders to bring them over |
| French reading voice (audio mode, if OpenAI's voice isn't available) | Windows Settings → Time & language → Speech → add French voices |
| Claude Code hooks (agent cards) | Coucou installs its hook into `~/.claude/settings.json` on first run, as on any PC |

## One thing to avoid

Don't start `coucou.exe` from a terminal inside the Claude desktop app (or
from Claude Code running there). Windows then treats Coucou as part of that
app and saves its files in the app's private folder
(`%LOCALAPPDATA%\Packages\Claude_…\LocalCache`). A normal launch then can't
see them: speech models look missing ("Simulated transcript") and the
ChatGPT login looks gone. Double-click the exe, or start it from a normal
terminal.

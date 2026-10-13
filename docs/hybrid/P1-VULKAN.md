# Hybrid assistant — P1: Whisper on the GPU (Vulkan), opt-in

whisper.cpp's ggml Vulkan backend, behind a cargo feature that is **off by
default**:

```
windows\scripts\dev.cmd gpu         # development (needs its console open)
windows\scripts\build-exe.cmd gpu   # a coucou.exe that runs on its own
```

Both work from any terminal or by double-click: `scripts\vs-env.cmd` sets up Visual
Studio's environment, libclang, the Vulkan SDK and the short target folder
(`<drive>\cv`). Without `gpu` they build the default (CPU) app. A development
build's `coucou.exe` loads its pages from the Vite server that `dev.cmd` starts,
so on its own it shows "can't reach this page". Use `build-exe.cmd` for an exe
that runs alone.

With the feature, `WhisperContextParameters.use_gpu` is on. Without it, nothing
changes.

## Measured (this machine)

AMD Radeon RX 7800 XT (Vulkan, driver loader 1.4.363), Ryzen 5 7600. Release
build, the same six real-speech clips as P1-MODELS.md.

| Model | CPU (default build) | **Vulkan** | Errors (both builds identical) |
|---|---|---|---|
| base | ~1.1 s | **0.08–0.14 s** | "Versal" ×2 |
| large-v3-turbo q5_0 | ~22 s | **0.37–0.45 s** | none |
| large-v3 q5_0 | ~27 s | **0.48–0.77 s** | "Machi" ×1 |

The first clip of a run adds about 1 s for loading the model onto the GPU. On this
machine **large-v3-turbo q5_0 on Vulkan is both the most accurate and fast enough
for dictation** (~0.4 s after the capture ends).

## No GPU driver → CPU, no crash

`VK_LOADER_DRIVERS_DISABLE=*` (the Vulkan loader's own switch) hides every driver.
With it, the Vulkan build still transcribed correctly, at CPU speed (base: 1.24 s
instead of 0.09 s). ggml finds no Vulkan device and uses the CPU backend.

## What blocks turning it on by default

1. **The Vulkan loader must exist.** The binary imports `vulkan-1.dll`. GPU drivers
   (AMD, NVIDIA, Intel) install it, but a PC without one (a VM, a server, a machine
   without a GPU driver) would fail to *start* the app, not just fall back.
   Plan for P4 packaging: ship the Khronos loader (Vulkan-Loader, Apache-2.0, from
   LunarG's VulkanRT redistributable) next to `coucou.exe`. With a loader but no
   driver, the fallback above applies. To verify then: on a VM without a GPU driver.
2. **Building needs the Vulkan SDK** (`VULKAN_SDK`, `glslc`): CI and the release
   workflow would need it installed.
3. **Path length.** The shader generator's sub-build nests deep inside the target
   folder. From `windows\target` one path is 266 characters, past Windows'
   260-character limit (MSBuild FTK1011). Build with a short target folder
   (`set CARGO_TARGET_DIR=D:\cv`, which gives 241) or enable Windows long paths
   (a system setting, the owner's call). Default (CPU) builds are not affected.

## Setup done on this machine

- LunarG Vulkan SDK 1.4.363.0 via `winget install KhronosGroup.VulkanSDK`
  (it also updated the system's `vulkan-1.dll` to 1.4.363).
- GPU builds went to `D:\cv` (a throwaway build folder).

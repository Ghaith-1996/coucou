// Exercise the real npm lifecycle commands without compiling Rust or starting
// Tauri. The command shims only write fixture files inside a temporary folder.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const config = JSON.parse(readFileSync(join(root, "src-tauri/tauri.conf.json"), "utf8"));
const linuxConfig = JSON.parse(readFileSync(join(root, "src-tauri/tauri.linux.conf.json"), "utf8"));
const windows = process.platform === "win32";
const relay = windows ? "coucou-hook.exe" : "coucou-hook";
const script = (name) => readFileSync(join(root, "scripts", name), "utf8");
const hookSource = readFileSync(join(root, "src-tauri/src/hooks.rs"), "utf8");
// The Linux override nulls the Windows resource during Tauri's config merge.
const resourceMap = Object.fromEntries(Object.entries((windows ? config : linuxConfig).bundle.resources).filter(([, target]) => target !== null));

test("both relay lifecycle hooks explicitly target Tauri's resource directory", () => {
  for (const hook of ["predev", "prebuild"]) {
    assert.equal(pkg.scripts[hook], "cargo build --release -p coucou-hook --target-dir target");
  }
  assert.equal(config.build.beforeDevCommand, "npm run dev");
  assert.equal(config.build.beforeBuildCommand, "npm run build");
  assert.equal(config.bundle.resources["../target/release/coucou-hook.exe"], "coucou-hook.exe");
  assert.equal(linuxConfig.bundle.resources["../target/release/coucou-hook"], "coucou-hook");
});

test("the app build and runtime retain Tauri's mapped resource contract", () => {
  assert.match(readFileSync(join(root, "src-tauri/build.rs"), "utf8"), /tauri_build::build\(\)/);
  // These are the ordered candidates exercised by the runtime fixture below.
  assert.match(hookSource, /BaseDirectory::Resource/);
  assert.match(hookSource, /parent\.join\(platform::HOOK_EXE\)/);
  assert.match(hookSource, /parent\.join\("\.\.\/release"\)\.join\(platform::HOOK_EXE\)/);
  assert.ok(hookSource.indexOf("parent.join(platform::HOOK_EXE)") < hookSource.indexOf('parent.join("../release")'));
});

test("Vulkan SDK discovery strips the quotes from directory FOR variables", () => {
  assert.match(script("vs-env.cmd"), /for \/d %%d in \("%SystemDrive%\\VulkanSDK\\\*"\) do set "VULKAN_SDK=%%~fd"/);
});

test("GPU wrappers retain their short app target and Vulkan feature", () => {
  assert.match(script("vs-env.cmd"), /set "CARGO_TARGET_DIR=%~d0\\cv"/);
  for (const name of ["dev.cmd", "build-exe.cmd"]) {
    assert.match(script(name), /call "%~dp0vs-env\.cmd" %1 \|\| goto :failed/);
    assert.match(script(name), /--features gpu-vulkan/);
  }
  assert.match(script("build-exe.cmd"), /set "OUT=%CARGO_TARGET_DIR%"/);
});

function fixture(t) {
  const dir = mkdtempSync(join(tmpdir(), "coucou relay "));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const bin = join(dir, "node_modules", ".bin");
  mkdirSync(bin, { recursive: true });
  const resource = join(dir, "target", "release", relay);
  const appLog = join(dir, "app.json");
  const runtimeLog = join(dir, "runtime.json");
  const env = { ...process.env, PATH: `${bin}${windows ? ";" : ":"}${process.env.PATH}` };
  // Windows environment keys are case-insensitive; avoid passing two PATHs.
  for (const key of Object.keys(env)) {
    if (/^path$/i.test(key) && key !== "PATH") delete env[key];
    if (/^cargo_target_dir$/i.test(key)) delete env[key];
  }
  env.TEST_NODE = process.execPath;
  env.TEST_GPU_TARGET = join(dir, "cv");
  writeFileSync(join(dir, "package.json"), JSON.stringify({
    private: true,
    scripts: {
      predev: pkg.scripts.predev,
      prebuild: pkg.scripts.prebuild,
      dev: "node app.cjs",
      build: "node app.cjs",
      tauri: "node tauri.cjs",
    },
  }));
  writeFileSync(join(dir, "cargo.cjs"), `
    const fs = require('node:fs');
    const path = require('node:path');
    if (process.env.TEST_CARGO_FAIL) process.exit(42);
    const args = process.argv.slice(2);
    const i = args.indexOf('--target-dir');
    const target = i < 0 ? process.env.CARGO_TARGET_DIR || 'target' : args[i + 1];
    const out = path.resolve(target, 'release', ${JSON.stringify(relay)});
    fs.mkdirSync(path.dirname(out), { recursive: true });
    fs.writeFileSync(out, 'fresh relay');
  `);
  writeFileSync(join(dir, "app.cjs"), `
    const fs = require('node:fs');
    if (fs.readFileSync(${JSON.stringify(resource)}, 'utf8') !== 'fresh relay') process.exit(43);
    fs.writeFileSync(${JSON.stringify(appLog)}, JSON.stringify({ target: process.env.CARGO_TARGET_DIR || null }));
  `);
  // Model the Windows runtime's Resource/adjacent/../release search, without
  // consulting a developer's pre-existing LOCALAPPDATA relay. This is a contract
  // fixture, not a real Tauri app or a GPU build.
  writeFileSync(join(dir, "runtime.cjs"), `
    const fs = require('node:fs');
    const path = require('node:path');
    const parent = process.argv[2];
    const candidates = [
      path.join(parent, ${JSON.stringify(relay)}),
      path.join(parent, ${JSON.stringify(relay)}),
      path.join(parent, '../release', ${JSON.stringify(relay)}),
      path.join(parent, '_up_/target/release', ${JSON.stringify(relay)}),
    ];
    const source = candidates.find(p => fs.existsSync(p));
    if (!source || fs.readFileSync(source, 'utf8') !== 'fresh relay') process.exit(44);
    fs.writeFileSync(${JSON.stringify(runtimeLog)}, JSON.stringify({ source }));
  `);
  // A stand-in for Tauri, which runs the real npm predev/prebuild lifecycle.
  // Then model the unconditional mapped-resource copy in pinned tauri-build
  // 2.7.0 (lib.rs:173-191, 734-763): it derives the app profile from OUT_DIR,
  // overwrites stale destinations, and aborts the app build if copying fails.
  // https://docs.rs/tauri-build/2.7.0/src/tauri_build/lib.rs.html#734-763
  // The real tauri-build dependency runs in Windows Rust CI, not this fixture.
  writeFileSync(join(dir, "tauri.cjs"), `
    const { spawnSync } = require('node:child_process');
    const args = process.argv.slice(2);
    if (args[0] === 'tauri') args.shift();
    require('node:fs').writeFileSync('tauri.json', JSON.stringify(args));
    const r = spawnSync(${JSON.stringify(windows ? "npm.cmd" : "npm")}, ['run', args[0]], {
      stdio: 'inherit', shell: ${windows}, env: process.env,
    });
    if (r.status !== 0) process.exit(r.status ?? 1);
    const fs = require('node:fs');
    const path = require('node:path');
    const profile = args[0] === 'dev' ? 'debug' : 'release';
    const appDir = path.resolve(process.env.CARGO_TARGET_DIR || 'target', profile);
    fs.mkdirSync(appDir, { recursive: true });
    for (const [source, target] of Object.entries(${JSON.stringify(resourceMap)})) {
      const from = path.resolve('src-tauri', source);
      const to = path.resolve(appDir, target);
      if (from !== to) fs.copyFileSync(from, to);
    }
    fs.writeFileSync(path.join(appDir, 'coucou.exe'), 'fixture app');
    const runtime = spawnSync(process.execPath, ['runtime.cjs', appDir], { stdio: 'inherit' });
    process.exit(runtime.status ?? 1);
  `);
  for (const name of ["cargo", "tauri"]) {
    if (windows) {
      writeFileSync(join(bin, `${name}.cmd`), `@echo off\r\n"%TEST_NODE%" "%~dp0..\\..\\${name}.cjs" %*\r\nexit /b %errorlevel%\r\n`);
    } else {
      writeFileSync(join(bin, name), `#!/bin/sh\nexec "$TEST_NODE" "$(dirname "$0")/../../${name}.cjs" "$@"\n`, { mode: 0o755 });
    }
  }
  if (windows) {
    // Never let npx resolve or download a real Tauri executable in these tests.
    writeFileSync(join(bin, "npx.cmd"), '@echo off\r\n"%TEST_NODE%" "%~dp0..\\..\\tauri.cjs" %*\r\nexit /b %errorlevel%\r\n');
  }
  return { dir, resource, appLog, runtimeLog, env };
}

function runNpm(f, phase, args = []) {
  return spawnSync(windows ? "npm.cmd" : "npm", ["run", phase, ...args], {
    cwd: f.dir, env: f.env, encoding: "utf8", shell: windows, timeout: 30_000,
  });
}

function success(result) {
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
}

for (const phase of ["dev", "build"]) {
  for (const target of ["default", "absolute", "relative"]) {
    test(`${phase} builds a fresh relay with ${target} target settings`, (t) => {
      const f = fixture(t);
      if (target !== "default") f.env.CARGO_TARGET_DIR = target === "absolute" ? join(f.dir, "custom target") : "custom target";
      if (target === "relative") {
        mkdirSync(dirname(f.resource), { recursive: true });
        writeFileSync(f.resource, "stale relay");
      } else {
        assert.equal(existsSync(f.resource), false, "start from a clean checkout");
      }
      success(runNpm(f, phase));
      assert.equal(readFileSync(f.resource, "utf8"), "fresh relay");
      assert.deepEqual(JSON.parse(readFileSync(f.appLog, "utf8")), { target: f.env.CARGO_TARGET_DIR ?? null });
      // This was only the frontend lifecycle. The app's Rust build runs next
      // and stages the resource into its own target/profile, including GPU cv.
      if (target !== "default") assert.equal(existsSync(resolve(f.dir, f.env.CARGO_TARGET_DIR, "release", relay)), false);

      // A failed rebuild must stop the lifecycle even when an old resource exists.
      rmSync(f.appLog);
      f.env.TEST_CARGO_FAIL = "1";
      const failed = runNpm(f, phase);
      assert.ifError(failed.error);
      assert.notEqual(failed.status, 0);
      assert.equal(existsSync(f.appLog), false);
    });
  }
}

for (const phase of ["dev", "build", "standalone"]) {
  for (const target of ["default", "absolute", "gpu"]) {
    test(`${phase} models a fresh runtime relay in the ${target} app target`, (t) => {
      const f = fixture(t);
      if (target !== "default") f.env.CARGO_TARGET_DIR = join(f.dir, target === "gpu" ? "cv" : "custom target");
      const appTarget = resolve(f.dir, f.env.CARGO_TARGET_DIR || "target");
      const profile = phase === "dev" ? "debug" : "release";
      const adjacent = join(appTarget, profile, relay);
      const args = ["--", phase === "standalone" ? "build" : phase, ...(phase === "standalone" ? ["--no-bundle"] : [])];
      assert.equal(existsSync(adjacent), false, "start without an app or installed relay");
      const verify = () => {
        success(runNpm(f, "tauri", args));
        assert.equal(readFileSync(f.resource, "utf8"), "fresh relay", "keep the installer resource");
        assert.equal(readFileSync(adjacent, "utf8"), "fresh relay");
        assert.equal(JSON.parse(readFileSync(f.runtimeLog, "utf8")).source, adjacent);
      };
      verify();
      // A newly built relay must replace stale copies at every candidate the
      // current profile can prefer, especially debug/ before ../release/.
      writeFileSync(f.resource, "stale resource");
      for (const name of ["debug", "release"]) {
        mkdirSync(join(appTarget, name), { recursive: true });
        writeFileSync(join(appTarget, name, relay), "stale app relay");
      }
      verify();
      if (phase === "standalone") {
        const portable = join(f.dir, "portable elsewhere");
        mkdirSync(portable);
        for (const name of ["coucou.exe", relay]) copyFileSync(join(appTarget, profile, name), join(portable, name));
        success(spawnSync(process.execPath, ["runtime.cjs", portable], { cwd: f.dir, encoding: "utf8" }));
        assert.equal(JSON.parse(readFileSync(f.runtimeLog, "utf8")).source, join(portable, relay));
      }
      rmSync(f.runtimeLog);
      f.env.TEST_CARGO_FAIL = "1";
      const failed = runNpm(f, "tauri", args);
      assert.ifError(failed.error);
      assert.notEqual(failed.status, 0);
      assert.equal(existsSync(f.runtimeLog), false, "never launch after a failed relay build");
    });
  }
}

for (const phase of ["dev", "build"]) {
  test(`${phase} resource staging failure stops before runtime`, (t) => {
    const f = fixture(t);
    f.env.CARGO_TARGET_DIR = join(f.dir, "cv");
    // A directory where the file should go forces a real filesystem copy error.
    mkdirSync(join(f.env.CARGO_TARGET_DIR, phase === "dev" ? "debug" : "release", relay), { recursive: true });
    const failed = runNpm(f, "tauri", ["--", phase]);
    assert.ifError(failed.error);
    assert.notEqual(failed.status, 0);
    assert.equal(readFileSync(f.resource, "utf8"), "fresh relay");
    assert.match(failed.stderr, /copyfile/, "the mapped resource copy failed");
    assert.equal(existsSync(f.runtimeLog), false);
  });
}

for (const name of ["dev.cmd", "build-exe.cmd"]) {
  for (const mode of ["cpu", "custom", "gpu"]) {
    test(`${name} preserves the ${mode} app target through cmd.exe`, { skip: !windows }, (t) => {
      const f = fixture(t);
      const scripts = join(f.dir, "scripts");
      mkdirSync(scripts);
      copyFileSync(join(root, "scripts", name), join(scripts, name));
      // Do not initialize Visual Studio or Vulkan, or touch the real drive root.
      writeFileSync(join(scripts, "vs-env.cmd"), '@echo off\r\nif /i "%~1"=="gpu" set "CARGO_TARGET_DIR=%TEST_GPU_TARGET%"\r\nexit /b 0\r\n');
      if (mode !== "cpu") f.env.CARGO_TARGET_DIR = join(f.dir, "custom app target");
      const expectedTarget = mode === "gpu" ? f.env.TEST_GPU_TARGET : f.env.CARGO_TARGET_DIR ?? null;
      const run = () => spawnSync(process.env.ComSpec || "cmd.exe", ["/d", "/c", `scripts\\${name}`, ...(mode === "gpu" ? ["gpu"] : [])], {
        cwd: f.dir, env: f.env, encoding: "utf8", input: "\r\n", timeout: 30_000,
      });
      const result = run();
      success(result);
      assert.equal(readFileSync(f.resource, "utf8"), "fresh relay");
      assert.deepEqual(JSON.parse(readFileSync(f.appLog, "utf8")), { target: expectedTarget });
      const adjacent = join(expectedTarget || join(f.dir, "target"), name === "dev.cmd" ? "debug" : "release", relay);
      assert.equal(JSON.parse(readFileSync(f.runtimeLog, "utf8")).source, adjacent);
      assert.equal(readFileSync(adjacent, "utf8"), "fresh relay");
      const tauriArgs = JSON.parse(readFileSync(join(f.dir, "tauri.json"), "utf8"));
      assert.equal(tauriArgs.includes("gpu-vulkan"), mode === "gpu");
      if (name === "build-exe.cmd") assert.ok(result.stdout.includes(`Done: ${expectedTarget || join(f.dir, "target")}\\release\\coucou.exe`));

      rmSync(f.appLog);
      rmSync(f.runtimeLog);
      f.env.TEST_CARGO_FAIL = "1";
      const failed = run();
      assert.ifError(failed.error);
      assert.equal(failed.status, 1);
      assert.equal(existsSync(f.appLog), false);
      assert.equal(existsSync(f.runtimeLog), false);
    });
  }
}

function sdkFixture(t) {
  const f = fixture(t);
  for (const key of Object.keys(f.env)) {
    if (/^(vulkan_sdk|systemdrive)$/i.test(key)) delete f.env[key];
  }
  // Isolate the real GPU setup section. Visual Studio initialization above it
  // is deliberately excluded; all SDK directories live inside the fixture.
  const source = script("vs-env.cmd");
  const gpuStart = source.indexOf('if /i not "%~1"=="gpu" exit /b 0');
  assert.ok(gpuStart >= 0, "the GPU section follows Visual Studio setup");
  writeFileSync(join(f.dir, "gpu-env.cmd"), `@echo off\r\n${source.slice(gpuStart)}`);
  f.env.SystemDrive = f.dir;
  mkdirSync(join(f.dir, "VulkanSDK"));
  writeFileSync(join(f.dir, "sdk.cjs"), `
    require('node:fs').writeFileSync('sdk.json', JSON.stringify({
      sdk: process.env.VULKAN_SDK ?? null,
      target: process.env.CARGO_TARGET_DIR ?? null,
    }));
  `);
  writeFileSync(join(f.dir, "sdk.cmd"), [
    "@echo off",
    'call gpu-env.cmd %1',
    'if errorlevel 1 exit /b 1',
    '"%TEST_NODE%" sdk.cjs',
    'exit /b %errorlevel%',
    '',
  ].join("\r\n"));
  f.runSdk = (mode = "gpu") => spawnSync(process.env.ComSpec || "cmd.exe", ["/d", "/c", "sdk.cmd", mode], {
    cwd: f.dir, env: f.env, encoding: "utf8", timeout: 30_000,
  });
  return f;
}

for (const version of ["1.4.363.0", "1.4.363.0 SDK with spaces"]) {
  test(`cmd.exe discovers an unquoted Vulkan SDK path for ${version}`, { skip: !windows }, (t) => {
    const f = sdkFixture(t);
    const sdk = join(f.dir, "VulkanSDK", version);
    mkdirSync(sdk);
    success(f.runSdk());
    const actual = JSON.parse(readFileSync(join(f.dir, "sdk.json"), "utf8"));
    assert.equal(actual.sdk, sdk, "%%~fd produces a usable fully qualified path without quotes");
    assert.equal(actual.sdk.includes('"'), false);
    assert.match(actual.target, /^[a-z]:\\cv$/i);
  });
}

test("cmd.exe preserves an existing Vulkan SDK path containing spaces", { skip: !windows }, (t) => {
  const f = sdkFixture(t);
  const sdk = join(f.dir, "user selected SDK");
  mkdirSync(sdk);
  mkdirSync(join(f.dir, "VulkanSDK", "other SDK"));
  f.env.VULKAN_SDK = sdk;
  success(f.runSdk());
  assert.equal(JSON.parse(readFileSync(join(f.dir, "sdk.json"), "utf8")).sdk, sdk);
});

test("cmd.exe fails GPU setup when no Vulkan SDK is configured or discovered", { skip: !windows }, (t) => {
  const f = sdkFixture(t);
  const failed = f.runSdk();
  assert.ifError(failed.error);
  assert.equal(failed.status, 1);
  assert.match(failed.stdout, /The Vulkan SDK was not found/);
  assert.equal(existsSync(join(f.dir, "sdk.json")), false);
});

test("cmd.exe CPU setup needs no Vulkan SDK and preserves the app target", { skip: !windows }, (t) => {
  const f = sdkFixture(t);
  f.env.CARGO_TARGET_DIR = join(f.dir, "cpu target");
  success(f.runSdk("cpu"));
  const actual = JSON.parse(readFileSync(join(f.dir, "sdk.json"), "utf8"));
  assert.equal(actual.sdk, null);
  assert.equal(actual.target, f.env.CARGO_TARGET_DIR);
});

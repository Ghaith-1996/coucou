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

test("both relay lifecycle hooks explicitly target Tauri's resource directory", () => {
  for (const hook of ["predev", "prebuild"]) {
    assert.equal(pkg.scripts[hook], "cargo build --release -p coucou-hook --target-dir target");
  }
  assert.equal(config.build.beforeDevCommand, "npm run dev");
  assert.equal(config.build.beforeBuildCommand, "npm run build");
  assert.equal(config.bundle.resources["../target/release/coucou-hook.exe"], "coucou-hook.exe");
  assert.equal(linuxConfig.bundle.resources["../target/release/coucou-hook"], "coucou-hook");
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
  // A stand-in for Tauri, which runs the real npm predev/prebuild lifecycle.
  writeFileSync(join(dir, "tauri.cjs"), `
    const { spawnSync } = require('node:child_process');
    const args = process.argv.slice(2);
    if (args[0] === 'tauri') args.shift();
    require('node:fs').writeFileSync('tauri.json', JSON.stringify(args));
    const r = spawnSync(${JSON.stringify(windows ? "npm.cmd" : "npm")}, ['run', args[0]], {
      stdio: 'inherit', shell: ${windows}, env: process.env,
    });
    process.exit(r.status ?? 1);
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
  return { dir, resource, appLog, env };
}

function runNpm(f, phase) {
  return spawnSync(windows ? "npm.cmd" : "npm", ["run", phase], {
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
      const tauriArgs = JSON.parse(readFileSync(join(f.dir, "tauri.json"), "utf8"));
      assert.equal(tauriArgs.includes("gpu-vulkan"), mode === "gpu");
      if (name === "build-exe.cmd") assert.ok(result.stdout.includes(`Done: ${expectedTarget || join(f.dir, "target")}\\release\\coucou.exe`));

      rmSync(f.appLog);
      f.env.TEST_CARGO_FAIL = "1";
      const failed = run();
      assert.ifError(failed.error);
      assert.equal(failed.status, 1);
      assert.equal(existsSync(f.appLog), false);
    });
  }
}

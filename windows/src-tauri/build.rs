fn main() {
    // Copies the mapped relay resource beside the actual app executable, using
    // OUT_DIR (including custom CARGO_TARGET_DIR/profile/target triples). This
    // also runs for dev and --no-bundle; predev/prebuild only prepare the source
    // at windows/target/release. Tauri watches it and propagates copy failures.
    tauri_build::build()
}

# Image regression harness

Run `cargo test --manifest-path native/tests/image-harness/Cargo.toml` from the repository root.

This compiles the production image store and its tests, without the Tauri desktop shell.
On Linux the harness provides only the macOS-equivalent symlink guard. The actual image
path, decode, save, preview lookup and deletion logic is imported unchanged.

The tests cover the distinction between internal hash paths and public filename lookups,
sharded and legacy originals, invalid/missing filenames, and lookup after deletion.
This is not a Windows/macOS application build or a substitute for native CI.

# Database regression harness

From the repository root:

```sh
cargo test --manifest-path native/tests/db-harness/Cargo.toml
```

This imports the actual `native/src/db.rs`, including its existing tests. It can run on
Linux without the Windows/macOS-only Tauri shell. It is not a native app build or a
replacement for macOS/Windows integration tests. Dependencies/features relevant to the
DB match the app manifest; there is no duplicate implementation of DB logic here.

The color regression enumerates Rust's Unicode whitespace classification, checks newline,
Tab and Unicode-padded values, non-colors, and content beyond the preview boundary.
Both display classification and text/color filter membership must match `hex_color`.

Frontend interaction regression (from the repository root after `npm ci` in `ui`):

```sh
node --test ui/tests/selection.test.mjs
```

This executes the actual App.vue event handlers with lightweight refs and the production
selection helper. It covers keyboard focus → hover → first Shift-click, Cmd/Ctrl toggles,
range selection, cancel, plain-click paste and anchor identity across list changes.
No real WebView/native clipboard is exercised.

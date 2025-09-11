# Repository Guidelines

## Project Structure & Module Organization
- `src/lib.rs`: Public API, core types (`Host`, `Device`, `DeviceError`).
- `src/adb.rs`: Low-level ADB protocol and sync operations.
- `src/shell.rs`: Shell helpers and escaping utilities.
- `src/test.rs`: Integration-style async tests (serialized where needed).
- `examples/hello-world.rs`: Minimal usage example.
- `.github/workflows/rust.yml`: CI for build, fmt, clippy, tests.
- Main types: `Host`, `Device`, `FileMetadata`, `AndroidStorage`.

## Architecture Overview
- Tokio-based async ADB client with sync protocol coverage: file push/pull, directory ops, package install/uninstall/list, shell (`exec:`/`shell:`), and port forward/reverse.
- Transfer progress reporting; chunk sizes: pull 64KB, push 32KB; progress updates throttled for large files.
- Run-as support for app storage paths with safe temp staging and permission handling; paths are validated and sanitized.
- Errors use `DeviceError`; ADB connect timeout is 5s; responses decoded as UTF‑8 with normalized newlines.

## Build, Test, and Development Commands
- Build: `cargo build` (use `--verbose` when debugging CI).
- Lint: `cargo clippy -- -D warnings` (must be clean in PRs).
- Format (check): `cargo fmt --all -- --check`; apply with `cargo fmt`.
- Unit tests: `cargo test`.
- Device tests (require ADB/emulator): `cargo test -- --ignored --test-threads=1`.
- Example: `cargo run --example hello-world`.
- Target a device: `ANDROID_SERIAL=emulator-5554 cargo test -- --ignored --test-threads=1`.

## Coding Style & Naming Conventions
- Rust 2021 edition, rustfmt defaults (4-space indent, 100 cols typical).
- Names: modules `snake_case`, types/traits `CamelCase`, functions `snake_case`, constants `SCREAMING_SNAKE_CASE`.
- Prefer `?` for error flow; return `Result<_, DeviceError>` from fallible APIs.
- Keep modules focused; expose public API via `lib.rs`; protocol details live in `adb.rs`.

## Testing Guidelines
- Async tests use `#[tokio::test]`; serialize shared-ADB cases with `serial_test` (e.g., `#[serial(forward)]`).
- Many device-dependent tests are `#[ignore]`; run them locally with the command above.
- CI runs `cargo test`, `clippy`, and `fmt` on Linux/macOS/Windows.

## Commit & Pull Request Guidelines
- Commits: short, imperative subject (e.g., "Add progress reporting", "Fix tests").
- PRs: include a clear description, linked issue(s), and notes on testing (mention if device/emulator was used). Ensure clippy and fmt pass.

## Security & Configuration Tips
- ADB must be installed and accessible as `adb`; server defaults to `localhost:5037`.
- Select a device via `ANDROID_SERIAL` or by passing a serial to `Host::device_or_default`.
- Library avoids root by default; use `run-as` only when explicitly enabled via `Device.run_as_package`.
- Avoid adding code that executes privileged commands implicitly; prefer explicit APIs.
- Dependencies: Tokio, `bstr`, `tempfile`, `walkdir`, `uuid`.

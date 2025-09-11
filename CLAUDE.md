# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is `forensic-adb`, a Rust library providing a Tokio-based async client for the Android Debug Bridge (adb) protocol. It's based on Mozilla's mozdevice but adapted for forensic analysis with root detection removed to avoid executing commands on remote devices by default.

## Key Architecture

### Core Modules
- `src/lib.rs` - Main library exports, core types (`Host`, `Device`), and primary ADB protocol implementation
- `src/adb.rs` - Low-level ADB protocol definitions and sync commands  
- `src/shell.rs` - Shell command utilities and escaping functions
- `src/test.rs` - Test utilities and helper functions

### Main Types
- `Host` - Represents ADB server connection, handles device discovery and management
- `Device` - Represents individual Android device, handles all device operations
- `DeviceError` - Comprehensive error handling for ADB operations
- `FileMetadata` - File system information from device
- `AndroidStorage` - Storage location targeting (App/Internal/Sdcard)

### Protocol Implementation
The library implements the full ADB sync protocol for file operations:
- File transfer (push/pull) with progress reporting
- Directory operations with recursive support
- Package management (install/uninstall/list)
- Shell command execution with run-as support
- Port forwarding and reverse port forwarding

## Development Commands

### Building and Testing
```bash
# Build the project
cargo build

# Build with verbose output  
cargo build --verbose

# Run all tests
cargo test --verbose

# Run tests with serial execution (required for ADB tests)
cargo test --verbose -- --test-threads=1
```

### Code Quality
```bash
# Run clippy linting
cargo clippy -- -D warnings

# Format code
cargo fmt --all -- --check

# Apply formatting
cargo fmt
```

### Running Examples
```bash
# Run the hello-world example
cargo run --example hello-world
```

## Testing Strategy

The project uses `serial_test` crate to ensure ADB operations don't interfere with each other during testing. Tests marked with `#[serial]` run sequentially rather than in parallel.

Key test requirements:
- Tests must run serially to avoid ADB conflicts
- Use `serial_test` and `serial_test_derive` for test synchronization
- Tests require actual ADB device or emulator connection

## Important Implementation Details

### Security Considerations
- No root commands are executed by default (forensic safety)
- Run-as functionality available for app-specific operations  
- Proper path sanitization and validation

### File Operations
- Uses 32KB buffer for push operations, 64KB for pull operations
- Progress reporting available for large file transfers
- Automatic directory creation with permission handling
- Temporary file staging for run-as operations

### Error Handling
- Comprehensive `DeviceError` enum covering all failure modes
- Proper timeout handling for network operations (5 second ADB connect timeout)
- UTF-8 validation for all string operations

## Dependencies Notes

- Built on Tokio for async operations
- Uses `bstr` for byte string handling in shell operations
- `tempfile` for secure temporary file creation
- `walkdir` for recursive directory traversal
- `uuid` for generating unique temporary file names
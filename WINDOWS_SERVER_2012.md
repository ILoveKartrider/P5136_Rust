# Windows Server 2012 x64 Compatibility Build Guide

If any translation sounds unnatural or is incorrect, please [open an issue](https://github.com/ILoveKartrider/P5136_Rust/issues) or submit a pull request.

## Summary

The current P5136 source does not require a Windows Server 2012-specific code path or source modification. A compatibility build should use the same source as the regular release with only a separate Rust target and static CRT build flags.

This procedure is intended to improve compatibility; it does not guarantee that the program will run on Windows Server 2012. Before distributing the binary, test process startup, GUI initialization, server startup, and client connectivity on the actual operating system.

## Why the regular x64 release may not run

The current official baseline for the default `x86_64-pc-windows-msvc` target is Windows 10 and Windows Server 2016 or newer. The current MSVC dynamic runtime officially supports the same range, so the regular release may fail during loader or CRT initialization on Windows Server 2012.

- Rust Windows MSVC requirements: <https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html>
- Microsoft Visual C++ Redistributable requirements: <https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist?view=msvc-170>

If only the GUI fails, also investigate Server Core versus Desktop Experience, the graphics driver, OpenGL support, and `winit` window initialization.

## Compatibility build policy

- Continue building the regular Windows release with `x86_64-pc-windows-msvc`.
- Build the Windows Server 2012 binary separately with `x86_64-win7-windows-msvc`.
- Use `crt-static` to reduce the runtime dependency on the current Visual C++ Redistributable.
- Do not replace the regular release with the compatibility build. Publish it as a separately named release asset.
- Do not add a global `.cargo` target configuration or compatibility-specific `cfg` branches to the source.

`x86_64-win7-windows-msvc` is recognized as a Tier 3 target by Rust 1.94, but a precompiled standard library cannot be installed with `rustup target add`. It therefore requires nightly Rust, `build-std`, and the `rust-src` component.

## Build commands

Run these commands on a Windows build machine with the MSVC build tools and Windows SDK installed:

```powershell
rustup toolchain install nightly --component rust-src

$previousRustFlags = $env:RUSTFLAGS
$env:RUSTFLAGS = "-C target-feature=+crt-static"

cargo +nightly build `
  --release `
  --locked `
  -p p5136-cli `
  --target x86_64-win7-windows-msvc `
  --target-dir target/p5136-win2012 `
  -Z build-std=std,panic_abort

$env:RUSTFLAGS = $previousRustFlags
```

The expected output is:

```text
target/p5136-win2012/x86_64-win7-windows-msvc/release/p5136.exe
```

Rename the file when publishing it so that it cannot be confused with the regular x64 build:

```text
p5136-win2012-x64.exe
```

Nightly toolchains can change over time. For the first build that passes testing on real hardware, record the output of `rustup show` and `rustc +nightly -vV`. Pin that exact dated nightly for subsequent releases.

## Validation procedure

First run the following command from a command prompt on Windows Server 2012:

```powershell
p5136-win2012-x64.exe --version
```

- If it fails before printing the version, investigate the PE loader, missing DLLs, CRT initialization, and unsupported operating-system APIs first.
- If it prints the version but only the GUI fails, investigate Desktop Experience, graphics drivers, OpenGL, and window initialization first.
- If the GUI opens, test server startup, a `127.0.0.1` connection, a LAN connection, clean shutdown, and a second launch.

If it fails, preserve the exact dialog text, process exit code, and the relevant `Application Error` and `Windows Error Reporting` entries from Event Viewer.

## Current validation status

This document defines only a source-compatible build procedure. It has not yet been verified as an official release on an actual Windows Server 2012 system. Until a successful build hash and test results are available, publish the resulting binary as an experimental asset.

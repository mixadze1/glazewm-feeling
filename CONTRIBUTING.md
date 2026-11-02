# Contributing to GlazeWM Feeling

Thanks for helping make window management feel better!

GlazeWM Feeling is a Windows-focused fork of [GlazeWM by glzr-io](https://github.com/glzr-io/glazewm). It builds on the upstream project with workspace animations, keyboard resizing, focus outlines, and layout improvements. This guide is for contributions to **Feeling**.

## Ways to help

- **Report a bug** or suggest an improvement in [Feeling's issues](https://github.com/mixadze1/glazewm-feeling/issues).
- **Help reproduce an issue** by sharing clear steps, configuration details, or a short recording.
- **Improve the code or documentation** with a focused pull request.
- **Support development** through [Ko-fi](https://ko-fi.com/mixadze1). Donations are optional; feedback, bug reports, and code contributions are just as welcome.

[![Support on Ko-fi](https://img.shields.io/badge/Support-Ko--fi-ff5e5b?logo=ko-fi&logoColor=white)](https://ko-fi.com/mixadze1)

## Reporting bugs and requesting features

Search the [existing issues](https://github.com/mixadze1/glazewm-feeling/issues) first. For a bug report, include:

- Your Feeling version and Windows version.
- Steps to reproduce, what you expected, and what happened instead.
- The affected applications and relevant configuration snippets.
- Your monitor arrangement and display scaling if the problem involves layouts, movement, or outlines.
- A screenshot or short recording when it helps explain the behavior.

Remove private information from logs, screenshots, and configuration before sharing them. If you can compare the behavior with upstream GlazeWM, mention the result, but that comparison is not required.

For a feature request, describe the task you want to make easier and give a concrete example. Discuss larger changes in an issue before starting implementation.

Please report Feeling-specific problems here, rather than in the upstream GlazeWM or Zebar repositories. If a problem is confirmed in upstream GlazeWM too, link the related upstream issue so the work can be coordinated.

## Local development

Use Windows with the C++ build tools and Windows SDK required by the Rust MSVC toolchain. Install [Rust through rustup](https://rustup.rs/). The repository's [rust-toolchain.toml](rust-toolchain.toml) selects the toolchain used for development.

Fork this repository, then clone your fork:

```powershell
git clone https://github.com/YOUR-USERNAME/glazewm-feeling.git
cd glazewm-feeling
git switch -c fix/describe-your-change
```

Build the application, CLI, and watcher:

```powershell
# Local development version used by the Windows resource build scripts.
$env:VERSION_NUMBER = '0.0.0'
cargo build -p wm -p wm-cli -p wm-watcher
```

Back up your configuration at `%USERPROFILE%\.glzr\glazewm\config.yaml`, then close any running GlazeWM instance before starting your development build:

```powershell
cargo run -p wm
```

Feeling keeps GlazeWM's executable and configuration names. Avoid running the upstream application and your development build at the same time. See the [README](README.md#build-from-source) for release build instructions and [sample configuration](resources/assets/sample-config.yaml) for default settings.

## Pull requests

Open a [pull request in this repository](https://github.com/mixadze1/glazewm-feeling/pulls) targeting `main`.

- Keep each PR focused on one fix or feature; avoid unrelated formatting changes.
- Explain the problem, the resulting behavior, and how you checked it. Link a related issue when one exists.
- Include before/after screenshots or a short recording for visible changes.
- Update documentation and sample configuration when behavior or settings change.
- Check that existing configurations and keyboard workflows still work.

Before submitting code changes, run these checks from the repository root, with `VERSION_NUMBER` still set:

```powershell
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Run relevant existing tests and manually check the behavior you changed. For animation or layout work, include rapid workspace switches, interrupted transitions, and multi-monitor behavior when relevant. State what you actually verified and any checks you could not run. Documentation-only changes do not need an application build.

## Codebase overview

The Rust workspace is organized under [`packages`](packages):

| Crate | Purpose |
| --- | --- |
| `wm` | Window management, commands, event handling, and animations. |
| `wm-cli` | Command-line interface for interacting with the running window manager. |
| `wm-common` | Shared types and utilities. |
| `wm-ipc-client` | WebSocket client for communication with the window manager. |
| `wm-platform` | Platform-specific APIs and native window handling. |
| `wm-watcher` | Cleanup when the main application exits. |
| `wm-macros` | Procedural macros shared by the workspace. |

The window manager's state lives in [`WmState`](packages/wm/src/wm_state.rs). [Commands](packages/wm/src/commands) handle actions from keyboard bindings, IPC, and internal calls; [events](packages/wm/src/events) react to platform changes. The application entry point is [`main.rs`](packages/wm/src/main.rs).

Windows are represented in a container tree: root, monitors, workspaces, split containers, and application windows. You do not need to understand the whole codebase to contribute; start with the area related to your change.

## Credit

Feeling builds on the work of the GlazeWM maintainers and contributors. Keep that attribution intact, and credit any external code or assets you include. Thank you for contributing!

# Code and UX Review

## Code review

### Fixed in this pass

- Terminal cleanup was not guarded. If drawing or input handling failed, raw mode could remain active. A `TerminalGuard` now restores the terminal on drop.
- The app had no durable user home. `~/.whispercli` is now created with `bin`, `models`, `transcripts`, and `logs`.
- Model state was only visual. `whispercli models install tiny|base` now downloads ggml models and verifies SHA1 from the whisper.cpp model table.
- Errors were mostly raw `?` propagation. The current code adds context around filesystem, terminal, download, checksum, and PATH setup failures.
- Distribution was undefined. CI, release packaging, npm wrapper, and Windows installer scripts are now present.
- The TUI no longer shows placeholder transcript text, placeholder microphone devices, placeholder audio levels, or advertised shortcuts that do not exist.
- `whispercli` with no subcommand now opens the TUI.
- Mouse capture is enabled for the TUI, with clickable setup and quit actions.

### Remaining engineering risks

- Live transcription now uses `cpal` for microphone capture and `whisper-rs` for chunked whisper.cpp inference, but short-chunk duplicate suppression and silence detection will need more real-world tuning.
- Native folder picking is represented as selectable common output folders in the TUI; a true OS folder picker is intentionally not used so the app remains terminal-native.
- Local default-feature builds require libclang for `whisper-rs` bindgen. CI installs LLVM before building release binaries; end users receive native binaries and do not need LLVM.

## UX review

### What is working

- The first visual direction is calmer and closer to a serious CLI tool.
- Wide terminals get a transcript-first layout with a compact right inspector.
- Narrow terminals collapse into a vertical status layout instead of overflowing.
- The app now exposes `doctor`, `init`, and `models` commands, which makes onboarding less mysterious.

### UX decisions

- Default install home is `~/.whispercli`, because the user asked for a visible user-owned folder and it works consistently across Windows/macOS/Linux.
- `tiny` is the default install suggestion because it keeps the first-run experience light.
- `.en` models are not listed in the primary flow because Japanese support is a core use case.
- npm is treated as a thin downloader, not the real runtime. The runtime remains a native Rust binary.

### Next UX pass

- Add `--plain` for automation and pipes.
- Add first-run hint inside the TUI when the selected model is missing.
- Add a progress indicator for model downloads.
- Add `whispercli models install recommended` as an alias for `tiny` or `base`.

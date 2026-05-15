# Changelog

## v0.3.0

- Added `--version`, `live --plain`, `live --jsonl`, `live --device`, `live --format`, `file <audio.wav>`, `doctor --json`, `models verify`, `models remove`, and `config get/set`.
- Added SRT, JSON, and JSONL transcript output formats.
- Fixed `--lang auto` so Whisper language auto-detection is requested correctly.
- Added audio callback drop counters and surfaced dropped audio chunks in the TUI.
- Flushes pending audio on stop before saving the final transcript.
- Switched live transcript writing to append-style writes instead of rewriting the whole file on every segment.
- Preserves explicit `--out` extensions unless `--format` is also provided.
- Detects corrupt installed models in `models list` and `doctor`.
- Added SHA256 checksum generation in Release assets and checksum verification in installers.
- Made npm install versioned by default instead of downloading `latest`.
- Added focused unit/CLI tests and stronger CI coverage.

## v0.2.0

- Added initial GitHub Release artifacts and npm wrapper distribution.
- Added model install/list, setup, doctor, device listing, and live TUI workflow.

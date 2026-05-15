# Changelog

## v0.3.3

- Reworked live transcription around rolling windows with overlap instead of fixed non-overlapping chunks.
- Added lightweight RMS-based VAD to skip silence and reduce hallucinated transcript text.
- Added overlap-aware dedupe so repeated text from rolling windows is trimmed before saving.
- Added `latency_mode` / `live --latency fast|balanced|accurate` with `balanced` as the default.
- Added inference backlog protection and surfaced skipped inference windows in plain/jsonl/TUI output.

## v0.3.2

- Added `large-v3-turbo-q5_0` model support and made `recommended` resolve to that model.
- Fixed TUI keyboard handling so release/repeat events do not move focus multiple times.
- Made arrow-key focus movement stop at list edges instead of wrapping unexpectedly.

## v0.3.1

- Split microphone capture/segmentation from Whisper inference so slow transcription no longer blocks audio queue draining.
- Finalization now stops capture before draining queued audio, reducing the chance of losing the last moments of speech.
- Whisper live/file transcription now keeps decoder context, uses the previous segment as a prompt, and joins multi-segment output without collapsing English words.

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

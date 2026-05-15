# whisperccpcli

npm wrapper for `whispercli`, a local-first Whisper CLI.

```sh
npm install -g whisperccpcli
whispercli doctor
whispercli models install large-v3-turbo-q5_0
whispercli live --plain --format txt --out meeting.txt --latency balanced
```

The postinstall script downloads the GitHub Release asset matching this package version by default. It verifies SHA256 checksums when the release provides `checksums.txt`.

Set `WHISPERCLI_VERSION`, `WHISPERCLI_INSTALL_DIR`, or `WHISPERCLI_SKIP_DOWNLOAD` to customize installation.

# whisperCLI

Rust製の軽量リアルタイム文字起こしCLIです。

現在のMVPは、`ratatui` + `crossterm` によるリッチTUI、ユーザー配下の
`~/.whispercli` 初期化、モデル管理、マイク入力、whisper.cpp推論、逐次保存の土台です。

端末サイズに応じて、横分割レイアウトと縦積みレイアウトを切り替えます。

## Install

### Windows without npm or winget

PowerShellで直接インストールできます。バイナリは `~\.whispercli\bin` に入り、
ユーザーPATHへ登録されます。

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/Aero123421/whisperccpcli/main/scripts/install.ps1 | iex"
```

### npm

```powershell
npm install -g whisperccpcli
whispercli doctor
```

npm版インストーラーは `package.json` のバージョン `vX.Y.Z` を既定で取得します。
必要なら `WHISPERCLI_VERSION`（任意）でインストール対象の GitHub Release タグを上書きできます。
また `WHISPERCLI_INSTALL_DIR`（インストール先）や `WHISPERCLI_SKIP_DOWNLOAD`（ダウンロードをスキップ）も利用可能です。

### macOS / Linux

```sh
curl -fsSL https://raw.githubusercontent.com/Aero123421/whisperccpcli/main/scripts/install.sh | sh
```
インストーラーは可能なら `checksums.txt` を取得して SHA256 検証を行います。

## Setup

```powershell
whispercli init
whispercli config
whispercli models install tiny
whispercli models install large-v3-turbo-q5_0
whispercli models list
```

初回セットアップを1コマンドで済ませる場合:

```powershell
whispercli init --download tiny
```

`whispercli init` は以下を自動作成します。

```text
~/.whispercli/
  bin/
  models/
  transcripts/
  logs/
```

## Run

Installed binary:

```powershell
whispercli
```

Settings:

```powershell
whispercli config
```

With options:

```powershell
whispercli live --out meeting.md --model tiny --lang ja
whispercli live --out meeting.md --model large-v3-turbo-q5_0 --lang ja --latency balanced
whispercli live --plain --format txt --out meeting.txt
whispercli live --jsonl --out live.jsonl
whispercli file audio.wav --format srt --out transcript.srt
```

日本語の安定性を優先する場合は、`tiny` より `large-v3-turbo-q5_0` を推奨します。
`recommended` は `large-v3-turbo-q5_0` の別名です。
live transcription はデフォルトで `balanced` latency mode を使い、8秒のrolling windowを2秒ごとに推論します。
低遅延なら `--latency fast`、文脈重視なら `--latency accurate` を指定できます。

終了:

```text
q / Esc / Ctrl+C
```

Mouse:

```text
Click Install model / Quit buttons in the TUI.
```

## MVP scope

- リッチTUI
- レスポンシブレイアウト
- `whispercli` だけで起動
- `whispercli config` でモデル、マイク、保存先、保存形式を変更
- マウスホバー、フォーカス、選択状態の表示
- `cpal` によるマイク入力
- `whisper-rs` / `whisper.cpp` によるrolling window文字起こし
- 軽量VAD、overlap、重複除去、推論backlog保護
- Markdown / Text / SRT / JSON / JSONL への逐次保存
- 保存先、モデル、言語、latency、状態、入力レベル、音声/推論drop数の表示
- `~/.whispercli` の自動作成
- `tiny` / `base` / `small` / `large-v3-turbo-q5_0` モデルのダウンロード、SHA1検証、verify/remove
- `doctor --json`、`config get/set`、`live --plain`、WAVファイル文字起こし
- GitHub ActionsによるCI/CD
- npm wrapper
- Windows/macOS/Linux向けRelease binary
- Windows用PowerShellインストーラー / macOS・Linux用shellインストーラー

## Later

- VAD・dedupeの実音声チューニング
- Homebrew / Scoop / winget
- Linux arm64 / musl binary
- platform別npm package

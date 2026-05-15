# whisperCLI

Rust製の軽量リアルタイム文字起こしCLIです。

現在のMVPは、`ratatui` + `crossterm` によるリッチTUI、ユーザー配下の
`~/.whispercli` 初期化、モデル管理、配布導線の土台です。

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

### macOS / Linux

```sh
curl -fsSL https://raw.githubusercontent.com/Aero123421/whisperccpcli/main/scripts/install.sh | sh
```

## Setup

```powershell
whispercli init
whispercli models install tiny
whispercli models list
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

```powershell
cargo run -- live --out meeting.md --model tiny --lang ja
```

終了:

```text
q / Esc / Ctrl+C
```

## MVP scope

- リッチTUI
- レスポンシブレイアウト
- ライブ文字起こし画面の骨格
- 保存先、モデル、言語、音量、タイムラインの表示
- `~/.whispercli` の自動作成
- `tiny` / `base` モデルのダウンロードとSHA1検証
- GitHub ActionsによるCI/CD
- npm wrapper
- Windows用PowerShellインストーラー

## Later

- `cpal` によるマイク入力
- `whisper-rs` / `whisper.cpp` による文字起こし
- `.txt` / `.md` 保存
- モデルダウンロード
- Windows/macOS/Linux向け単体バイナリ配布

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

### macOS / Linux

```sh
curl -fsSL https://raw.githubusercontent.com/Aero123421/whisperccpcli/main/scripts/install.sh | sh
```

## Setup

```powershell
whispercli init
whispercli config
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
```

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
- `whisper-rs` / `whisper.cpp` によるチャンク文字起こし
- Markdown / Text への逐次保存
- 保存先、モデル、言語、状態、入力レベルの表示
- `~/.whispercli` の自動作成
- `tiny` / `base` / `small` モデルのダウンロードとSHA1検証
- GitHub ActionsによるCI/CD
- npm wrapper
- Windows用PowerShellインストーラー

## Later

- 重複除去とVADの精度改善
- SRT / JSON 出力
- ファイル音声の文字起こし
- Windows/macOS/Linux向け単体バイナリ配布

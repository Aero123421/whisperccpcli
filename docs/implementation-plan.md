# Implementation Plan

## Product direction

`whisperCLI` は、軽量モデルを使ってローカルでリアルタイム文字起こしするRust製CLIです。
通常利用ではリッチTUIを表示し、自動化やログ用途ではplain出力も選べるようにします。

## Phase 1: TUI skeleton

- `clap` でCLIコマンドを定義する
- `ratatui` + `crossterm` でリッチTUIを描画する
- 端末幅に応じてレイアウトを切り替える
  - Wide: transcript + right inspector
  - Narrow: transcript + stacked status
- `q`, `Esc`, `Ctrl+C` で終了する

## Phase 2: Local transcript output

- `.txt` と `.md` の出力形式を実装する
- 実行中に定期flushする
- Ctrl+C終了時に壊れたファイルを残さない
- `--plain` を追加し、TUIなしで標準出力に流せるようにする

## Phase 3: Audio input

- `cpal` でWindows/macOS/Linuxのマイク入力を扱う
- `whispercli devices` で入力デバイス一覧を表示する
- サンプルレート変換とモノラル化を実装する
- TUIに音量メーターと入力状態を反映する

## Phase 4: whisper.cpp

- `whisper-rs` 経由でwhisper.cppを呼ぶ
- 初期対応モデルは `tiny` と `base`
- 日本語を含む多言語モデルを標準にする
- チャンク推論方式でリアルタイム表示する

## Phase 5: Model management

- `whispercli models install tiny`
- `whispercli models list`
- モデルはユーザーディレクトリに保存する
- ダウンロード後にチェックサム検証する

## Phase 6: Distribution

- GitHub Releasesで単体バイナリを配布する
- Windows: `winget`, Scoop
- macOS: Homebrew
- Linux: tarball, deb/rpm
- npm packageはバイナリ取得用wrapperとして用意する

## Initial command shape

```powershell
whispercli live --out meeting.md --model tiny --lang ja
whispercli live --plain --out meeting.txt
whispercli file audio.wav --out transcript.md
whispercli devices
whispercli models install tiny
```

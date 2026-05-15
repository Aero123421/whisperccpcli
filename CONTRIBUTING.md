# Contributing

Thanks for helping improve whisperCLI.

## Local Checks

Run these before opening a PR:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --no-default-features
npm --prefix npm pack --dry-run
```

On Linux/macOS, also check:

```sh
bash -n scripts/install.sh
```

On Windows, check:

```powershell
$tokens = $null
$errors = $null
$null = [System.Management.Automation.Language.Parser]::ParseFile((Resolve-Path scripts/install.ps1), [ref]$tokens, [ref]$errors)
if ($errors.Count) { $errors | Format-List; exit 1 }
```

## Release Notes

User-facing changes should update `CHANGELOG.md`. Release tags use `vX.Y.Z` and the npm package version should match the Rust package version.

$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $RepoRoot
try {
    cargo test -p fossilsense
    cargo run -p fossilsense -- index samples/mini-c --db target/ci-mini.sqlite --force

    Push-Location extensions/vscode
    try {
        pnpm test
    }
    finally {
        Pop-Location
    }
}
finally {
    Pop-Location
}

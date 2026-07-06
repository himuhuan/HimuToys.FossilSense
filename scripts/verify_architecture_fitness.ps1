param(
    [string]$Root = (Split-Path -Parent $PSScriptRoot),
    [ValidateSet('text', 'json')]
    [string]$Format = 'text',
    [int]$LargeThreshold = 800
)

$ErrorActionPreference = 'Stop'

$Script = Join-Path $PSScriptRoot 'architecture_fitness.js'
node $Script --root $Root --format $Format --large-threshold $LargeThreshold
exit $LASTEXITCODE

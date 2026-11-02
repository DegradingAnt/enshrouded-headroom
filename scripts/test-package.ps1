# WHY NOT RUST: exercises the actual PowerShell packager with an isolated command-level corruption fixture.
#Requires -Version 7.0
param([Parameter(Mandatory)][string]$LoaderZip)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$projectRoot = Split-Path -Parent $PSScriptRoot
$expectedLoaderHash = '3d5f2f2a47f456db878d17c1b135b0eae3209a7d8dabb0ff3f84e1b008c4f3a7'
if ((Get-FileHash -LiteralPath $LoaderZip -Algorithm SHA256).Hash -ne $expectedLoaderHash) {
    throw 'The fixture requires the independently verified loader archive.'
}
$fixture = Join-Path $projectRoot ("dist/package-test-" + [guid]::NewGuid().ToString('N'))
foreach ($relative in @('scripts', 'target/release', 'dist/cache')) {
    New-Item -ItemType Directory -Path (Join-Path $fixture $relative) -Force | Out-Null
}
foreach ($relative in @('scripts/package.ps1', 'Cargo.toml', 'enshrouded-headroom.default.ini', 'LICENSE', 'THIRD_PARTY_NOTICES.md')) {
    Microsoft.PowerShell.Management\Copy-Item -LiteralPath (Join-Path $projectRoot $relative) -Destination (Join-Path $fixture $relative)
}
Microsoft.PowerShell.Management\Copy-Item -LiteralPath $LoaderZip -Destination (Join-Path $fixture 'dist/cache/Ultimate-ASI-Loader_x64-v10.0.0.zip')
[IO.File]::WriteAllBytes((Join-Path $fixture 'target/release/enshrouded-headroom.asi'), [byte[]](1..16))
$version = [regex]::Match((Get-Content -LiteralPath (Join-Path $fixture 'Cargo.toml') -Raw), '(?m)^version\s*=\s*"([^\"]+)"').Groups[1].Value
$finalZip = Join-Path $fixture "dist/enshrouded-headroom-$version.zip"
[IO.File]::WriteAllText($finalZip, 'previous package must survive a refused replacement')
$previousHash = (Get-FileHash -LiteralPath $finalZip -Algorithm SHA256).Hash
$global:packageTestCorruptions = 0
# This is a packaging-only fixture: native compilation is tested separately by the real package run.
function cargo { $global:LASTEXITCODE = 0 }
function Copy-Item {
    param([string]$LiteralPath, [string]$Destination)
    Microsoft.PowerShell.Management\Copy-Item -LiteralPath $LiteralPath -Destination $Destination
    if ($Destination -match '[\\/]stage-[^\\]+[\\/]plugins[\\/]enshrouded-headroom\.asi$') {
        [IO.File]::WriteAllBytes($Destination, [byte[]](16..1))
        $global:packageTestCorruptions++
    }
}
$failure = $null
try { & (Join-Path $fixture 'scripts/package.ps1') }
catch { $failure = $_.Exception.Message }
if ($global:packageTestCorruptions -ne 1) { throw "Expected one staged ASI corruption, got $global:packageTestCorruptions; earlier failure: $failure" }
if ($null -eq $failure -or $failure -notlike '*ASI SHA-256 mismatch*') {
    throw "Expected an ASI hash refusal, got: $failure"
}
if ((Get-FileHash -LiteralPath $finalZip -Algorithm SHA256).Hash -ne $previousHash) {
    throw 'A refused build replaced the previous final package.'
}
Write-Output "PASS: staged ASI corruption refused before publication; previous package preserved."
Write-Output "Refusal: $failure"
Write-Output "Fixture retained: $fixture"

# WHY NOT RUST: release zipping with Compress-Archive and a hash check; no build step needed
#Requires -Version 7.0
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$projectRoot = Split-Path -Parent $PSScriptRoot
$distRoot = Join-Path $projectRoot 'dist'
$cacheRoot = Join-Path $distRoot 'cache'
$loaderZip = Join-Path $cacheRoot 'Ultimate-ASI-Loader_x64-v10.0.0.zip'
$loaderUrl = 'https://github.com/ThirteenAG/Ultimate-ASI-Loader/releases/download/v10.0.0/Ultimate-ASI-Loader_x64.zip'
$loaderSha256 = '3d5f2f2a47f456db878d17c1b135b0eae3209a7d8dabb0ff3f84e1b008c4f3a7'

Push-Location -LiteralPath $projectRoot
try {
    & cargo asi
    if ($LASTEXITCODE -ne 0) {
        throw "cargo asi failed with exit code $LASTEXITCODE."
    }

    $manifest = Get-Content -LiteralPath (Join-Path $projectRoot 'Cargo.toml') -Raw
    $package = [regex]::Match($manifest, '(?ms)^\[package\]\s*\r?\n(?<body>.*?)(?=^\[|\z)')
    $version = [regex]::Match($package.Groups['body'].Value, '(?m)^version\s*=\s*"(?<version>[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)"\s*$')
    if (-not $package.Success -or -not $version.Success) {
        throw 'Could not read the package version from Cargo.toml.'
    }
    $packageZip = Join-Path $distRoot "enshrouded-headroom-$($version.Groups['version'].Value).zip"

    New-Item -ItemType Directory -Path $cacheRoot -Force | Out-Null
    if (-not (Test-Path -LiteralPath $loaderZip)) {
        $partialZip = "$loaderZip.part"
        try {
            Invoke-WebRequest -Uri $loaderUrl -OutFile $partialZip
            $actualHash = (Get-FileHash -LiteralPath $partialZip -Algorithm SHA256).Hash
            if ($actualHash -ne $loaderSha256) {
                throw "Loader SHA-256 mismatch: expected $loaderSha256, got $actualHash. Refusing to package."
            }
            Move-Item -LiteralPath $partialZip -Destination $loaderZip
        }
        finally {
            if (Test-Path -LiteralPath $partialZip) {
                Remove-Item -LiteralPath $partialZip -Force
            }
        }
    }
    $actualHash = (Get-FileHash -LiteralPath $loaderZip -Algorithm SHA256).Hash
    if ($actualHash -ne $loaderSha256) {
        throw "Loader SHA-256 mismatch: expected $loaderSha256, got $actualHash. Refusing to package. Delete $loaderZip and run again."
    }

    Write-Output "Loader ZIP SHA-256 verified: $actualHash"

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $stage = Join-Path $distRoot "stage-$([guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Path (Join-Path $stage 'plugins') | Out-Null
    try {
        $loader = [System.IO.Compression.ZipFile]::OpenRead($loaderZip)
        try {
            $dll = $loader.GetEntry('dinput8.dll')
            if ($null -eq $dll) {
                throw 'The verified loader archive does not contain dinput8.dll.'
            }
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($dll, (Join-Path $stage 'dinput8.dll'))
        }
        finally {
            $loader.Dispose()
        }

        Copy-Item -LiteralPath (Join-Path $projectRoot 'target/release/enshrouded-headroom.asi') -Destination (Join-Path $stage 'plugins/enshrouded-headroom.asi')
        Copy-Item -LiteralPath (Join-Path $projectRoot 'enshrouded-headroom.default.ini') -Destination (Join-Path $stage 'plugins/enshrouded-headroom.ini')
        Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination (Join-Path $stage 'LICENSE')
        Copy-Item -LiteralPath (Join-Path $projectRoot 'THIRD_PARTY_NOTICES.md') -Destination (Join-Path $stage 'THIRD_PARTY_NOTICES.md')

        # Exact text from Ultimate ASI Loader v10.0.0's lowercase source file "license".
        # https://raw.githubusercontent.com/ThirteenAG/Ultimate-ASI-Loader/v10.0.0/license
        $loaderLicense = @"
MIT License

Copyright (c) 2023 ThirteenAG

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"@
        [System.IO.File]::WriteAllText((Join-Path $stage 'ultimate-asi-loader-LICENSE.txt'), ($loaderLicense -replace "`r`n", "`n") + "`n", [System.Text.UTF8Encoding]::new($false))

        $stagedZip = Join-Path $stage 'package.zip'
        $contents = @('dinput8.dll', 'ultimate-asi-loader-LICENSE.txt', 'plugins', 'LICENSE', 'THIRD_PARTY_NOTICES.md') | ForEach-Object { Join-Path $stage $_ }
        Compress-Archive -LiteralPath $contents -DestinationPath $stagedZip
        $archive = [System.IO.Compression.ZipFile]::OpenRead($stagedZip)
        try {
            $expectedEntries = @('LICENSE', 'THIRD_PARTY_NOTICES.md', 'dinput8.dll', 'plugins/enshrouded-headroom.asi', 'plugins/enshrouded-headroom.ini', 'ultimate-asi-loader-LICENSE.txt') | Sort-Object -CaseSensitive
            $actualEntries = @($archive.Entries.FullName | Sort-Object -CaseSensitive)
            if (@(Compare-Object -ReferenceObject $expectedEntries -DifferenceObject $actualEntries -CaseSensitive).Count -ne 0) {
                throw 'Package archive entries do not match the expected six files. Refusing to package.'
            }
            Write-Output 'Zip contents:'
            $archive.Entries | ForEach-Object { Write-Output ("  {0} ({1} bytes)" -f $_.FullName, $_.Length) }
            $asi = $archive.GetEntry('plugins/enshrouded-headroom.asi')
            $stream = $asi.Open()
            $sha256 = [System.Security.Cryptography.SHA256]::Create()
            try {
                $asiHash = [BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '').ToLowerInvariant()
                $sourceAsiHash = (Get-FileHash -LiteralPath (Join-Path $projectRoot 'target/release/enshrouded-headroom.asi') -Algorithm SHA256).Hash.ToLowerInvariant()
                Write-Output "Source ASI SHA-256: $sourceAsiHash"
                Write-Output "Inner ASI SHA-256: $asiHash"
                if ($asiHash -ne $sourceAsiHash) {
                    throw "ASI SHA-256 mismatch: source $sourceAsiHash, archived $asiHash. Refusing to publish package."
                }
                Write-Output 'ASI hash comparison: MATCH'
            }
            finally {
                $sha256.Dispose()
                $stream.Dispose()
            }
        }
        finally {
            $archive.Dispose()
        }
        Move-Item -LiteralPath $stagedZip -Destination $packageZip -Force
        Write-Output "Package: $packageZip"
        Write-Output ("Package ZIP SHA-256: " + (Get-FileHash -LiteralPath $packageZip -Algorithm SHA256).Hash.ToLowerInvariant())
    }
    finally {
        $resolvedDist = (Resolve-Path -LiteralPath $distRoot).ProviderPath.TrimEnd('\') + '\'
        $resolvedStage = (Resolve-Path -LiteralPath $stage).ProviderPath
        if (-not $resolvedStage.StartsWith($resolvedDist, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove staging directory outside dist: $resolvedStage"
        }
        Remove-Item -LiteralPath $resolvedStage -Recurse -Force
    }
}
finally {
    Pop-Location
}

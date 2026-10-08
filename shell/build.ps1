<#
.SYNOPSIS
    Build and package OpenCADStudio for Windows (portable .exe + MSI installer).

.DESCRIPTION
    Local counterpart of the `build-windows` job in .github/workflows/release.yml.
    Runs the same steps on a developer machine:

      1. Work out the package versions the way scripts/release.py does
         (Cargo version 2026.40.1 -> display 2026.40.1, MSI 26.40.1).
      2. Rasterise the SVG icons into .ico files (ImageMagick `magick`).
      3. `cargo build --locked --release` for the app and the Explorer
         thumbnail DLL (crates/dwg-thumbnailer-win).
      4. Optionally Authenticode-sign the .exe (signtool + PFX).
      5. Compile the MSI with WiX 3 (candle/light) from packaging/windows.
      6. Optionally sign the MSI, then copy everything to -OutDir with the
         release file names and a SHA256SUMS.txt.

.PARAMETER Tag
    Release tag such as v2026.41 or v2026.41.1. Defaults to the version in
    Cargo.toml.

.PARAMETER OutDir
    Where the packaged files land. Default: dist\ under the repository root.

.PARAMETER SkipBuild
    Reuse target\release\*.exe / *.dll from a previous build.

.PARAMETER SkipMsi
    Produce only the portable executable (no WiX needed).

.PARAMETER SkipIcons
    Do not regenerate the .ico files (keeps existing ones, or builds without
    an embedded icon when none exist).

.PARAMETER PfxPath
    Code-signing certificate (.pfx). Falls back to the WINDOWS_PFX_BASE64
    environment variable (same name as CI). Without either, the outputs are
    left unsigned.

.PARAMETER PfxPassword
    Password for the certificate, as a string or SecureString. Falls back to
    the WINDOWS_PFX_PASSWORD environment variable.

.PARAMETER Clean
    Run `cargo clean` first.

.EXAMPLE
    pwsh shell\build.ps1
    pwsh shell\build.ps1 -Tag v2026.41 -OutDir C:\releases
    pwsh shell\build.ps1 -SkipMsi -SkipIcons           # quick portable build
    pwsh shell\build.ps1 -PfxPath .\cert.pfx -PfxPassword (Read-Host -AsSecureString)

.NOTES
    Requirements: Rust (cargo), Visual Studio 2017+ or Build Tools for Visual
    Studio with the "Desktop development with C++" workload, and optionally
    ImageMagick 7 (`magick`), WiX Toolset 3.x (`$env:WIX` or
    "C:\Program Files (x86)\WiX Toolset v3.*"), Windows SDK (signtool).
    Works in Windows PowerShell 5.1 and PowerShell 7.
#>
[CmdletBinding()]
param(
    [string]$Tag,
    [string]$OutDir,
    [switch]$SkipBuild,
    [switch]$SkipMsi,
    [switch]$SkipIcons,
    [string]$PfxPath,
    [object]$PfxPassword,
    [switch]$Clean
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ── Helpers ──────────────────────────────────────────────────────────────────

function Write-Step([string]$Message) {
    Write-Host ""
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Invoke-Checked([string]$Description, [scriptblock]$Command) {
    & $Command
    if ($LASTEXITCODE -ne 0) {
        throw "$Description failed with exit code $LASTEXITCODE"
    }
}

function Find-Command([string]$Name) {
    $cmd = Get-Command $Name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    return $null
}

# Mirrors scripts/release.py `versions`: weekly releases are YYYY.WW[.N];
# the MSI ProductVersion must fit 255.255.65535, so the year loses "20".
function Get-PackageVersions([string]$TagValue) {
    $version = $TagValue -replace '^v', ''
    if ($version -match '^(20\d{2})\.(\d{2})(?:\.([1-9]\d*))?$') {
        $year = [int]$Matches[1]
        $week = [int]$Matches[2]
        $patch = if ($Matches[3]) { [int]$Matches[3] } else { 0 }
        if ($week -lt 1 -or $week -gt 53) { throw "Invalid ISO week in $TagValue" }
        return [pscustomobject]@{
            Version = $version
            Cargo   = "$year.$week.$patch"
            Msi     = "$($year - 2000).$week.$patch"
            Tag     = "v$version"
        }
    }
    if ($version -match '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') {
        $major, $minor, $patch = [int]$Matches[1], [int]$Matches[2], [int]$Matches[3]
        if ($major -gt 255 -or $minor -gt 255 -or $patch -gt 65535) {
            throw "Unsupported MSI version: $version"
        }
        return [pscustomobject]@{ Version = $version; Cargo = $version; Msi = $version; Tag = "v$version" }
    }
    throw "Invalid release version: $TagValue (expected vYYYY.WW, vYYYY.WW.N or vX.Y.Z)"
}

# Cargo.toml `version = "2026.40.1"` -> display version 2026.40.1 (or 2026.40
# when the patch is 0), the same mapping build.rs and release.py apply.
function Get-CargoDisplayVersion([string]$CargoToml) {
    $text = Get-Content $CargoToml -Raw
    if ($text -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw "No version in $CargoToml" }
    $cargo = $Matches[1]
    $parts = $cargo.Split('.')
    if ($parts.Length -eq 3 -and $parts[0].Length -eq 4 -and $parts[0].StartsWith('20')) {
        $weekly = "{0}.{1:d2}" -f $parts[0], [int]$parts[1]
        if ($parts[2] -eq '0') { return $weekly }
        return "$weekly.$($parts[2])"
    }
    return $cargo
}

function Find-SignTool {
    $kits = 'C:\Program Files (x86)\Windows Kits\10\bin'
    if (-not (Test-Path $kits)) { return $null }
    Get-ChildItem $kits -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
        Where-Object { $_.DirectoryName -match 'x64' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}

function Find-WixBin {
    if ($env:WIX -and (Test-Path (Join-Path $env:WIX 'bin\candle.exe'))) {
        return (Join-Path $env:WIX 'bin')
    }
    $candidates = Get-ChildItem 'C:\Program Files (x86)' -Directory -Filter 'WiX Toolset v3*' -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending
    foreach ($dir in $candidates) {
        $bin = Join-Path $dir.FullName 'bin'
        if (Test-Path (Join-Path $bin 'candle.exe')) { return $bin }
    }
    $candle = Find-Command 'candle.exe'
    if ($candle) { return (Split-Path $candle) }
    return $null
}

# The MSVC Rust target links with Visual Studio's link.exe. Without the
# "Desktop development with C++" workload (or Build Tools with VC tools),
# cargo fails deep inside the first build script, so check up front.
function Test-MsvcToolchain {
    if (Find-Command 'link.exe') {
        # A Developer Command Prompt / VsDevCmd shell already exposes it.
        $probe = & link.exe 2>&1 | Out-String
        if ($probe -match 'Microsoft \(R\) Incremental Linker') { return $true }
    }
    $vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        $path = & $vswhere -latest -products * `
            -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
            -property installationPath 2>$null
        if ($path) { return $true }
    }
    return $false
}

function Get-PlainPassword([object]$Value) {
    if ($null -eq $Value) { return $null }
    if ($Value -is [securestring]) {
        $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($Value)
        try { return [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr) }
        finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
    }
    return [string]$Value
}

function Invoke-Sign([string]$SignTool, [string]$Pfx, [string]$Password, [string]$File) {
    Write-Host "Signing $File"
    & $SignTool sign /fd SHA256 /tr https://timestamp.digicert.com /td SHA256 /f $Pfx /p $Password $File
    if ($LASTEXITCODE -ne 0) { throw "signtool failed for $File" }
}

# ── Setup ────────────────────────────────────────────────────────────────────

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
$pfxFile = $null
Push-Location $repoRoot
try {
    # $IsWindows only exists in PowerShell 7+; Windows PowerShell 5.1 is Windows.
    if ((Test-Path variable:IsWindows) -and -not $IsWindows) {
        throw "This script packages the Windows build and must run on Windows."
    }

    if (-not $OutDir) { $OutDir = Join-Path $repoRoot 'dist' }
    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

    Write-Step "Versions"
    if (-not $Tag) { $Tag = Get-CargoDisplayVersion (Join-Path $repoRoot 'Cargo.toml') }
    $versions = Get-PackageVersions $Tag
    $cargoVersion = (Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value
    if ($cargoVersion -ne $versions.Cargo) {
        Write-Warning "Cargo.toml version ($cargoVersion) differs from the tag's Cargo version ($($versions.Cargo)); the executable will report $cargoVersion."
    }
    Write-Host ("Release {0}  Cargo {1}  MSI {2}" -f $versions.Tag, $versions.Cargo, $versions.Msi)

    $cargo = Find-Command 'cargo'
    if (-not $cargo) { throw "cargo not found. Install Rust from https://rustup.rs and reopen the shell." }
    if (-not $SkipBuild -and -not (Test-MsvcToolchain)) {
        throw @"
Visual C++ build tools not found (cargo needs link.exe from Visual Studio).
Install the "Desktop development with C++" workload, for example:

  winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

or download Build Tools for Visual Studio from https://visualstudio.microsoft.com/visual-cpp-build-tools/
and tick "Desktop development with C++" (this also installs the Windows SDK that provides signtool).
Then open a new PowerShell window and run this script again.
"@
    }

    # ── Icons ────────────────────────────────────────────────────────────────
    $winDir = Join-Path $repoRoot 'packaging\windows'
    $appIcon = Join-Path $winDir 'AppIcon.ico'
    $icons = @(
        @{ Svg = 'assets\logo.svg';                       Ico = $appIcon },
        @{ Svg = 'assets\mimetypes\image-vnd.dwg.svg';    Ico = (Join-Path $winDir 'dwg.ico') },
        @{ Svg = 'assets\mimetypes\image-vnd.dxf.svg';    Ico = (Join-Path $winDir 'dxf.ico') }
    )
    if (-not $SkipIcons) {
        Write-Step "Icons"
        $magick = Find-Command 'magick'
        if ($magick) {
            foreach ($icon in $icons) {
                Write-Host "$($icon.Svg) -> $($icon.Ico)"
                Invoke-Checked "magick $($icon.Svg)" {
                    & $magick $icon.Svg -define icon:auto-resize=16,24,32,48,64,128,256 $icon.Ico
                }
            }
        } else {
            $missing = $icons | Where-Object { -not (Test-Path $_.Ico) } | ForEach-Object { $_.Ico }
            if ($missing) {
                Write-Warning "ImageMagick (magick) not found; icons not generated: $($missing -join ', '). The executable gets no embedded icon and the MSI step needs dwg.ico/dxf.ico."
            } else {
                Write-Host "ImageMagick not found; keeping the existing .ico files."
            }
        }
    }

    # ── Build ────────────────────────────────────────────────────────────────
    if ($Clean) {
        Write-Step "cargo clean"
        Invoke-Checked "cargo clean" { & $cargo clean }
    }
    $exePath = Join-Path $repoRoot 'target\release\OpenCADStudio.exe'
    $dllPath = Join-Path $repoRoot 'target\release\dwg_thumbnailer_win.dll'
    if (-not $SkipBuild) {
        Write-Step "cargo build --release"
        Invoke-Checked "cargo build (app)" { & $cargo build --locked --release }
        Invoke-Checked "cargo build (thumbnailer)" { & $cargo build --locked --release -p dwg-thumbnailer-win }
    }
    foreach ($artifact in @($exePath, $dllPath)) {
        if (-not (Test-Path $artifact)) { throw "Missing build output: $artifact (run without -SkipBuild)" }
    }

    # ── Signing credentials ─────────────────────────────────────────────────
    $signTool = $null
    $pfxFile = $null
    $pfxPassword = Get-PlainPassword $PfxPassword
    if ($PfxPath) {
        $pfxFile = (Resolve-Path $PfxPath).Path
    } elseif ($env:WINDOWS_PFX_BASE64) {
        $pfxFile = Join-Path ([IO.Path]::GetTempPath()) "ocs-signing-$PID.pfx"
        [IO.File]::WriteAllBytes($pfxFile, [Convert]::FromBase64String($env:WINDOWS_PFX_BASE64))
        if (-not $pfxPassword) { $pfxPassword = $env:WINDOWS_PFX_PASSWORD }
    }
    if ($pfxFile) {
        $signTool = Find-SignTool
        if (-not $signTool) { throw "signtool.exe not found in the Windows SDK; install the SDK or drop the signing options." }
        Write-Step "Sign executable"
        Invoke-Sign $signTool $pfxFile $pfxPassword $exePath
    } else {
        Write-Host "No signing certificate (-PfxPath or WINDOWS_PFX_BASE64); outputs stay unsigned."
    }

    # ── MSI ──────────────────────────────────────────────────────────────────
    $msiPath = Join-Path $repoRoot 'OpenCADStudio.msi'
    $msiBuilt = $false
    if (-not $SkipMsi) {
        Write-Step "MSI installer (WiX 3)"
        $wixBin = Find-WixBin
        if (-not $wixBin) {
            Write-Warning "WiX Toolset 3 not found (set `$env:WIX or install from https://wixtoolset.org/). Skipping the MSI; the portable executable is still packaged."
        } else {
            foreach ($required in @($appIcon, (Join-Path $winDir 'dwg.ico'), (Join-Path $winDir 'dxf.ico'))) {
                if (-not (Test-Path $required)) { throw "MSI needs $required; run without -SkipIcons with ImageMagick installed." }
            }
            $candle = Join-Path $wixBin 'candle.exe'
            $light = Join-Path $wixBin 'light.exe'
            $objDir = "$winDir\"
            Invoke-Checked "candle" {
                & $candle -arch x64 `
                    "-dVersion=$($versions.Msi)" `
                    "-dSource=$exePath" `
                    "-dIcon=$appIcon" `
                    "-dLicense=$(Join-Path $winDir 'License.rtf')" `
                    (Join-Path $winDir 'main.wxs') (Join-Path $winDir 'ui.wxs') `
                    -out $objDir
            }
            Invoke-Checked "light" {
                & $light (Join-Path $winDir 'main.wixobj') (Join-Path $winDir 'ui.wixobj') `
                    -ext WixUIExtension -cultures:en-us `
                    -loc (Join-Path $winDir 'strings.en-US.wxl') `
                    -out $msiPath
            }
            Remove-Item (Join-Path $winDir '*.wixobj'), (Join-Path $winDir '*.wixpdb'), (Join-Path $repoRoot '*.wixpdb') -ErrorAction SilentlyContinue
            $msiBuilt = $true
            if ($signTool) {
                Write-Step "Sign MSI"
                Invoke-Sign $signTool $pfxFile $pfxPassword $msiPath
            }
        }
    }

    # ── Collect outputs ──────────────────────────────────────────────────────
    Write-Step "Package"
    $tagName = $versions.Tag
    $portable = Join-Path $OutDir "OpenCADStudio-$tagName-windows-x86_64-portable.exe"
    Copy-Item $exePath $portable -Force
    $outputs = @($portable)
    if ($msiBuilt) {
        $installer = Join-Path $OutDir "OpenCADStudio-$tagName-windows-x86_64-installer.msi"
        Move-Item $msiPath $installer -Force
        $outputs += $installer
    }
    $sums = Join-Path $OutDir 'SHA256SUMS.txt'
    $outputs | ForEach-Object {
        $hash = (Get-FileHash $_ -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $(Split-Path $_ -Leaf)"
    } | Set-Content -Path $sums -Encoding ascii
    $outputs += $sums

    Write-Host ""
    Write-Host "Done. Outputs in $OutDir" -ForegroundColor Green
    foreach ($file in $outputs) {
        $size = (Get-Item $file).Length
        Write-Host ("  {0,-60} {1,10:n0} bytes" -f (Split-Path $file -Leaf), $size)
    }
}
finally {
    if ($pfxFile -and $env:WINDOWS_PFX_BASE64 -and -not $PfxPath -and (Test-Path $pfxFile)) {
        Remove-Item $pfxFile -Force -ErrorAction SilentlyContinue
    }
    Pop-Location
}

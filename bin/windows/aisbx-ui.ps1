#!/usr/bin/env pwsh
# aisbx-ui - launcher for the aisbx desktop GUI (Windows), run by aisbx-ui.cmd: aisbx.ps1 for
# gui\ and -p aisbx-ui. aisbx.ps1 explains each step and the environment (AISBX_REBUILD,
# AISBX_SKIP_BUILD, AISBX_HOME); keep the two and bin/unix/aisbx-ui in step.

$ErrorActionPreference = 'Stop'

function Test-Truthy([string] $Value) {
    return -not ([string]::IsNullOrEmpty($Value) -or
        @('0', 'no', 'off', 'false') -contains $Value.ToLowerInvariant())
}

function Write-Hint([string] $Text) {
    [Console]::Error.WriteLine($Text)
}

$self = $PSCommandPath
for ($hop = 0; $hop -lt 10; $hop++) {
    $item = Get-Item -LiteralPath $self -Force
    $target = $null
    if ($item.PSObject.Properties['LinkTarget']) { $target = $item.LinkTarget }
    elseif ($item.PSObject.Properties['Target']) { $target = @($item.Target)[0] }
    if ([string]::IsNullOrEmpty($target)) { break }
    if (-not [System.IO.Path]::IsPathRooted($target)) {
        $target = Join-Path (Split-Path -Parent $self) $target
    }
    $self = $target
}

$here = Split-Path -Parent $self                          # <repo>\bin\windows
$root = Split-Path -Parent (Split-Path -Parent $here)
$crate = Join-Path $root 'gui'
$core = Join-Path $root 'core'
$buildDir = Join-Path $here 'build'
# $IsWindows is undefined on Windows PowerShell 5.1, which only ever runs on Windows.
$exeName = if ($false -eq $IsWindows) { 'aisbx-ui' } else { 'aisbx-ui.exe' }
$bin = Join-Path $buildDir $exeName

if (-not (Test-Path -LiteralPath (Join-Path $root 'Cargo.toml')) -or
    -not (Test-Path -LiteralPath (Join-Path $crate 'Cargo.toml')) -or
    -not (Test-Path -LiteralPath (Join-Path $core 'Cargo.toml')) -or
    -not (Test-Path -LiteralPath (Join-Path $root 'kits'))) {
    Write-Hint "aisbx-ui: $root does not look like the aisbx checkout - expected"
    Write-Hint "       $root\Cargo.toml, $crate\Cargo.toml, $core\Cargo.toml and $root\kits to exist."
    Write-Hint "       Keep this script in the repository's bin\windows\ directory and put that"
    Write-Hint "       directory on your PATH rather than copying the script somewhere else."
    exit 1
}

$argv = @($args)
$rebuild = Test-Truthy $env:AISBX_REBUILD
$skipBuild = Test-Truthy $env:AISBX_SKIP_BUILD
if ($argv.Count -gt 0 -and $argv[0] -eq '--rebuild') {
    $rebuild = $true
    $argv = @($argv | Select-Object -Skip 1)
}

$missing = $false

if (-not (Get-Command sbx -ErrorAction SilentlyContinue)) {
    $missing = $true
    Write-Hint 'aisbx-ui: the sbx CLI (Docker Sandboxes) is not on your PATH. The dashboard runs every'
    Write-Hint '       operation through it, so it cannot start without it. Download a binary from'
    Write-Hint '       https://github.com/docker/sbx-releases/releases and put its folder on your'
    Write-Hint '       PATH - permanently under Settings > System > About > Advanced system'
    Write-Hint '       settings > Environment Variables, or for this session only:'
    Write-Hint ''
    Write-Hint '           $env:Path = "C:\path\to\sbx;$env:Path"'
    Write-Hint ''
    Write-Hint '       Then sign in once with `sbx login`.'
    Write-Hint ''
}

if ($skipBuild) {
    if (-not (Test-Path -LiteralPath $bin)) {
        $missing = $true
        Write-Hint 'aisbx-ui: AISBX_SKIP_BUILD is set, but there is no binary at'
        Write-Hint "       $bin."
        Write-Hint '       Clear it and run aisbx-ui again to build one:  $env:AISBX_SKIP_BUILD = $null'
        Write-Hint ''
    }
}
elseif (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $missing = $true
    $homeDir = if ($env:USERPROFILE) { $env:USERPROFILE } else { $HOME }
    $cargoBin = Join-Path (Join-Path $homeDir '.cargo') 'bin'
    if (Test-Path -LiteralPath (Join-Path $cargoBin 'cargo.exe')) {
        Write-Hint 'aisbx-ui: cargo is installed, but its folder is not on your PATH:'
        Write-Hint ''
        Write-Hint "           $cargoBin"
        Write-Hint ''
        Write-Hint '       Add it permanently under Settings > System > About > Advanced system'
        Write-Hint '       settings > Environment Variables, or for this session only:'
        Write-Hint ''
        Write-Hint "           `$env:Path = `"$cargoBin;`$env:Path`""
        Write-Hint ''
    }
    else {
        Write-Hint 'aisbx-ui: cargo (the Rust toolchain) is not on your PATH. It builds the dashboard'
        Write-Hint '       from core\ and gui\. Install it with rustup:'
        Write-Hint ''
        Write-Hint '           winget install Rustlang.Rustup'
        Write-Hint ''
        Write-Hint '       (or download it from https://rustup.rs), then open a new terminal so the'
        Write-Hint '       PATH entry it adds takes effect.'
        Write-Hint ''
        Write-Hint '       If you already have a built binary in bin\windows\build\, set'
        Write-Hint '       $env:AISBX_SKIP_BUILD = 1 to run it without cargo.'
        Write-Hint ''
    }
}

if ($missing) { exit 1 }

# Pruned at every depth, as find does in bin/unix/aisbx-ui: target\ folders and dotfiles.
function Get-Sources([string] $Dir) {
    Get-ChildItem -LiteralPath $Dir -Force -ErrorAction SilentlyContinue |
        Where-Object { -not $_.Name.StartsWith('.') -and -not ($_.PSIsContainer -and $_.Name -eq 'target') } |
        ForEach-Object { if ($_.PSIsContainer) { Get-Sources $_.FullName } else { $_ } }
}

function Test-Stale {
    if (-not (Test-Path -LiteralPath $bin)) { return $true }
    $builtAt = (Get-Item -LiteralPath $bin -Force).LastWriteTimeUtc
    $newer = @(Get-Sources $core) + @(Get-Sources $crate) +
        @(@('Cargo.toml', 'Cargo.lock') | ForEach-Object { Join-Path $root $_ } |
            Where-Object { Test-Path -LiteralPath $_ } |
            ForEach-Object { Get-Item -LiteralPath $_ -Force }) |
        Where-Object { $_.LastWriteTimeUtc -gt $builtAt } |
        Select-Object -First 1
    return $null -ne $newer
}

if (-not $skipBuild -and ($rebuild -or (Test-Stale))) {
    Write-Hint 'aisbx-ui: building the dashboard (cargo build --release)...'
    New-Item -ItemType Directory -Force -Path $buildDir | Out-Null
    & cargo build --release `
        --manifest-path (Join-Path $root 'Cargo.toml') `
        -p aisbx-ui `
        --target-dir (Join-Path $root 'target')
    if ($LASTEXITCODE -ne 0) {
        Write-Hint ''
        Write-Hint 'aisbx-ui: the build failed - see the cargo output above.'
        exit 1
    }
    # Copy next to the binary and rename over it, so a stale binary is never half-written.
    $staged = "$bin.new"
    $built = Join-Path (Join-Path (Join-Path $root 'target') 'release') $exeName
    Copy-Item -LiteralPath $built -Destination $staged -Force
    try {
        Move-Item -LiteralPath $staged -Destination $bin -Force
    }
    catch {
        Write-Hint "aisbx-ui: could not replace $bin - close any other running aisbx-ui and try again."
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        exit 1
    }
    Write-Hint "aisbx-ui: built $bin"
}

if ([string]::IsNullOrEmpty($env:AISBX_HOME)) { $env:AISBX_HOME = $root }

& $bin @argv
if ($null -eq $LASTEXITCODE) { exit 0 }
exit $LASTEXITCODE

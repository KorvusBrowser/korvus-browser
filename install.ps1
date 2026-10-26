# korvus installer for Windows: downloads korvus for this machine from the latest release, checks it against the
# release's SHA256SUMS, unpacks it under ~\.korvus, puts it on your PATH, and runs `korvus setup`.
#
#   irm https://korvusbrowser.com/install.ps1 | iex
#
# (korvusbrowser.com hands out the latest release's copy of this file, which is also each release's install.ps1.)
#
# Run it again to update. Each release unpacks into a directory of its own (~\.korvus\dist\<version>) and
# ~\.korvus\current points at the newest. korvus keeps its configuration, license and named personas in ~\.korvus too.
#
# Environment: KORVUS_HOME, KORVUS_VERSION (vX.Y.Z), KORVUS_RELEASE_BASE (a directory of a release's assets; file://
# for a local one), KORVUS_NO_MODIFY_PATH=1, KORVUS_NO_PROMPT=1, CI. `$env:KORVUS_INSTALL_LIB = '1'; . ./install.ps1`
# defines the functions without installing anything.
$ErrorActionPreference = 'Stop'
$ReleasesUrl = 'https://github.com/KorvusBrowser/korvus-browser/releases'

function script:ReleaseBase {
  if ($env:KORVUS_RELEASE_BASE) { return $env:KORVUS_RELEASE_BASE.TrimEnd('/') }
  if ($env:KORVUS_VERSION) { return "$ReleasesUrl/download/$($env:KORVUS_VERSION)" }
  return "$ReleasesUrl/latest/download"
}

function script:Download([string]$url, [string]$dest) {
  if ($url -like 'file:*') {
    Copy-Item -LiteralPath ([Uri]$url).LocalPath -Destination $dest
    return
  }
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
  Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $dest
}

function script:SumsLine([string]$sums, [string]$name) {
  foreach ($line in $sums -split "`n") {
    $parts = $line.Trim() -split '\s+', 2
    if ($parts.Count -eq 2 -and ($parts[1].TrimStart('*')) -eq $name) { return $parts[0].ToLower() }
  }
  return $null
}

# The version the korvus binary $exe says it is: `korvus --version` (152.0.2 on), or the first line of the banner older
# releases print for it; KORVUS_VERSION's when it says neither.
function script:VersionOf([string]$exe) {
  $prev = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'  # an older release exits non-zero after its banner; that is not an error here
  try { $said = @(& $exe --version 2>$null) } catch { $said = @() } finally { $ErrorActionPreference = $prev }
  foreach ($line in $said) {
    if ("$line" -match '^korvus (\d+\.\d+\.\d+)') { return $Matches[1] }
  }
  if ($env:KORVUS_VERSION) { return $env:KORVUS_VERSION.TrimStart('v') }
  return $null
}

function Get-KorvusPlatform {
  switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { return 'windows-x86_64' }
    'ARM64' { return 'windows-x86_64' }  # no native arm64 build yet; Windows 11 on ARM runs the x86-64 one
    default { throw "korvus: no build for Windows on $($env:PROCESSOR_ARCHITECTURE)" }
  }
}

function Install-Korvus {
  $platform = Get-KorvusPlatform
  $asset = "korvus-$platform.zip"
  $base = script:ReleaseBase
  $root = if ($env:KORVUS_HOME) { $env:KORVUS_HOME } else { Join-Path $env:USERPROFILE '.korvus' }
  $work = Join-Path ([IO.Path]::GetTempPath()) ("korvus-" + [Guid]::NewGuid())
  New-Item -ItemType Directory -Path $work | Out-Null
  try {
    script:Download "$base/SHA256SUMS" "$work\SHA256SUMS"
    script:Download "$base/$asset" "$work\$asset"
    $want = script:SumsLine ([IO.File]::ReadAllText("$work\SHA256SUMS")) $asset
    if (-not $want -or (Get-FileHash -Algorithm SHA256 -LiteralPath "$work\$asset").Hash.ToLower() -ne $want) {
      throw "korvus: $asset does not match the release's SHA256SUMS"
    }
    Expand-Archive -LiteralPath "$work\$asset" -DestinationPath "$work\x"
    # The archive holds korvus.exe at its root (152.0.0 on), or in one folder named korvus-<version>-<platform> (before).
    if (Test-Path -LiteralPath "$work\x\korvus.exe") {
      $src = "$work\x"
      $version = script:VersionOf "$src\korvus.exe"
    } else {
      $top = @(Get-ChildItem -LiteralPath "$work\x" -Directory)
      if ($top.Count -ne 1 -or -not (Test-Path -LiteralPath (Join-Path $top[0].FullName 'korvus.exe'))) {
        throw "korvus: $asset holds no korvus.exe"
      }
      $src = $top[0].FullName
      $version = $top[0].Name -replace '^korvus-', '' -replace "-$platform$", ''
    }
    if (-not $version) { throw "korvus: cannot tell which version $asset is: set KORVUS_VERSION=vX.Y.Z" }
    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force -Path $dist | Out-Null
    $dir = Join-Path $dist $version
    if (Test-Path -LiteralPath $dir) {
      Write-Host "korvus: $version is installed already: $dir"
    } else {
      Move-Item -LiteralPath $src -Destination $dir
    }
    # ~\.korvus\current: a junction to the newest release (a running `korvus serve` runs its own release's files)
    $current = Join-Path $root 'current'
    if (Test-Path -LiteralPath $current) { [IO.Directory]::Delete($current, $false) }
    New-Item -ItemType Junction -Path $current -Target $dir | Out-Null
    $exe = Join-Path $current 'korvus.exe'
    Write-Host "korvus: installed $version`: $exe"

    if ($env:KORVUS_NO_MODIFY_PATH -ne '1') {
      $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
      $entries = @()
      if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ -ne '' } }
      if ($entries -notcontains $current) {
        [Environment]::SetEnvironmentVariable('Path', ((@($entries) + $current) -join ';'), 'User')
      }
      if (($env:Path -split ';') -notcontains $current) { $env:Path = "$($env:Path);$current" }
    }

    $config = if ($env:KORVUS_CONFIG) { $env:KORVUS_CONFIG } else { Join-Path $root 'config.toml' }
    if (Test-Path -LiteralPath $config) {
      Write-Host "korvus: keeping your settings ($config); ``korvus setup`` changes them"
    } else {
      $interactive = (-not [Console]::IsInputRedirected) -and (-not [Console]::IsOutputRedirected) `
        -and (-not $env:CI) -and ($env:KORVUS_NO_PROMPT -ne '1')
      if ($interactive) { & $exe setup } else { & $exe setup --non-interactive }
      if ($LASTEXITCODE -ne 0) { Write-Host 'korvus is installed; run "korvus setup" to finish' }
    }
    Write-Host 'Next: "korvus serve" (a DevTools endpoint on :9222). Docs: https://korvusbrowser.com/docs'
  } finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $work
  }
}

if (-not $env:KORVUS_INSTALL_LIB) { Install-Korvus }

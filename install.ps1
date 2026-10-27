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

# What the installer says, one line a step: a step done (a green check), under way (a cyan arrow), a warning; each a
# label and what it says. Colours are the console's; a log keeps the words.
function script:Say([string]$mark, [ConsoleColor]$color, [string]$label, [string]$text) {
  Write-Host '  ' -NoNewline
  Write-Host $mark -ForegroundColor $color -NoNewline
  Write-Host (' {0,-10} {1}' -f $label, $text)
}
function script:Ok([string]$label, [string]$text) { script:Say ([string][char]0x2713) Green $label $text }
function script:Step([string]$label, [string]$text) { script:Say ([string][char]0x2192) Cyan $label $text }
function script:Warn([string]$text) {
  Write-Host '  ' -NoNewline
  Write-Host '!' -ForegroundColor Yellow -NoNewline
  Write-Host " $text"
}

# A path as a person reads it: ~\... under the user's profile.
function script:Tilde([string]$path) {
  if ($env:USERPROFILE -and $path.StartsWith($env:USERPROFILE, [StringComparison]::OrdinalIgnoreCase)) {
    return '~' + $path.Substring($env:USERPROFILE.Length)
  }
  return $path
}

# The URL a HEAD request ends at after its redirects, and the size it reports; $null when it cannot be had.
function script:Head([string]$url) {
  try {
    $r = Invoke-WebRequest -UseBasicParsing -Method Head -Uri $url -TimeoutSec 10
    $final = if ($r.BaseResponse.ResponseUri) { $r.BaseResponse.ResponseUri.AbsoluteUri } else { $r.BaseResponse.RequestMessage.RequestUri.AbsoluteUri }
    $length = [long]0
    [void][long]::TryParse((@($r.Headers['Content-Length']) | Select-Object -First 1), [ref]$length)
    return @{ Url = $final; Length = $length }
  } catch { return $null }
}

# The release the installer fetches, as the log names it: KORVUS_VERSION, the latest's tag (GitHub's /latest
# redirects to /tag/vX.Y.Z), or the address KORVUS_RELEASE_BASE names.
function script:ReleaseName {
  if ($env:KORVUS_RELEASE_BASE) { return $env:KORVUS_RELEASE_BASE.TrimEnd('/') }
  if ($env:KORVUS_VERSION) { return $env:KORVUS_VERSION }
  $h = script:Head "$ReleasesUrl/latest"
  if ($h -and $h.Url -match '/tag/(v[^/]+)$') { return "$($Matches[1]) (the latest)" }
  return 'the latest'
}

function script:ReleaseBase {
  if ($env:KORVUS_RELEASE_BASE) { return $env:KORVUS_RELEASE_BASE.TrimEnd('/') }
  if ($env:KORVUS_VERSION) { return "$ReleasesUrl/download/$($env:KORVUS_VERSION)" }
  return "$ReleasesUrl/latest/download"
}

# Download $url to $dest; with curl.exe's progress bar when $progress and Windows has curl.exe (10 and later), else
# quietly: Windows PowerShell's own progress display slows a download many times over.
function script:Download([string]$url, [string]$dest, [switch]$progress) {
  if ($url -like 'file:*') {
    Copy-Item -LiteralPath ([Uri]$url).LocalPath -Destination $dest
    return
  }
  $curl = Get-Command curl.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($progress -and $curl -and -not [Console]::IsErrorRedirected) {
    & $curl.Source -fL --retry 2 --progress-bar -o $dest $url
    if ($LASTEXITCODE -ne 0) { throw "korvus: could not download $url" }
    return
  }
  [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
  $prev = $ProgressPreference
  $ProgressPreference = 'SilentlyContinue'
  try { Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $dest } finally { $ProgressPreference = $prev }
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
  Write-Host ''
  Write-Host 'korvus' -ForegroundColor White -NoNewline
  Write-Host ' installer'
  Write-Host ''
  $platform = Get-KorvusPlatform
  script:Ok 'Platform' $platform
  $asset = "korvus-$platform.zip"
  $base = script:ReleaseBase
  script:Ok 'Release' (script:ReleaseName)
  $root = if ($env:KORVUS_HOME) { $env:KORVUS_HOME } else { Join-Path $env:USERPROFILE '.korvus' }
  $work = Join-Path ([IO.Path]::GetTempPath()) ("korvus-" + [Guid]::NewGuid())
  New-Item -ItemType Directory -Path $work | Out-Null
  try {
    script:Download "$base/SHA256SUMS" "$work\SHA256SUMS"
    $size = ''
    if ($base -notlike 'file:*') {
      $h = script:Head "$base/$asset"
      if ($h -and $h.Length -gt 0) { $size = ' ({0} MB)' -f [math]::Round($h.Length / 1MB) }
    }
    script:Step 'Download' "$asset$size"
    script:Download "$base/$asset" "$work\$asset" -progress
    $want = script:SumsLine ([IO.File]::ReadAllText("$work\SHA256SUMS")) $asset
    if (-not $want -or (Get-FileHash -Algorithm SHA256 -LiteralPath "$work\$asset").Hash.ToLower() -ne $want) {
      throw "korvus: $asset does not match the release's SHA256SUMS"
    }
    script:Ok 'Verified' "SHA-256 matches the release's SHA256SUMS"
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
      script:Ok 'Installed' "korvus $version was already in $(script:Tilde $dir)"
    } else {
      Move-Item -LiteralPath $src -Destination $dir
      script:Ok 'Installed' "korvus $version in $(script:Tilde $dir)"
    }
    # ~\.korvus\current: a junction to the newest release (a running `korvus serve` runs its own release's files)
    $current = Join-Path $root 'current'
    if (Test-Path -LiteralPath $current) { [IO.Directory]::Delete($current, $false) }
    New-Item -ItemType Junction -Path $current -Target $dir | Out-Null
    $exe = Join-Path $current 'korvus.exe'
    script:Ok 'Linked' "$(script:Tilde $current) -> $version"

    if ($env:KORVUS_NO_MODIFY_PATH -ne '1') {
      $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
      $entries = @()
      if ($userPath) { $entries = $userPath -split ';' | Where-Object { $_ -ne '' } }
      if ($entries -notcontains $current) {
        [Environment]::SetEnvironmentVariable('Path', ((@($entries) + $current) -join ';'), 'User')
        script:Ok 'PATH' "added to your user PATH"
      } else {
        script:Ok 'PATH' "already on your user PATH"
      }
      if (($env:Path -split ';') -notcontains $current) { $env:Path = "$($env:Path);$current" }
    } else {
      script:Ok 'PATH' 'left as it is (KORVUS_NO_MODIFY_PATH)'
    }

    $config = if ($env:KORVUS_CONFIG) { $env:KORVUS_CONFIG } else { Join-Path $root 'config.toml' }
    if (Test-Path -LiteralPath $config) {
      script:Ok 'Settings' "kept ($(script:Tilde $config)); ``korvus setup`` changes them"
    } else {
      $interactive = (-not [Console]::IsInputRedirected) -and (-not [Console]::IsOutputRedirected) `
        -and (-not $env:CI) -and ($env:KORVUS_NO_PROMPT -ne '1')
      if ($interactive) {
        script:Step 'Setup' "two questions, written to $(script:Tilde $config)"
        & $exe setup
      } else { & $exe setup --non-interactive }
      if ($LASTEXITCODE -ne 0) { script:Warn 'korvus is installed; run "korvus setup" to finish' }
    }
    Write-Host ''
    Write-Host "korvus $version is ready." -ForegroundColor Green
    Write-Host '  Open a new terminal, then:  ' -NoNewline
    Write-Host 'korvus serve' -ForegroundColor Cyan -NoNewline
    Write-Host '   (a DevTools endpoint on :9222)'
    Write-Host '  Later: ' -NoNewline
    Write-Host 'korvus update' -ForegroundColor Cyan -NoNewline
    Write-Host '  (korvus also asks when a release is out)'
    Write-Host '  Docs:  https://korvusbrowser.com/docs'
    Write-Host ''
  } finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $work
  }
}

if (-not $env:KORVUS_INSTALL_LIB) { Install-Korvus }

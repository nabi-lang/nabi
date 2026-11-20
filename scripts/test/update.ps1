# Update an older install to a staged release set on
# Windows: the archives dist/native.sh left in -Dist, served over http
# in the GitHub releases' layout. The counterpart of test/update.sh; three
# updates, each on a fresh NABI_HOME holding the older release:
#
#   - its `nabi toolchain update`, then the staged nabi builds and runs a
#     project the older nabi made and locked (a git dependency), and
#     leaves its nabi.lock in lock format 1;
#   - its install.ps1 -Version <staged> from the staged set;
#   - the staged archive's own install.ps1 -Version <staged>.
#
#   pwsh scripts/test/update.ps1 -Dist <dist>
#
# NABI_UPDATE_FROM names the older release (required), downloaded from
# GitHub; NABI_UPDATE_PORT the local port (default 8765).
param([Parameter(Mandatory)][string]$Dist)
$ErrorActionPreference = 'Stop'

$Dist = (Resolve-Path $Dist).Path
$v = (Get-Content -Raw (Join-Path $Dist 'nabi-version')).Trim()
$from = $env:NABI_UPDATE_FROM
if (-not $from) { throw 'set NABI_UPDATE_FROM to the release to update from' }
$port = if ($env:NABI_UPDATE_PORT) { $env:NABI_UPDATE_PORT } else { '8765' }
$gh = 'https://github.com/nabi-lang/nabi'
$triple = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
$name = "nabi-$v-$triple"
if (-not (Test-Path (Join-Path $Dist "$name.zip"))) { throw "no $name.zip in $Dist" }
# `nabi toolchain update` takes the same version as up to date.
if ($v -eq $from) { throw "the staged set is $v, the release to update from; set NABI_UPDATE_FROM to an older one" }
$p = $v.Split('.')
$series = if ($p[0] -eq '0') { "0.$($p[1])" } else { $p[0] }

function Step([string]$Text) { Write-Output "`n== $Text" }
# Run a native command; fail on a non-zero exit.
function Must {
    $exe = $args[0]
    $rest = @($args | Select-Object -Skip 1)
    & $exe @rest
    if ($LASTEXITCODE) { throw "$args failed ($LASTEXITCODE)" }
}
# Run install script $Script with $Params; fail when it does.
function Install([string]$Script, [hashtable]$Params) {
    $global:LASTEXITCODE = 0
    & $Script @Params -NoModifyPath -Quiet
    if ($LASTEXITCODE) { throw "$Script failed ($LASTEXITCODE)" }
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) "nabi-test-update-$([Guid]::NewGuid())"
New-Item -ItemType Directory $tmp | Out-Null
$env:NABI_NO_MODIFY_PATH = '1'
$env:NO_COLOR = '1'
Remove-Item Env:NABI_TOOLCHAIN -ErrorAction SilentlyContinue
$server = $null
try {
    Step "nabi $from ($triple) from $gh"
    $base = Join-Path $tmp 'base'
    New-Item -ItemType Directory $base | Out-Null
    $oldPs1 = Join-Path $base 'install.ps1'
    Invoke-WebRequest -UseBasicParsing -OutFile $oldPs1 "https://raw.githubusercontent.com/nabi-lang/nabi/v$from/install.ps1"
    $oldZip = Join-Path $base "nabi-$from-$triple.zip"
    Invoke-WebRequest -UseBasicParsing -OutFile $oldZip "$gh/releases/download/v$from/nabi-$from-$triple.zip"

    # The proxies in $NabiHome run nabi $Want by default, and the list has it.
    function CheckDefault([string]$NabiHome, [string]$Want) {
        $env:NABI_HOME = $NabiHome
        Push-Location $tmp
        try {
            $got = (& "$NabiHome\bin\nabic.exe" --version | Select-Object -First 1)
            if ($got -ne "nabic $Want" -and -not $got.StartsWith("nabic $Want ")) {
                throw "$NabiHome\bin\nabic.exe --version says '$got', not nabic $Want"
            }
            $list = (& "$NabiHome\bin\nabi.exe" toolchain list) -join "`n"
            if (-not $list.Contains("$Want-$triple (default)")) { throw "${NabiHome}: $Want-$triple is not the default: $list" }
        } finally { Pop-Location }
    }
    # A NABI_HOME with nabi $from as the default, installed by its install.ps1.
    function InstallFrom([string]$NabiHome) {
        $env:NABI_HOME = $NabiHome
        Install $oldPs1 @{ Archive = $oldZip }
        CheckDefault $NabiHome $from
    }

    Step "staged nabi $v on http://127.0.0.1:$port"
    $serve = Join-Path $tmp 'serve'
    $rel = Join-Path $serve "download\v$v"
    New-Item -ItemType Directory $rel, (Join-Path $serve 'latest\download') | Out-Null
    Copy-Item (Join-Path $Dist "$name.zip"), (Join-Path $Dist "$name.zip.sha256") $rel
    Copy-Item (Join-Path $Dist 'nabi-version') (Join-Path $serve 'latest\download')
    # Signed with a throwaway key the update and the new install.ps1 trust
    # with --key / -Key.
    if (-not (Get-Command minisign -ErrorAction SilentlyContinue)) { throw 'needs minisign (CI: .github/actions/setup-minisign)' }
    $hash = (Get-FileHash -Algorithm SHA256 (Join-Path $rel "$name.zip")).Hash.ToLower()
    Set-Content -NoNewline -Path (Join-Path $rel 'SHA256SUMS') -Value "$hash  $name.zip`n"
    & minisign -G -W -p (Join-Path $tmp 'test.pub') -s (Join-Path $tmp 'test.key') | Out-Null
    if ($LASTEXITCODE) { throw 'minisign -G failed' }
    & minisign -S -s (Join-Path $tmp 'test.key') -m (Join-Path $rel 'SHA256SUMS') -t "nabi $v SHA256SUMS" | Out-Null
    if ($LASTEXITCODE) { throw 'minisign -S failed' }
    $testKey = Join-Path $tmp 'test.pub'
    $server = Start-Process python -PassThru -NoNewWindow -ArgumentList '-m', 'http.server', $port, '--bind', '127.0.0.1', '--directory', $serve
    $url = "http://127.0.0.1:$port"
    for ($i = 0; ; $i++) {
        try { Invoke-WebRequest -UseBasicParsing "$url/latest/download/nabi-version" | Out-Null; break }
        catch { if ($i -ge 50) { throw "the local server does not answer: $_" }; Start-Sleep -Milliseconds 200 }
    }

    Step "nabi ${from}: nabi toolchain update"
    $h = Join-Path $tmp 'update'
    InstallFrom $h
    # A project nabi $from made and locked: a git dependency.
    $dep = Join-Path $tmp 'greet'
    New-Item -ItemType Directory "$dep\src" | Out-Null
    Set-Content "$dep\src\lib.nabi" "pub fn greet() -> i32 {`n    return 1;`n}"
    Must git -C $dep init -q
    Must git -C $dep add .
    Must git -C $dep -c user.name=nabi -c user.email=nabi@localhost commit -qm greet
    Push-Location $tmp
    Must "$h\bin\nabi.exe" -q new app
    Pop-Location
    $app = Join-Path $tmp 'app'
    Set-Content "$app\src\main.nabi" "use greet::greet;`nprintln(f`"{greet()}`");"
    Push-Location $app
    try {
        Must "$h\bin\nabi.exe" -q add greet --git $dep
        Must "$h\bin\nabi.exe" -q build
        if (-not (Test-Path nabi.lock)) { throw "nabi $from wrote no nabi.lock" }

        $env:NABI_DIST = $url
        Must "$h\bin\nabi.exe" toolchain update --key $testKey
        Remove-Item Env:NABI_DIST
        CheckDefault $h $v
        $env:NABI_HOME = $h
        if (-not ((& "$h\bin\nabi.exe" toolchain list) -join "`n").Contains("$from-$triple")) {
            throw "nabi $from is no longer installed"
        }

        # The project moves to the staged series and builds as it was locked.
        # [IO.File] resolves a relative path against the process, not the
        # location: name the file in full.
        $manifest = Join-Path $app 'nabi.nabion'
        $m = [IO.File]::ReadAllText($manifest) -replace 'toolchain: "[^"]*"', "toolchain: `"$series`""
        [IO.File]::WriteAllText($manifest, $m)
        Must "$h\bin\nabi.exe" build
        $out = (& "$h\bin\nabi.exe" -q run) -join "`n"
        if ($LASTEXITCODE -or $out -ne '1') { throw "the project prints '$out', not 1" }
        if (-not (Test-Path nabi.lock)) { throw "nabi $v removed nabi.lock" }
        $lock = Get-Content -Raw nabi.lock
        if ($lock -notmatch '(?m)^Lock \{\s*version: 1,') { throw "nabi $v did not write nabi.lock in lock format 1:`n$lock" }
    } finally { Pop-Location }

    Step "nabi ${from}: its install.ps1 -Version $v"
    $h = Join-Path $tmp 'install-old'
    InstallFrom $h
    $env:NABI_DIST = $url
    # It knows only the release key: minisign off PATH, so it checks the
    # SHA256SUMS digest alone.
    $path = $env:Path
    $env:Path = ($env:Path -split ';' | Where-Object { $_ -and -not (Test-Path (Join-Path $_ 'minisign.exe')) }) -join ';'
    Install $oldPs1 @{ Version = $v }
    $env:Path = $path
    Remove-Item Env:NABI_DIST
    CheckDefault $h $v

    Step "nabi ${from}: $name\install.ps1 -Version $v"
    $h = Join-Path $tmp 'install-new'
    InstallFrom $h
    $unpacked = Join-Path $tmp 'unpacked'
    Expand-Archive (Join-Path $Dist "$name.zip") $unpacked
    $env:NABI_DIST = $url
    $env:NABI_HOME = $h
    # Copied out of the archive, so it downloads instead of installing
    # the toolchain around it.
    Copy-Item "$unpacked\$name\install.ps1" (Join-Path $tmp 'install.ps1')
    Install (Join-Path $tmp "install.ps1") @{ Version = $v; Key = $testKey }
    Remove-Item Env:NABI_DIST
    CheckDefault $h $v

    Write-Output "`ntest/update.ps1: nabi $from updates to $v ($triple)"
} finally {
    if ($server) { Stop-Process -Id $server.Id -ErrorAction SilentlyContinue }
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

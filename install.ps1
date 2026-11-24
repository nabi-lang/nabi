# Install a nabi toolchain into $env:NABI_HOME (default
# %USERPROFILE%\.nabi) on Windows: $NABI_HOME\toolchains\<version>-<triple>\,
# made the default, with the proxies $NABI_HOME\bin\{nabi,nabic,nabicc,nabils}.exe,
# and $NABI_HOME\bin put on the user's PATH. The counterpart of install.sh.
#
#   powershell -ExecutionPolicy Bypass -c "irm https://nabi.sh/install.ps1 | iex"
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Archive nabi-<version>-<triple>.zip
#   nabi-<version>-<triple>\install.ps1          (from an unpacked archive)
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Version 0.1.0   (that release)
#   powershell -ExecutionPolicy Bypass -File install.ps1 -NoPackages      (without the official packages)
#
# Environment: NABI_HOME, NABI_VERSION (default below), NABI_DIST (where
# releases are downloaded from), NABI_NO_MODIFY_PATH, NABI_NO_PACKAGES,
# NO_COLOR, CLICOLOR_FORCE. A download is checked against the release's
# SHA256SUMS; with minisign.exe on PATH, SHA256SUMS.minisig must verify
# with the release key first, else the end says how to check it by hand.
# -Key <file> (a minisign .pub file) replaces the release key, to test a
# set signed with a throwaway key; it warns, naming the key. A release carries the official packages, so
# builds that use them need no network; -NoPackages (or NABI_NO_PACKAGES=1)
# runs `nabi component remove packages` after the install. nabic links with its own linker and import libraries:
# no MSVC or Windows SDK.
#
# It puts $NABI_HOME\bin first on the user's PATH and writes the PowerShell
# completions to $NABI_HOME\completions\nabi.ps1, loaded from the profile.
# -NoModifyPath (or NABI_NO_MODIFY_PATH=1) leaves PATH and the profile
# alone and says what to add.
#
# Output is that of install.sh and `nabi toolchain update`: status words
# right-aligned, coloured as nabi colours them: on a console
# that takes escapes or with CLICOLOR_FORCE, never with NO_COLOR or
# TERM=dumb. -Quiet prints errors only, -Verbose adds notes (URLs, paths).
#
# Everything runs from Main, called on the last line, so a download cut
# short runs nothing.
[Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingWriteHost', '',
    Justification = 'Status lines for a person, colored on a console.')]
param([string]$Archive, [string]$Version, [string]$Key, [switch]$NoModifyPath, [switch]$NoPackages, [switch]$Quiet, [switch]$Verbose)
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 redraws a progress bar per chunk (a download many
# times slower) and may offer TLS 1.0 only.
$ProgressPreference = 'SilentlyContinue'
if ($PSVersionTable.PSEdition -ne 'Core') {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
}

if (-not $Version) { $Version = if ($env:NABI_VERSION) { $env:NABI_VERSION } else { '0.1.5' } }
# The release key, keys/release.pub (key id B33E6FE61BE5314C).
$releaseKey = 'RWRMMeUb5m8+s0rEtzbR47dmyrMdlGyGQSvWUn/Ob5QYh0GHgs1GvJEs'
$dist = if ($env:NABI_DIST) { $env:NABI_DIST } else { 'https://github.com/nabi-lang/nabi/releases' }
$home_ = if ($env:NABI_HOME) { $env:NABI_HOME } else { Join-Path $env:USERPROFILE '.nabi' }
if ($env:NABI_NO_MODIFY_PATH -and $env:NABI_NO_MODIFY_PATH -ne '0') { $NoModifyPath = $true }
if ($env:NABI_NO_PACKAGES -and $env:NABI_NO_PACKAGES -ne '0') { $NoPackages = $true }
$color = if ($env:NO_COLOR) { $false }
    elseif ($env:CLICOLOR_FORCE -and $env:CLICOLOR_FORCE -ne '0') { $true }
    elseif ($env:TERM -eq 'dumb') { $false }
    else { -not [Console]::IsOutputRedirected -and $Host.UI.SupportsVirtualTerminal }
$e = [char]27
$sgr = if (-not $color) { @{ Status = ''; Error = ''; Note = ''; Reset = '' } }
    elseif ($env:COLORTERM -in 'truecolor', '24bit') {
        @{ Status = "${e}[1;38;2;56;189;248m"; Error = "${e}[1;38;2;255;95;95m"; Note = "${e}[38;2;56;189;248m"; Reset = "${e}[0m" }
    } elseif ($env:TERM -like '*256color*') {
        @{ Status = "${e}[1;38;5;81m"; Error = "${e}[1;38;5;203m"; Note = "${e}[38;5;81m"; Reset = "${e}[0m" }
    } else {
        @{ Status = "${e}[1;96m"; Error = "${e}[1;91m"; Note = "${e}[96m"; Reset = "${e}[0m" }
    }
$quiet_ = [bool]$Quiet
$verbose_ = [bool]$Verbose

# The hosts with a release archive (scripts/check/targets.sh checks it).
$supported = 'aarch64-apple-darwin', 'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu', 'x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc', 'x86_64-unknown-linux-musl', 'aarch64-unknown-linux-musl', 'x86_64-unknown-freebsd'

# The archive and key are parameters: Main assigns the archive when it downloads one.
function Main([string]$Archive, [string]$Key) {
    # A status line: $Word right-aligned, then $Text.
    function Status([string]$Word, [string]$Text) {
        if (-not $quiet_) { Write-Host ('{0}{1,12}{2} {3}' -f $sgr.Status, $Word, $sgr.Reset, $Text) }
    }
    function Note([string]$Text) {
        if ($verbose_) { Write-Host ('{0}{1,12}:{2} {3}' -f $sgr.Note, 'note', $sgr.Reset, $Text) }
    }
    function Info([string]$Text) { if (-not $quiet_) { Write-Host $Text } }
    function Err([string]$Text) { Write-Host ('install.ps1: {0}error:{1} {2}' -f $sgr.Error, $sgr.Reset, $Text) }

    # The machine's architecture, not this PowerShell's: an x64 or x86
    # PowerShell on arm64 Windows runs emulated and sees its own.
    function HostTriple {
        $env_ = Get-ItemProperty -ErrorAction SilentlyContinue 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
        $arch = if ($env_ -and $env_.PROCESSOR_ARCHITECTURE) { $env_.PROCESSOR_ARCHITECTURE } else { $env:PROCESSOR_ARCHITECTURE }
        switch ($arch) {
            'AMD64' { 'x86_64-pc-windows-msvc' }
            'ARM64' { 'aarch64-pc-windows-msvc' }
            default { throw "no nabi toolchain for the $arch architecture; build nabi from source: https://github.com/nabi-lang/nabi#building-from-source" }
        }
    }


    # Download $Uri to $OutFile: $false on a 404, an error on anything else.
    function Download([string]$Uri, [string]$OutFile) {
        try {
            Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $OutFile
            return $true
        } catch {
            $r = $_.Exception.Response
            if ($r -and [int]$r.StatusCode -eq 404) { return $false }
            if ($r) { throw "cannot download ${Uri}: HTTP $([int]$r.StatusCode)" }
            throw "cannot download $Uri (no network, or the host is unreachable?): $($_.Exception.Message)"
        }
    }

    try {
        $tmp = Join-Path ([IO.Path]::GetTempPath()) "nabi-install-$([Guid]::NewGuid())"
        New-Item -ItemType Directory $tmp | Out-Null
        try {
            # An unpacked toolchain around this script installs itself.
            $here = if ($PSScriptRoot) { $PSScriptRoot } else { '' }
            if (-not $Archive -and $here -and (Test-Path "$here\bin\nabi.exe") -and (Test-Path "$here\bin\nabic.exe")) {
                $src = $here
            } else {
                if (-not $Archive) {
                    $triple = HostTriple
                    if ($Version -notmatch '^\d+\.\d+\.\d+') {
                        throw "-Version takes a full version like 0.1.0, not $Version"
                    }
                    $url ="$dist/download/v$Version/nabi-$Version-$triple.zip"
                    Status 'Downloading' "nabi $Version ($triple)"
                    Note "from $url"
                    $Archive = Join-Path $tmp 'nabi.zip'
                    if (-not (Download $url $Archive)) {
                        # No such host, or no such version.
                        if ($supported -contains $triple) {
                            throw "nabi $Version has no release archive at $url; check the version (-Version, NABI_VERSION)"
                        }
                        throw ("there is no nabi $Version release archive for $triple`n" +
                            "  release archives are built for: $($supported -join ' ')`n" +
                            "  on this host, build nabi from source: see `"Building from source`" in`n" +
                            "  https://github.com/nabi-lang/nabi#building-from-source")
                    }
                    # The signed SHA256SUMS, checked with minisign when it is here.
                    $rel = "$dist/download/v$Version"
                    foreach ($f in 'SHA256SUMS', 'SHA256SUMS.minisig') {
                        if (-not (Download "$rel/$f" (Join-Path $tmp $f))) {
                            throw "$rel/$f is missing (HTTP 404), so the archive cannot be checked; report it at https://github.com/nabi-lang/nabi/issues"
                        }
                    }
                    $pub = $releaseKey
                    if ($Key) {
                        if (-not (Test-Path $Key)) { throw "-Key $Key does not exist; give a minisign public key file, or leave out -Key" }
                        $pub = (Get-Content $Key | Where-Object { $_.Trim() -and $_ -notmatch '^untrusted comment:' } | Select-Object -Last 1).Trim()
                        $raw = [Convert]::FromBase64String($pub)
                        $id = (($raw[9..2] | ForEach-Object { '{0:X2}' -f $_ }) -join '')
                        Write-Host ('install.ps1: {0}warning:{1} trusting key {2} from {3} instead of the nabi release key B33E6FE61BE5314C' -f $sgr.Error, $sgr.Reset, $id, $Key)
                    }
                    $signed = $false
                    if (Get-Command minisign -ErrorAction SilentlyContinue) {
                        & minisign -Vq -P $pub -m (Join-Path $tmp 'SHA256SUMS') -x (Join-Path $tmp 'SHA256SUMS.minisig') | Out-Null
                        if ($LASTEXITCODE) {
                            throw 'SHA256SUMS.minisig does not verify with the key: the download is damaged or was altered, so nothing was installed; try again, and report it at https://github.com/nabi-lang/nabi/security if it persists'
                        }
                        $signed = $true
                        Note "SHA256SUMS.minisig verifies with $(if ($Key) { $Key } else { 'the release key' })"
                    }
                    $name = "nabi-$Version-$triple.zip"
                    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { ($_ -split '\s+')[1] -in $name, "*$name" } | Select-Object -First 1
                    if (-not $line) { throw "the release's SHA256SUMS lists no $name, so it cannot be checked; report it at https://github.com/nabi-lang/nabi/issues" }
                    Set-Content -Path "$Archive.sha256" -Value "$(($line -split '\s+')[0])  $name"
                }
                if (-not (Test-Path $Archive)) { throw "$Archive does not exist; give the path of a nabi-<version>-<triple>.zip to -Archive" }
                # A download is checked against its release's .sha256, an archive
                # file against the one beside it, if any.
                if (Test-Path "$Archive.sha256") {
                    $want = ((Get-Content -Raw "$Archive.sha256").Trim() -split '\s+')[0].ToLower()
                    $got = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLower()
                    if ($got -ne $want) {
                        throw "$Archive has SHA-256 $got, but its .sha256 says $want (a damaged or altered download)"
                    }
                    Note "SHA-256 matches $Archive.sha256"
                }
                Expand-Archive -Path $Archive -DestinationPath "$tmp\files"
                $bin = Get-ChildItem -Directory "$tmp\files\*\bin" | Select-Object -First 1
                if (-not $bin) { throw "$Archive is not a nabi toolchain (no bin\)" }
                $src = $bin.Parent.FullName
            }

            $full = (& "$src\bin\nabic.exe" --version).Split(' ')[1]
            if ($LASTEXITCODE) { throw "cannot run $src\bin\nabic.exe (an archive for another OS or CPU?)" }
            # The toolchain's own nabi installs it (and checks it).
            $env:NABI_HOME = $home_
            & "$src\bin\nabi.exe" -q toolchain install $full --from $src
            if ($LASTEXITCODE) { throw 'nabi toolchain install failed' }
            $carried = Test-Path "$src\lib\nabi\packages"
            & "$src\bin\nabi.exe" -q toolchain default $full
            if ($LASTEXITCODE) { throw 'nabi toolchain default failed' }
            & "$home_\bin\nabi.exe" --version | Out-Null
            if ($LASTEXITCODE) { throw "the installed proxy $home_\bin\nabi.exe does not run" }
        } finally {
            Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
        }
    } catch {
        Err $_.Exception.Message
        # Run as a file: a failing exit code; under iex, leave the shell open.
        if ($PSCommandPath) { exit 1 }
        return
    }

    # $Path as ~\.. when under the user's profile directory, for display only.
    function Shown([string]$Path) {
        if ($env:USERPROFILE -and $Path.StartsWith("$env:USERPROFILE\", [StringComparison]::OrdinalIgnoreCase)) {
            return '~' + $Path.Substring($env:USERPROFILE.Length)
        }
        $Path
    }
    # The installed toolchain's own copy, whatever a manifest here pins.
    if ($NoPackages -and $carried) {
        $env:NABI_TOOLCHAIN = $full
        & "$home_\bin\nabi.exe" -q component remove packages
        if ($LASTEXITCODE) { Err 'nabi is installed with the official packages; `nabi component remove packages` removes them' }
        Remove-Item env:NABI_TOOLCHAIN
    }
    $shown = Shown $home_
    Status 'Installed' "nabi $full to $shown"
    Note "toolchain in $home_\toolchains, proxies in $home_\bin"

    # Whether this PowerShell runs profile scripts (the execution policy
    # without this process's -ExecutionPolicy).
    function ProfilesRun {
        $policy = 'Undefined'
        foreach ($scope in 'MachinePolicy', 'UserPolicy', 'CurrentUser', 'LocalMachine') {
            $p = Get-ExecutionPolicy -Scope $scope
            if ($p -ne 'Undefined') { $policy = $p; break }
        }
        # Undefined: Restricted on Windows PowerShell, RemoteSigned on 7.
        if ($policy -eq 'Undefined') { return $PSVersionTable.PSEdition -eq 'Core' }
        $policy -notin 'Restricted', 'AllSigned'
    }

    Info ''
    $binDir = Join-Path $home_ 'bin'
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = if ($userPath) { $userPath.Split(';') } else { @() }
    if ($NoModifyPath) {
        if ($parts -notcontains $binDir) {
            Info "Add $shown\bin to your PATH (then open a new terminal)."
            Info ''
        }
    } else {
        $changed = @()
        if ($parts -notcontains $binDir) {
            [Environment]::SetEnvironmentVariable('Path', (@($binDir) + $parts | Where-Object { $_ }) -join ';', 'User')
            $changed += 'the user PATH'
        }
        # nabi's completions; when `completions` fails there are none.
        $completions = Join-Path $home_ 'completions\nabi.ps1'
        New-Item -ItemType Directory -Force (Split-Path $completions) | Out-Null
        $script = & "$binDir\nabi.exe" completions powershell 2>$null
        if ($LASTEXITCODE) { $script = '' }
        Set-Content -Path $completions -Value $script -Encoding UTF8
        Note "PowerShell completions in $completions"
        $quoted = $completions.Replace("'", "''")
        $line = "if (Test-Path '$quoted') { . '$quoted' }"
        $userProfile = $PROFILE.CurrentUserAllHosts
        if (-not (ProfilesRun)) {
            Info "PowerShell does not run profile scripts here (execution policy), so for"
            Info "completions add this line to $(Shown $userProfile) once it does:"
            Info "  $line"
            Info ''
        } elseif (-not ((Test-Path $userProfile) -and (Get-Content $userProfile) -contains $line)) {
            New-Item -ItemType Directory -Force (Split-Path $userProfile) | Out-Null
            Add-Content -Path $userProfile -Value $line -Encoding UTF8
            $changed += Shown $userProfile
        }
        if ($changed) {
            Status 'Modified' ($changed -join ', ')
            Info ''
        }
        if ($env:Path.Split(';') -notcontains $binDir) {
            Info 'A new terminal finds nabi on PATH; to use it in this one, run:'
            Info "  `$env:Path = `"$binDir;`$env:Path`""
            Info ''
        }
    }
    # Downloaded, but the signature was not checked here: how to.
    if ($rel -and -not $signed) {
        Info 'SHA256SUMS was checked over HTTPS; its signature was not (no minisign on PATH).'
        Info "To check it, download SHA256SUMS and SHA256SUMS.minisig from $rel/, then:"
        Info "  minisign -Vm SHA256SUMS -P $releaseKey"
        Info "  gh attestation verify $name --repo nabi-lang/nabi"
        Info ''
    }
    Info 'Next steps:'
    Info '  nabi --version'
    Info '  nabi new hello; cd hello; nabi run'
}

Main $Archive $Key

# Builds, signs, and packages a smudgy release.
#
# Prerequisites (one-time):
#   - az login                      (DefaultAzureCredential; service principal env vars also work in CI)
#   - Inno Setup 6
#   - Windows SDK signtool (the Microsoft.Trusted.Signing.Client package is fetched automatically)
#   - cargo install --locked cargo-about --version 0.9.0 --features cli
#
# Signing uses Azure Artifact Signing; the cert is short-lived, so every
# signature is RFC 3161 timestamped against timestamp.acs.microsoft.com.

param(
    # Azure Artifact Signing endpoint for the account's region (West US 2).
    [string]$Endpoint = 'https://wus2.codesigning.azure.net',
    # Artifact Signing account name (Azure resource name).
    [string]$Account = 'wbk',
    # Certificate profile name within the account.
    [string]$CertProfile = 'wkalata',
    [switch]$SkipBuild,
    # Build the binaries and stop before signing/packaging. CI splits the run
    # this way: the federated login assertion used for signing lives only ~5
    # minutes, so the workflow re-authenticates between build and sign.
    [switch]$BuildOnly,
    # Opt-in: package the exact validated export from bin/pgo.py. The DevTools
    # sidecar still uses its ordinary release-full build.
    [string]$PgoManifest
)
if ($BuildOnly -and $SkipBuild) { throw '-BuildOnly and -SkipBuild are mutually exclusive' }

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot

$pgo = $null
# A PGO -BuildOnly phase records the manifest it staged, so the -SkipBuild phase
# cannot silently fall back to whatever ordinary binary sits in target/.
$pgoPending = Join-Path $repoRoot 'target\pgo-package\pending-manifest.txt'
if ($PgoManifest) {
    # The pending-manifest handshake below stores and compares this string, so the
    # build and package phases must agree on it however the operator spelled it.
    $PgoManifest = (Resolve-Path -LiteralPath $PgoManifest).Path
    $verified = & python (Join-Path $PSScriptRoot 'pgo.py') verify $PgoManifest --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'PGO artifact verification failed' }
    $pgo = $verified | ConvertFrom-Json
    if ($SkipBuild) {
        # The build phase is where the notices are checked; it must have run for this manifest.
        if (-not (Test-Path -LiteralPath $pgoPending)) { throw "No PGO build phase is pending; run -BuildOnly -PgoManifest first" }
        $pending = (Get-Content -LiteralPath $pgoPending -Raw).Trim()
        if ($pending -ne $PgoManifest) { throw "A PGO build phase is pending for $pending; pass that manifest or delete $pgoPending" }
    }
} elseif ($SkipBuild -and (Test-Path -LiteralPath $pgoPending)) {
    throw "A PGO build phase is pending for $((Get-Content -LiteralPath $pgoPending -Raw).Trim()); pass -PgoManifest, or delete $pgoPending to package an ordinary build"
}

$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
    Where-Object FullName -Match '\\x64\\' | Sort-Object FullName | Select-Object -Last 1 -ExpandProperty FullName
if (-not $signtool) { throw "signtool.exe not found; install the Windows 10/11 SDK" }

# Fetch the Artifact Signing dlib (signtool plugin) from NuGet on first run.
$toolsDir = Join-Path $repoRoot 'target\signing-tools'
$dlib = Join-Path $toolsDir 'bin\x64\Azure.CodeSigning.Dlib.dll'
if (-not (Test-Path $dlib)) {
    Write-Host "==> Downloading Microsoft.Trusted.Signing.Client" -ForegroundColor Cyan
    New-Item -ItemType Directory -Force $toolsDir | Out-Null
    $nupkg = Join-Path $toolsDir 'client.zip'
    Invoke-WebRequest 'https://www.nuget.org/api/v2/package/Microsoft.Trusted.Signing.Client' -OutFile $nupkg
    Expand-Archive $nupkg -DestinationPath $toolsDir -Force
    Remove-Item $nupkg
    if (-not (Test-Path $dlib)) { throw "package layout unexpected; $dlib missing" }
}

$metadata = Join-Path $toolsDir 'metadata.json'
# ExcludeCredentials pins DefaultAzureCredential to exactly the Azure CLI
# credential (`az login` locally, azure/login OIDC in CI). Every other probe is
# a stall risk on build machines: the managed-identity probe retries against an
# identity-less IMDS endpoint for minutes, and CI images ship Visual Studio and
# the Az PowerShell module, whose credential providers spawn subprocesses that
# have been observed to hang indefinitely.
@{ Endpoint = $Endpoint; CodeSigningAccountName = $Account; CertificateProfileName = $CertProfile
   ExcludeCredentials = @(
       'EnvironmentCredential', 'WorkloadIdentityCredential', 'ManagedIdentityCredential',
       'SharedTokenCacheCredential', 'VisualStudioCredential', 'VisualStudioCodeCredential',
       'AzurePowerShellCredential', 'AzureDeveloperCliCredential', 'InteractiveBrowserCredential'
   ) } |
    ConvertTo-Json | Set-Content $metadata -Encoding ascii

$signArgs = @('sign', '/v', '/debug', '/fd', 'SHA256', '/tr', 'http://timestamp.acs.microsoft.com', '/td', 'SHA256',
              '/dlib', $dlib, '/dmdf', $metadata)

$iscc = "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
if (-not (Test-Path $iscc)) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
if (-not (Test-Path $iscc)) { throw "ISCC.exe (Inno Setup 6) not found" }

$cargoAboutVersion = (& cargo about --version 2>$null)
if ($LASTEXITCODE -ne 0 -or $cargoAboutVersion -ne 'cargo-about 0.9.0') {
    throw "cargo-about 0.9.0 is required; run 'cargo install --locked cargo-about --version 0.9.0 --features cli'"
}

if (-not $SkipBuild) {
    # Materialize the patch-crate-managed dependency patches (patches/*.patch ->
    # target/patch/): [patch.crates-io] points there, so every cargo invocation
    # below (cargo-about included) fails to resolve on a fresh checkout or
    # after a `cargo clean` until they exist.
    if (-not (Get-Command cargo-patch-crate -ErrorAction SilentlyContinue)) {
        Write-Host "==> cargo install --locked patch-crate" -ForegroundColor Cyan
        cargo install --locked patch-crate
        if ($LASTEXITCODE -ne 0) { throw "cargo install patch-crate failed" }
    }
    Write-Host "==> cargo patch-crate --force" -ForegroundColor Cyan
    cargo patch-crate --force
    if ($LASTEXITCODE -ne 0) { throw "cargo patch-crate failed" }

    # The PGO app embeds the notice used during training, and its input
    # fingerprint pins that text. Regenerate anyway and require the result to be
    # byte-identical: a stale committed notice must not ride a validated build.
    $notice = Join-Path $repoRoot 'THIRD-PARTY-NOTICES.md'
    $pgoNotice = $null
    if ($pgo) {
        $pgoNotice = [System.IO.File]::ReadAllBytes($notice)
        $pgoNoticeHash = (Get-FileHash -LiteralPath $notice -Algorithm SHA256).Hash
    }
    Write-Host "==> cargo about generate --workspace --features smudgy_ui/web-audio-cpal --locked about.hbs -o THIRD-PARTY-NOTICES.md" -ForegroundColor Cyan
    cargo about generate --workspace --features smudgy_ui/web-audio-cpal --locked about.hbs -o THIRD-PARTY-NOTICES.md
    $noticeExit = $LASTEXITCODE
    if ($noticeExit -eq 0) {
        python bin/normalize-third-party-notices.py
        $noticeExit = $LASTEXITCODE
    }
    if ($pgo) {
        # Whatever happened, leave the fingerprinted notice in place so later verification still passes.
        $stale = $noticeExit -ne 0 -or (Get-FileHash -LiteralPath $notice -Algorithm SHA256).Hash -ne $pgoNoticeHash
        if ($stale) { [System.IO.File]::WriteAllBytes($notice, $pgoNotice) }
        if ($noticeExit -ne 0) { throw "third-party notice generation failed" }
        if ($stale) { throw 'THIRD-PARTY-NOTICES.md is stale for the current Cargo.lock and release features; regenerate it and start a new PGO run' }
    } elseif ($noticeExit -ne 0) {
        throw "third-party notice generation failed"
    }
    if ($pgo) {
        Write-Host "==> cargo build --locked --profile release-full -p smudgy_inspector" -ForegroundColor Cyan
    } else {
        Write-Host "==> cargo build --profile release-full -p smudgy_ui -p smudgy_inspector --features smudgy_ui/web-audio-cpal" -ForegroundColor Cyan
    }
    # smudgy_inspector (the bundled DevTools sidecar) is not in the workspace
    # default-members, so name it explicitly alongside the app.
    if ($pgo) {
        cargo build --locked --profile release-full -p smudgy_inspector
    } else {
        cargo build --profile release-full -p smudgy_ui -p smudgy_inspector --features smudgy_ui/web-audio-cpal
    }
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}

# The app and its bundled DevTools sidecar -- both signed and packaged.
$exe = Join-Path $repoRoot 'target\release-full\smudgy.exe'
$inspector = Join-Path $repoRoot 'target\release-full\smudgy_inspector.exe'
if ($pgo) {
    # Sign a staging copy so the validated unsigned artifact remains verifiable.
    $pgoStage = Join-Path $repoRoot ('target\pgo-package\' + $pgo.executable_sha256)
    New-Item -ItemType Directory -Force $pgoStage | Out-Null
    $exe = Join-Path $pgoStage 'smudgy.exe'
    Copy-Item -LiteralPath $pgo.executable -Destination $exe -Force
    if ((Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant() -ne $pgo.executable_sha256) {
        throw 'Staged PGO executable hash mismatch'
    }
}
foreach ($f in @($exe, $inspector)) {
    if (-not (Test-Path $f)) { throw "$f not found" }
}

if ($BuildOnly) {
    if ($pgo) {
        New-Item -ItemType Directory -Force (Split-Path -Parent $pgoPending) | Out-Null
        Set-Content -LiteralPath $pgoPending -Value $PgoManifest -Encoding utf8
    } elseif (Test-Path -LiteralPath $pgoPending) {
        # An ordinary build supersedes a staged PGO phase that was never packaged.
        Remove-Item -LiteralPath $pgoPending
    }
    Write-Host "==> Build complete (-BuildOnly); signing/packaging deferred" -ForegroundColor Green
    exit 0
}

foreach ($f in @($exe, $inspector)) {
    Write-Host "==> Signing $f" -ForegroundColor Cyan
    & $signtool @signArgs $f
    if ($LASTEXITCODE -ne 0) { throw "signing $(Split-Path -Leaf $f) failed (is 'az login' current?)" }
}

# Inno Setup invokes "smudgysign <file>" (SignTool=smudgysign $f in installer.iss)
# to sign the uninstaller and the setup exe.
Write-Host "==> Compiling installer" -ForegroundColor Cyan
$iss = Join-Path $repoRoot 'assets\installer.iss'

# Pick the build channel from the version stamped in installer.iss and tell the
# installer, which uses it to choose the install identity (AppId/dir/name/data
# dir). Mirrors core::models::settings::build_channel and bin/bump-version.sh:
# a clean X.Y.Z is a release; a prerelease beginning with `rc`, `ptb`, or
# `nightly` (the next char a digit / '-' / '.' / '+' / end) is prod-like. All
# four keep the release install identity and so upgrade/clobber each other; any
# other '-'/'+' suffix is a dev build, which gets its own identity (the
# SmudgyDevBuild define) and installs side by side.
$version = ([regex]'(?m)^#define\s+MyAppVersion\s+"([^"]+)"').Match((Get-Content -Raw $iss)).Groups[1].Value
if (-not $version) { throw "could not read MyAppVersion from $iss" }
$channel = 'release'
if ($version -match '-') {
    $prerelease = ($version -split '-', 2)[1]
    if ($prerelease -match '^[Rr][Cc]($|[-.+0-9])') {
        $channel = 'release candidate'
    } elseif ($prerelease -match '^[Pp][Tt][Bb]($|[-.+0-9])') {
        $channel = 'public test'
    } elseif ($prerelease -match '^[Nn][Ii][Gg][Hh][Tt][Ll][Yy]($|[-.+0-9])') {
        $channel = 'nightly'
    }
}
if ($channel -eq 'release' -and $version -match '[-+]') { $channel = 'dev' }
Write-Host "    $version -> $channel build" -ForegroundColor Cyan

# $q is Inno's escape for a double-quote; $f is the file being signed.
$signDef = '/Ssmudgysign=$q{0}$q sign /fd SHA256 /tr http://timestamp.acs.microsoft.com /td SHA256 /dlib $q{1}$q /dmdf $q{2}$q $f' -f $signtool, $dlib, $metadata
$isccArgs = @($signDef)
if ($pgo) { $isccArgs += "/DSmudgyAppExe=$exe" }
if ($channel -eq 'dev') { $isccArgs += '/DSmudgyDevBuild' }
& $iscc @isccArgs $iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

$setup = Join-Path $repoRoot 'assets\Output\smudgy-setup.exe'
Write-Host "==> Verifying signatures" -ForegroundColor Cyan
foreach ($f in @($exe, $inspector, $setup)) {
    $sig = Get-AuthenticodeSignature $f
    "{0}: {1} ({2})" -f (Split-Path -Leaf $f), $sig.Status, $sig.SignerCertificate.Subject
    if ($sig.Status -ne 'Valid') { throw "signature on $f is $($sig.Status)" }
}

if ($pgo -and (Test-Path -LiteralPath $pgoPending) -and ((Get-Content -LiteralPath $pgoPending -Raw).Trim() -eq $PgoManifest)) {
    Remove-Item -LiteralPath $pgoPending
}
Write-Host "==> Done: $setup" -ForegroundColor Green

# OneClick KVM — script de release pour Windows.
#
# Workflow :
#   1. Build release Tauri (cargo + Vite + NSIS).
#   2. Signature Authenticode du binaire et de l'installeur via signtool.exe.
#   3. Calcul des hashs SHA-256 pour vérification utilisateur.
#   4. (Optionnel) Création de la signature Ed25519 pour tauri-plugin-updater.
#   5. (Optionnel) Génération du fichier `latest.json` pour le auto-update.
#
# Prérequis :
#   - Variables d'environnement définies :
#       OKVM_VERSION              ex: "0.2.0"
#       OKVM_SIGNTOOL_CERT_PFX    chemin .pfx (peut être chiffré par mot de passe)
#       OKVM_SIGNTOOL_CERT_PASS   mot de passe du .pfx (Get-Content - SecureString)
#       OKVM_TIMESTAMP_URL        ex: "http://timestamp.digicert.com"
#       OKVM_UPDATER_PRIVKEY      (optionnel) chemin clé Ed25519 .key
#       OKVM_UPDATE_BASE_URL      (optionnel) ex: "https://github.com/org/oneclick-kvm/releases/download"
#
#   - signtool.exe accessible (Windows SDK).
#   - pnpm + Rust stable MSVC installés.
#
# Usage :
#   pwsh -File scripts/release.ps1
#
# Sortie : F:\ONECLICK KVM\app\src-tauri\target\x86_64-pc-windows-msvc\release\bundle\nsis\
#   - OneClick KVM_<version>_x64-setup.exe              (signé)
#   - OneClick KVM_<version>_x64-setup.exe.sig          (signature Ed25519, si OKVM_UPDATER_PRIVKEY défini)
#   - latest.json                                        (manifeste update, si OKVM_UPDATE_BASE_URL défini)
#   - sha256.txt                                         (hashs des artefacts)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# 1. Vérifications d'environnement
# ---------------------------------------------------------------------------
if (-not $env:OKVM_VERSION) {
    throw "OKVM_VERSION non définie (ex: 0.2.0)"
}
$version = $env:OKVM_VERSION

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$appDir = Join-Path $repoRoot "app"
$bundleDir = Join-Path $repoRoot "app\src-tauri\target\x86_64-pc-windows-msvc\release\bundle\nsis"
$installerPath = Join-Path $bundleDir "OneClick KVM_${version}_x64-setup.exe"

Write-Host "OneClick KVM release pipeline v$version" -ForegroundColor Cyan
Write-Host "Repo root : $repoRoot"
Write-Host ""

# ---------------------------------------------------------------------------
# 2. Build release
# ---------------------------------------------------------------------------
Write-Host "[1/5] Build release Tauri ..." -ForegroundColor Yellow
Push-Location $appDir
try {
    & pnpm tauri build
    if ($LASTEXITCODE -ne 0) { throw "pnpm tauri build a échoué" }
} finally {
    Pop-Location
}

if (-not (Test-Path $installerPath)) {
    throw "Installeur introuvable : $installerPath"
}

# ---------------------------------------------------------------------------
# 3. Signature Authenticode (signtool.exe)
# ---------------------------------------------------------------------------
if ($env:OKVM_SIGNTOOL_CERT_PFX) {
    Write-Host "[2/5] Signature Authenticode ..." -ForegroundColor Yellow

    if (-not (Test-Path $env:OKVM_SIGNTOOL_CERT_PFX)) {
        throw "Certificat .pfx introuvable : $($env:OKVM_SIGNTOOL_CERT_PFX)"
    }
    $timestampUrl = if ($env:OKVM_TIMESTAMP_URL) { $env:OKVM_TIMESTAMP_URL } else { "http://timestamp.digicert.com" }

    $signtoolArgs = @(
        "sign",
        "/fd", "SHA256",
        "/td", "SHA256",
        "/tr", $timestampUrl,
        "/f",  $env:OKVM_SIGNTOOL_CERT_PFX
    )
    if ($env:OKVM_SIGNTOOL_CERT_PASS) {
        $signtoolArgs += @("/p", $env:OKVM_SIGNTOOL_CERT_PASS)
    }
    $signtoolArgs += $installerPath

    & signtool.exe @signtoolArgs
    if ($LASTEXITCODE -ne 0) { throw "signtool.exe a échoué" }
    Write-Host "  signé : $installerPath"
} else {
    Write-Host "[2/5] (skip) OKVM_SIGNTOOL_CERT_PFX non défini — pas de signature" -ForegroundColor DarkGray
}

# ---------------------------------------------------------------------------
# 4. SHA-256
# ---------------------------------------------------------------------------
Write-Host "[3/5] Calcul SHA-256 ..." -ForegroundColor Yellow
$sha = (Get-FileHash -Algorithm SHA256 -Path $installerPath).Hash.ToLower()
$shaPath = Join-Path $bundleDir "sha256.txt"
"$sha  OneClick KVM_${version}_x64-setup.exe" | Out-File -FilePath $shaPath -Encoding utf8
Write-Host "  $sha"

# ---------------------------------------------------------------------------
# 5. Signature Ed25519 pour tauri-plugin-updater (optionnel)
# ---------------------------------------------------------------------------
if ($env:OKVM_UPDATER_PRIVKEY) {
    Write-Host "[4/5] Signature Ed25519 (tauri-plugin-updater) ..." -ForegroundColor Yellow
    if (-not (Test-Path $env:OKVM_UPDATER_PRIVKEY)) {
        throw "Clé privée Ed25519 introuvable : $($env:OKVM_UPDATER_PRIVKEY)"
    }
    # `pnpm tauri signer sign --private-key <path> <file>` génère un .sig à côté.
    Push-Location $appDir
    try {
        & pnpm tauri signer sign --private-key $env:OKVM_UPDATER_PRIVKEY $installerPath
        if ($LASTEXITCODE -ne 0) { throw "tauri signer sign a échoué" }
    } finally {
        Pop-Location
    }
} else {
    Write-Host "[4/5] (skip) OKVM_UPDATER_PRIVKEY non défini — pas de signature auto-update" -ForegroundColor DarkGray
}

# ---------------------------------------------------------------------------
# 6. Manifeste latest.json pour auto-update (optionnel)
# ---------------------------------------------------------------------------
if ($env:OKVM_UPDATE_BASE_URL) {
    Write-Host "[5/5] Génération latest.json ..." -ForegroundColor Yellow
    $sigPath = "$installerPath.sig"
    $signature = ""
    if (Test-Path $sigPath) {
        $signature = Get-Content -Path $sigPath -Raw
    }
    $downloadUrl = "$($env:OKVM_UPDATE_BASE_URL)/v$version/OneClick KVM_${version}_x64-setup.exe"
    $manifest = @{
        version   = $version
        notes     = "Voir CHANGELOG.md"
        pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
        platforms = @{
            "windows-x86_64" = @{
                signature = $signature
                url       = $downloadUrl
            }
        }
    }
    $manifestPath = Join-Path $bundleDir "latest.json"
    $manifest | ConvertTo-Json -Depth 5 | Out-File -FilePath $manifestPath -Encoding utf8
    Write-Host "  écrit : $manifestPath"
} else {
    Write-Host "[5/5] (skip) OKVM_UPDATE_BASE_URL non défini — pas de manifeste" -ForegroundColor DarkGray
}

Write-Host ""
Write-Host "Release v$version prête." -ForegroundColor Green
Write-Host "Artefacts dans : $bundleDir"

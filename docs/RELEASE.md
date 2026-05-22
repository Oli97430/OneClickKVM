# Procédure de release OneClick KVM

Guide pour packager, signer et publier une nouvelle version.

## Vue d'ensemble

OneClick KVM v0.1.0 est distribué comme un installeur **NSIS** non signé. Cela
fonctionne mais Windows SmartScreen avertit l'utilisateur au premier lancement
("Application non reconnue").

À partir de V3, la procédure de release supporte (de façon **optionnelle**) :

- **Signature Authenticode** du binaire et de l'installeur → supprime
  l'avertissement SmartScreen.
- **Signature Ed25519** du blob installeur → permet à
  [`tauri-plugin-updater`](https://v2.tauri.app/plugin/updater/) de vérifier
  l'authenticité d'une mise à jour avant de l'appliquer.
- **Manifeste `latest.json`** publié à côté de l'installeur → l'app interroge
  cette URL pour savoir si une nouvelle version est disponible.

Tout passe par `scripts/release.ps1`.

## Pré-requis

- **Rust stable** 1.80+ MSVC (`rustup default stable-x86_64-pc-windows-msvc`)
- **Node.js 22+** et **pnpm 10+**
- **Windows SDK** (fournit `signtool.exe` dans
  `C:\Program Files (x86)\Windows Kits\10\bin\<sdk>\x64\`)
- (Optionnel) **Certificat Authenticode** au format `.pfx` (acheté chez
  DigiCert / Sectigo / SSL.com, ou auto-signé pour les tests).
- (Optionnel) **Clé privée Ed25519** pour Tauri updater, générée avec :
  ```powershell
  pnpm tauri signer generate -w okvm-update.key
  # Affiche également la clé publique base64 — à coller dans tauri.conf.json
  ```

## Variables d'environnement

Le script `release.ps1` lit ces variables. Toutes sont optionnelles, mais sans
elles certaines étapes sont sautées.

| Variable | Effet |
|---|---|
| `OKVM_VERSION` | **Obligatoire**. Version semver (ex: `0.2.0`). |
| `OKVM_SIGNTOOL_CERT_PFX` | Chemin vers le `.pfx`. Si absent → pas de signature Authenticode. |
| `OKVM_SIGNTOOL_CERT_PASS` | Mot de passe du `.pfx`. Optionnel. |
| `OKVM_TIMESTAMP_URL` | Serveur RFC 3161 (défaut : `http://timestamp.digicert.com`). |
| `OKVM_UPDATER_PRIVKEY` | Chemin vers la clé Ed25519 `.key`. Si absent → pas de signature update. |
| `OKVM_UPDATE_BASE_URL` | Base URL des releases (ex: `https://github.com/org/oneclick-kvm/releases/download`). Si absent → pas de `latest.json`. |

## Exécution

```powershell
$env:OKVM_VERSION = "0.2.0"
$env:OKVM_SIGNTOOL_CERT_PFX = "C:\secrets\okvm-codesign.pfx"
$env:OKVM_SIGNTOOL_CERT_PASS = (Read-Host -AsSecureString | ConvertFrom-SecureString)
$env:OKVM_UPDATER_PRIVKEY = "C:\secrets\okvm-update.key"
$env:OKVM_UPDATE_BASE_URL = "https://github.com/oneclick-kvm/oneclick-kvm/releases/download"

pwsh -File scripts/release.ps1
```

Le script produit, dans
`app/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/` :

- `OneClick KVM_<version>_x64-setup.exe` — installeur signé
- `OneClick KVM_<version>_x64-setup.exe.sig` — signature Ed25519 (si fournie)
- `latest.json` — manifeste auto-update (si base URL fournie)
- `sha256.txt` — hash SHA-256 de l'installeur

## Publication GitHub

Une release type contient :

```
v0.2.0/
├── OneClick KVM_0.2.0_x64-setup.exe
├── OneClick KVM_0.2.0_x64-setup.exe.sig
└── latest.json
```

Le `latest.json` doit être uploadé à une URL stable (par exemple
`https://github.com/<org>/oneclick-kvm/releases/latest/download/latest.json`)
pour que l'updater intégré puisse vérifier les mises à jour.

## Vérification utilisateur

Un utilisateur peut vérifier manuellement l'intégrité d'un installeur :

```powershell
Get-FileHash -Algorithm SHA256 .\OneClick`ssss`KVM_0.2.0_x64-setup.exe
# Compare avec sha256.txt publié.
```

## Activer l'auto-update dans l'app

Dans `app/src-tauri/Cargo.toml`, ajouter :

```toml
tauri-plugin-updater = "2"
```

Dans `tauri.conf.json` (sous la racine), ajouter :

```json
"plugins": {
  "updater": {
    "active": true,
    "endpoints": [
      "https://github.com/<org>/oneclick-kvm/releases/latest/download/latest.json"
    ],
    "pubkey": "<COLLER_ICI_LA_CLE_PUBLIQUE_BASE64>",
    "dialog": true
  }
}
```

Dans `app/src-tauri/src/lib.rs`, enregistrer le plugin :

```rust
tauri::Builder::default()
    .plugin(tauri_plugin_updater::Builder::new().build())
    // ... reste de la config
```

Au démarrage suivant, l'app vérifiera la présence d'une version plus récente
et proposera la mise à jour.

## Création d'un certificat auto-signé (tests uniquement)

Pour tester la chaîne de signature sans acheter de cert :

```powershell
New-SelfSignedCertificate `
    -Type CodeSigning `
    -Subject "CN=OneClick KVM Test" `
    -KeyUsage DigitalSignature `
    -FriendlyName "OneClick KVM Test Code Signing" `
    -CertStoreLocation Cert:\CurrentUser\My

Get-ChildItem Cert:\CurrentUser\My | Where-Object Subject -like "*OneClick KVM Test*" |
    Export-PfxCertificate -FilePath "$env:USERPROFILE\okvm-test.pfx" `
    -Password (ConvertTo-SecureString "test1234" -AsPlainText -Force)
```

**Important** : un cert auto-signé n'enlève PAS l'avertissement SmartScreen
sur les machines des utilisateurs — il sert uniquement à valider que le
pipeline de signature fonctionne. Pour la prod, acheter un cert EV ou OV
chez un CA reconnu.

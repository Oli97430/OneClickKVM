# Procédure de release OneClick KVM

Guide pour packager et publier une nouvelle version.

## Vue d'ensemble

Le pipeline release **par défaut** produit un installeur NSIS **non signé**
attaché à une release GitHub, accompagné de son hash SHA-256 pour vérification
manuelle d'intégrité. Conséquence : SmartScreen affiche "Application non
reconnue" au premier lancement chez l'utilisateur, qui doit cliquer
"Informations complémentaires" → "Exécuter quand même".

Le projet a **choisi de ne pas s'engager** sur Authenticode (~300 €/an de
cert récurrent, processus EV plus lourd pour effacer SmartScreen sans
historique de réputation). Le script `release.ps1` **supporte** la signature
de façon optionnelle si vous changez d'avis — il suffit de pointer
`OKVM_SIGNTOOL_CERT_PFX` vers un .pfx valide.

## Pré-requis

- **Rust stable** 1.80+ MSVC (`rustup default stable-x86_64-pc-windows-msvc`)
- **Node.js 22+** et **pnpm 10+**
- (Optionnel — uniquement si vous activez la signature)
  - **Windows SDK** pour `signtool.exe`
  - **Certificat Authenticode** au format `.pfx`
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

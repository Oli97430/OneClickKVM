# Troubleshooting

Problèmes courants et leur résolution.

## Première étape pour tout problème : lire les logs JSON

OneClick KVM écrit tous ses events au format JSON structuré dans
`%LocalAppData%\OneClick\OneClickKVM\data\logs\app.log.<date>`. Pour y
accéder facilement :

- **Depuis l'UI** : Settings → Dossiers → 📋 *Ouvrir les logs*
- **Depuis PowerShell** :
  ```powershell
  explorer.exe "$env:LocalAppData\OneClick\OneClickKVM\data\logs"
  ```

Si l'app crash AVANT que le file logger soit init (très rare — c'est la
2ᵉ ligne de `run()`), regarde aussi :
`%LocalAppData%\Temp\oneclick-kvm-crash.log` (panic hook). Cf. v0.1.2
release notes pour le contexte de ce fichier.

Les logs **ne contiennent pas de payload sensible** (pas de touches, pas
de clés, pas de contenu clipboard) — `okvm-logging` est conçu pour
être safe à partager.

## L'app ne démarre pas / page blanche

1. **Lire `app.log.<date>`** (cf. ci-dessus) ou
   `%LocalAppData%\Temp\oneclick-kvm-crash.log` si elle a crashé tôt.
2. **Vérifier WebView2** : Windows 11 l'a en standard, mais sur Windows 10
   il faut parfois l'installer depuis
   [microsoft.com/edge/webview2](https://developer.microsoft.com/microsoft-edge/webview2/)
   (Evergreen Bootstrapper). Vérifier dans :
   `HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`.
3. **Vérifier les logs Windows** : Event Viewer → Applications and Services
   Logs → filtrer par source `OneClickKVM` (seulement WARN/ERROR — pour
   le détail complet, le fichier app.log est plus riche).
4. **Reset complet de la config** : depuis Settings → "Réinitialiser toute
   la configuration", ou manuellement supprimer
   `%APPDATA%\OneClick\OneClickKVM\` puis relancer.

> 💡 **Régression historique v0.1.2 (RPC_E_CHANGED_MODE)** : si tu vois
> ce message dans le crash log, c'est probablement un MFT video qui init
> COM sur le main thread (cf. CHANGELOG [0.1.2] et le contrat
> `okvm_video::ensure_mf_init`). En version actuelle, un `debug_assert`
> détecte cette classe de bug en dev. En release, le file logger trace
> "boot: mf-boot-probe thread spawné" : si tu vois ce log mais l'app
> crash quand même, c'est un autre bug — partage le log.

## Les pairs ne se découvrent pas sur le LAN

1. **Pare-feu Windows** : à la première ouverture, Windows demande
   l'autorisation pour les **réseaux privés** — cocher la case. Si déjà
   refusé, modifier la règle dans :
   `Panneau de configuration → Pare-feu → Autoriser une application` →
   chercher "OneClick KVM".
2. **Vérifier le réseau** : les 2 PCs doivent être sur le même sous-réseau
   (ex: tous deux en 192.168.1.x). Pas de VLAN, pas de Wi-Fi isolation
   "guest".
3. **mDNS bloqué par le routeur** : certains routeurs (FreeBox, Livebox) en
   mode "isolation" cassent mDNS — activer le fallback UDP broadcast dans
   Settings (déjà activé par défaut).
4. **IPv6 désactivé** : par défaut on bind `[::]:47101` (dual-stack). Si
   IPv6 est désactivé sur la carte réseau, l'écoute peut échouer
   silencieusement → bind explicite `0.0.0.0:47101` dans Settings.

## Appairage : "PairingFailed" même avec le bon PIN

1. **Vérifier l'horloge des deux PCs** : un écart > 5 minutes peut faire
   échouer la validation du nonce (anti-replay).
2. **Mode pairing actif sur le serveur** : le PIN ne fonctionne que si
   le mode pairing est activé côté serveur ET non expiré (60 s par défaut).
   Vérifier la bannière sur l'autre PC.
3. **5 tentatives ratées = mode désactivé** : protection anti-brute-force.
   Réactiver le pairing depuis l'UI pour générer un nouveau PIN.

## Le partage d'écran lagger / artefacts

1. **Backend H.264 software** (openh264, default sur certaines machines) :
   plus lourd CPU. Dans Settings → Encodeur H.264 → basculer sur
   "Media Foundation" qui utilise les optimisations SSE/AVX
   (et basculera sur HW NVENC/QSV/AMF en V3.3).
2. **Réseau saturé** : MJPEG (fallback) ≈ 10 Mbps à 720p15, H.264 ≈ 1.5 Mbps.
   Sur Wi-Fi 2.4 GHz saturé, H.264 est obligatoire.
3. **Multi-écran** : pour l'instant on capture uniquement l'écran d'index 0
   (le primaire). Sélection du moniteur arrive V3.x.

## "Become master" ne capture pas le clavier

1. **Pas d'admin requis** mais : certaines applications **élevées par UAC**
   (Task Manager, regedit, ...) consomment les events au niveau Win32 avant
   les hooks user-mode. Pour les capturer aussi, il faudrait lancer
   OneClick KVM en admin — ce qu'on **ne veut pas** par défaut.
2. **AutoHotkey ou autre hook similaire** : un seul `WH_KEYBOARD_LL` à la
   fois — fermer les autres outils.
3. **Le mode master ne s'active pas** : le bouton est grisé tant qu'aucun
   pair n'est connecté (cf. tooltip).

## L'audio partagé crépite / coupe

1. **WASAPI loopback exclusif** : si une autre application utilise le
   périphérique de sortie en mode exclusif, le loopback échoue. Tester en
   désactivant l'exclusivité dans Sound → Properties → Advanced →
   "Allow applications to take exclusive control".
2. **Opus 64 kbps trop bas pour de la musique HiFi** : la cible actuelle
   est la voix / médias bureautique. Pour du multi-canal, attendre V3+.
3. **Audio en TCP** (jusqu'à V3.1) : sur lien saturé, la latence peut
   monter à 100+ ms. V3.1 → UDP+FEC réduira sous 30 ms p99.

## Fichiers : transfert s'arrête à mi-chemin

1. **Espace disque insuffisant** côté receveur : `Documents/OneClickKVM/Inbox/`
   doit avoir au moins la taille totale du transfert + 10 % de buffer.
2. **Anti-virus qui scanne en temps réel** : Windows Defender peut ralentir
   d'un facteur 5-10. Ajouter une exclusion sur le dossier Inbox.
3. **Connexion réseau intermittente** : pas de reprise sur perte pour
   l'instant — un timeout > 30 s annule le transfert. Reprise V3+.

## "L'installeur est bloqué par SmartScreen"

**Comportement attendu et permanent**. OneClick KVM est un projet personnel
qui ne s'engage pas sur une certification Authenticode (~300 €/an de cert
récurrent). SmartScreen affichera donc toujours "Application non reconnue"
au premier lancement.

**Pour vérifier l'intégrité du téléchargement** (recommandé à chaque
nouvelle release), comparer le SHA-256 publié sur la page release GitHub :

```powershell
Get-FileHash -Algorithm SHA256 .\OneClick' KVM_0.1.x_x64-setup.exe
# Comparer avec le hash dans sha256.txt sur la release
```

Si les hashes matchent, c'est l'installeur officiel — pas un fichier altéré
sur le chemin. Vous pouvez ensuite :
1. Cliquer **"Informations complémentaires"** sur la pop-up SmartScreen
2. Cliquer **"Exécuter quand même"**

Si les hashes **diffèrent**, **ne pas exécuter** et signaler une issue
GitHub : il y a eu un MITM sur ton téléchargement.

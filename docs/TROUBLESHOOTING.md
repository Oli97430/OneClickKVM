# Troubleshooting

Problèmes courants et leur résolution.

## L'app ne démarre pas / page blanche

1. **Vérifier WebView2** : Windows 11 l'a en standard, mais sur Windows 10
   il faut parfois l'installer depuis
   [microsoft.com/edge/webview2](https://developer.microsoft.com/microsoft-edge/webview2/)
   (Evergreen Bootstrapper).
2. **Vérifier les logs Windows** : Event Viewer → Applications and Services
   Logs → filtrer par source `OneClickKVM`.
3. **Reset complet de la config** : depuis Settings → "Réinitialiser toute
   la configuration", ou manuellement supprimer
   `%APPDATA%\OneClickKVM\` puis relancer.

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

C'est normal en v0.1.x : l'installeur n'est pas signé Authenticode.

Sur la fenêtre **"Windows protected your PC"** :
1. Cliquer sur **"More info"** (ou "Informations complémentaires")
2. Cliquer sur **"Run anyway"** (ou "Exécuter quand même")

Ou vérifier le hash SHA-256 publié dans la release GitHub pour
s'assurer que l'EXE n'a pas été modifié :

```powershell
Get-FileHash -Algorithm SHA256 .\OneClick' KVM_0.1.x_x64-setup.exe
# Comparer avec sha256.txt publié sur la même release
```

Le code signing arrive en V3 (cf. [docs/RELEASE.md](RELEASE.md)).

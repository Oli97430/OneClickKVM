# Tests end-to-end OneClick KVM

L'app ne peut pas être complètement validée par les tests Rust isolés — la
boucle TCP loopback + handshake AES est testée, mais le chemin complet
**discovery LAN → appairage PIN → bascule curseur → audio → vidéo** demande
2 instances.

Ce guide explique comment lancer **2 instances locales** sur la même machine
Windows pour valider tout ça sans 2 PC physiques.

## Pré-requis

- Installeur NSIS produit : `just build` puis l'EXE dans
  `app/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/`
- (Optionnel) Une seule installation suffit ; on lance le même `.exe` 2 fois
  avec des env vars différentes.

## Isolation des configs

OneClick KVM lit la variable d'environnement **`OKVM_INSTANCE`** au démarrage.
Si elle est définie, le répertoire de config bascule de
`%APPDATA%\OneClickKVM\` vers `%APPDATA%\OneClickKVM-{instance}\`.

Conséquences :
- **Identité Ed25519 différente** par instance → les 2 instances voient
  réellement 2 pairs distincts dans la découverte.
- **Settings indépendants** → chaque instance peut avoir son propre
  `bind_addr`, son backend H.264, sa langue, etc.
- **Liste de pairs indépendante** → l'appairage côté Alice ne pollue pas
  Bob et inversement.

Sanitisation : seuls `[a-zA-Z0-9_-]` sont conservés (max 32 chars). Donc
`OKVM_INSTANCE="../evil"` devient `evil` — pas de path traversal possible.

## Setup recommandé : Alice + Bob

### Ouvrir 2 terminaux PowerShell

#### Terminal 1 — Alice

```powershell
$env:OKVM_INSTANCE = "alice"
# Lancer l'EXE installé OU pnpm tauri dev OU le binaire release direct.
& "C:\Users\$env:USERNAME\AppData\Local\OneClick KVM\OneClick KVM.exe"
# OU en dev :
# cd F:\ONECLICK KVM\app ; pnpm tauri dev
```

#### Terminal 2 — Bob

```powershell
$env:OKVM_INSTANCE = "bob"
& "C:\Users\$env:USERNAME\AppData\Local\OneClick KVM\OneClick KVM.exe"
```

### Configurer des ports différents

Par défaut les 2 instances voudront `[::]:47101`. Le 2e fail au bind.

**Solution** : avant le 2e lancement, ouvrir Settings côté Alice → laisser
`[::]:47101` ; côté Bob → mettre `[::]:47102` puis "Enregistrer" et
relancer Bob (les settings réseau ne sont pas hot-reload). Le hostname
auto-détecté inclut le nom de machine → on verra `<PC>` apparaître 2 fois
dans la découverte (différenciés par empreinte).

## Scénario de test E2E

### 1. Bidirectionnel TCP + handshake

- [ ] Alice et Bob se voient mutuellement dans **Pairs détectés**
      (mDNS ou UDP broadcast) avec leur empreinte cryptographique
- [ ] Cliquer "Ouvrir l'appairage" sur Alice → un PIN à 6 chiffres apparaît
- [ ] Sur Bob, cliquer **+ Pair** sur la carte d'Alice → entrer le PIN
- [ ] Le toast "Appairé" apparaît côté Bob ; côté Alice le badge passe
      "Appairé" + "En ligne"
- [ ] **Anti-brute-force PIN** : sur Bob, ré-essayer 5 fois avec un mauvais PIN
      → la 5e doit déclencher la désactivation du pairing mode côté Alice
      (vérifier dans les logs `tracing::warn` "pairing mode désactivé")

### 2. Capture clavier / souris (KM)

- [ ] Sur Bob, "Activer master" → modal confirm → Activer
- [ ] Bouger la souris vers le bord droit de l'écran Bob → le curseur
      doit "switcher" vers le bureau d'Alice
- [ ] Taper du texte → apparaît dans une note ouverte sur Alice
- [ ] `Ctrl+Alt+Win+0` → curseur revient sur Bob
- [ ] `Ctrl+Alt+Win+1` → switch direct vers Alice (peer #1)

### 3. Partage audio (V3.1 — UDP+FEC bout-en-bout)

- [ ] Sur Alice, lancer une vidéo YouTube (pour avoir du son)
- [ ] Cliquer "Partager audio" côté Alice
- [ ] Bob doit entendre le son de la vidéo (via WASAPI playback)
- [ ] **Vérifier UDP** : dans les logs côté Bob, chercher
      `"UDP audio: remote addr pinned"` confirmant que le NAT pinning a marché.
      Si on voit `"app_audio_recv_tx fermé"`, c'est que le pipe a coupé.

### 4. Partage écran

- [ ] Sur Alice, "Partager écran" → la fenêtre d'Alice apparaît dans le
      panneau "Écrans partagés" de Bob
- [ ] Vérifier dans AboutView côté Alice quel encodeur H.264 est actif
      (cf. champ "Encodage H.264 (actif)")
- [ ] Latence visible : décrocher une fenêtre côté Alice, mesurer le délai
      d'affichage côté Bob (estimation oculaire, devrait être <200ms en LAN)

### 5. Drag & drop fichier

- [ ] Glisser un fichier (~10 MB suffisant) depuis l'Explorateur sur la
      fenêtre OneClick KVM côté Bob
- [ ] Choisir Alice comme cible
- [ ] Vérifier la progression dans TransferList ; le fichier arrive dans
      `Documents/OneClickKVM/Inbox/` côté Alice
- [ ] Vérifier BLAKE3 — Alice voit "Vérification OK" (sinon "Hash mismatch")

### 6. Reconnexion auto

- [ ] Fermer brutalement Alice (clic croix → l'app reste dans le tray ;
      pour vraiment couper utiliser Task Manager → Kill)
- [ ] Côté Bob, attendre ~6 secondes → toast "Pair déconnecté"
- [ ] Relancer Alice depuis le tray ou le menu Démarrer (avec `OKVM_INSTANCE`)
- [ ] Côté Bob, après ~5s → toast "Reconnexion à <Alice>…" puis "Reconnecté"

### 7. Reset complet

- [ ] Settings → Zone sensible → "Réinitialiser toute la configuration"
- [ ] Confirmer
- [ ] L'app indique "Reset effectué", redémarrer manuellement
- [ ] Au prochain lancement, c'est comme un first-run : nouvelle identité
      Ed25519, pairs.json vide, config par défaut

## Limitations connues du test local

- **Mêmes carte réseau, même IP** : Alice et Bob partagent l'IP loopback ou
  l'IP LAN de la machine. Donc le scénario "pas de mDNS car routeur isolation"
  ne peut pas être testé en mono-machine.
- **Latence artificiellement basse** : LAN loopback est en localhost = ~0.05ms,
  un vrai LAN c'est 1-2ms. Les chiffres de latence audio/vidéo ne sont pas
  représentatifs.
- **Capture clavier** : sur 1 machine, les hooks `WH_KEYBOARD_LL` peuvent
  rentrer en conflit si Bob a activé master pendant qu'Alice écoute aussi.
  Préférer ne tester le KM qu'avec 1 master à la fois.
- **Audio loopback feedback** : si Alice partage son audio et Bob le rejoue,
  Alice peut capturer en boucle ce que Bob joue. Activer la sourdine sur
  Bob pendant ce test (ou utiliser des écouteurs).

## Rapport de bugs trouvés

Voir [`docs/TROUBLESHOOTING.md`](TROUBLESHOOTING.md) pour les pièges
courants. Si un test E2E échoue de façon non listée, ouvrir une
[issue GitHub](https://github.com/Oli97430/OneClickKVM/issues/new?template=bug_report.yml).

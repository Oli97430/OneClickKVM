# justfile — Just task runner pour les commandes courantes.
# Install : winget install Casey.Just  (ou cargo install just)
# Usage   : just <recette>
#
# https://just.systems

# Force PowerShell pour toutes les recettes (le défaut Windows est cmd.exe
# qui ne sait pas faire `$env:VAR = "x"` ni les pipes Windows modernes).
set shell := ["powershell", "-NoLogo", "-Command"]

# Liste les recettes disponibles (alias par défaut).
default:
    @just --list

# === Dev quotidien ==========================================================

# Lance l'app en mode dev avec hot-reload Vite + Tauri.
dev:
    cd app && pnpm tauri dev

# === Test E2E mono-machine (2 instances) ====================================
# Lance "alice" en mode dev avec OKVM_INSTANCE=alice et bind par défaut [::]:47101.
# Voir docs/TESTING.md pour le scénario complet.
dev-alice:
    $env:OKVM_INSTANCE = "alice"; cd app; pnpm tauri dev

# Lance "bob" en mode dev avec OKVM_INSTANCE=bob et bind suggéré [::]:47102.
# Important : avant le 1er lancement, ouvrir Settings côté Bob et changer
# bind_addr à [::]:47102 (sinon collision de port avec Alice). Une seule
# fois — la config est persistée dans %APPDATA%/OneClickKVM-bob/.
dev-bob:
    $env:OKVM_INSTANCE = "bob"; cd app; pnpm tauri dev

# Lance les EXE release (post `just build`) en 2 instances "alice" et "bob".
# Évite de recompiler à chaque fois. Le binaire est trouvé via le nom NSIS-bundle.
run-2:
    @echo "Démarre Alice dans une fenêtre PowerShell séparée…"
    Start-Process powershell -ArgumentList "-NoExit","-Command","`$env:OKVM_INSTANCE='alice'; & 'app/src-tauri/target/x86_64-pc-windows-msvc/release/oneclick-kvm-app.exe'"
    @echo "Démarre Bob dans une seconde fenêtre…"
    Start-Process powershell -ArgumentList "-NoExit","-Command","`$env:OKVM_INSTANCE='bob'; & 'app/src-tauri/target/x86_64-pc-windows-msvc/release/oneclick-kvm-app.exe'"
    @echo "Les 2 instances tournent en parallèle. Voir docs/TESTING.md pour les 7 scénarios."

# Nettoie les configs des instances de test (alice + bob).
clean-test-configs:
    @echo "Suppression de %APPDATA%/OneClickKVM-alice et OneClickKVM-bob..."
    if (Test-Path "$env:APPDATA/OneClickKVM-alice") { Remove-Item -Recurse -Force "$env:APPDATA/OneClickKVM-alice" }
    if (Test-Path "$env:APPDATA/OneClickKVM-bob") { Remove-Item -Recurse -Force "$env:APPDATA/OneClickKVM-bob" }
    @echo "✓ configs alice/bob nettoyées (la config par défaut OneClickKVM/ reste intacte)"


# Build l'installeur NSIS release (4 MB, install per-user).
build:
    cd app && pnpm tauri build

# Build le shell sans rebuild les crates (rapide pour itérer sur l'UI).
build-fast:
    cd app && pnpm build

# Lance tous les tests du workspace.
test:
    cargo test --workspace --no-fail-fast

# Lance les tests d'un crate spécifique. Ex: `just test-crate okvm-udp`
test-crate crate:
    cargo test -p {{crate}} --no-fail-fast

# Format check (ne modifie rien) — utilisé par la CI.
fmt-check:
    cargo fmt --all -- --check

# Format tout le code Rust (rustfmt.toml).
fmt:
    cargo fmt --all

# Clippy strict (tous warnings → errors).
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Clippy permissif (affiche tout, ne casse pas).
clippy-soft:
    cargo clippy --workspace --all-targets

# Svelte check + a11y warnings.
svelte-check:
    cd app && pnpm exec svelte-check --tsconfig ./tsconfig.json

# === CI préflight (avant push) ==============================================

# Exécute la même séquence que la CI Windows : fmt + test + svelte-check.
# Si ça passe, le push passera la CI.
ci-local: fmt-check test svelte-check
    @echo "✅ CI préflight passé"

# === Release ================================================================

# Bump de version (cargo + tauri.conf + CHANGELOG ; à éditer ensuite).
# Ex: `just bump 0.2.0`
bump version:
    @echo "Manual edits required :"
    @echo "  1. Cargo.toml          → [workspace.package] version = \"{{version}}\""
    @echo "  2. app/src-tauri/Cargo.toml  → version = \"{{version}}\""
    @echo "  3. app/src-tauri/tauri.conf.json  → \"version\": \"{{version}}\""
    @echo "  4. CHANGELOG.md         → nouvelle section [{{version}}]"
    @echo ""
    @echo "Ensuite : git commit, git tag -a v{{version}}, git push --tags"

# Pousse un tag v* qui déclenche le workflow Release auto.
release-tag version:
    git tag -a v{{version}} -m "OneClick KVM {{version}}"
    git push origin v{{version}}
    @echo ""
    @echo "Suivre la build : gh run watch"

# === Utilitaires ============================================================

# Affiche les pairs détectés sur le LAN (dev tool, à venir).
discover:
    @echo "(à implémenter : okvm-discovery binary helper)"

# Affiche les encodeurs H.264 disponibles sur cette machine.
list-h264:
    cargo test -p okvm-video mediafoundation::tests::enumeration_does_not_panic -- --nocapture

# Nettoie tous les artefacts de build (target/ + dist/ + node_modules/).
clean:
    cargo clean
    rm -rf app/dist
    rm -rf app/node_modules
    rm -rf app/src-tauri/target

# Affiche la taille du build release courant.
size:
    @ls -lh app/src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/*.exe 2>/dev/null || echo "Pas de build release — lance 'just build' d'abord"

# === Open utility ===========================================================

# Ouvre %APPDATA%\OneClickKVM\ dans l'explorateur.
open-config:
    explorer.exe "%APPDATA%\OneClickKVM"

# Ouvre l'Event Viewer filtré sur source OneClickKVM.
open-logs:
    @echo "Event Viewer → Applications and Services Logs"
    eventvwr.msc

//! `okvm-config` — gestion de la configuration utilisateur.
//!
//! - Stocke `config.json` et `peers.json` dans `%APPDATA%\OneClickKVM\`.
//! - Encapsule l'identité Ed25519 via Windows DPAPI (user scope).
//! - Permet d'**exporter** la configuration vers un fichier `.okvm` chiffré
//!   par mot de passe (Argon2 → AES-GCM) pour migrer ou sauvegarder.
//! - Permet de **réinitialiser** (effacement + regénération identité) via
//!   `reset()`.
//!
//! Statut : API publique stub, persistance simple JSON, DPAPI à câbler.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]

pub mod dpapi;

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use okvm_core::{DeviceId, Fingerprint, Permission, Result};

/// Configuration utilisateur globale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Langue UI (BCP-47).
    pub language: String,
    /// Mode sombre / clair / système.
    pub theme: Theme,
    /// Bind address d'écoute (par défaut `[::]`).
    pub bind_addr: String,
    /// Démarre minimisé.
    pub start_minimized: bool,
    /// Démarrer automatiquement avec Windows.
    pub autostart: bool,
    /// Activer le broadcast UDP de découverte.
    pub discovery_broadcast: bool,
    /// Activer mDNS.
    pub discovery_mdns: bool,
    /// Préfixer les logs sensibles par `[redacted]` si jamais on les loggue
    /// (défense en profondeur).
    pub redact_logs: bool,
    /// État de la fenêtre principale (position + taille) sauvegardé à la
    /// fermeture, restauré au prochain démarrage. `None` au premier lancement.
    #[serde(default)]
    pub window_state: Option<WindowState>,
    /// Backend H.264 préféré pour la capture d'écran sortante. Défaut :
    /// `MediaFoundation` sur Windows (souvent plus rapide qu'openh264 grâce aux
    /// optimisations SSE/AVX du MFT Microsoft). Fallback automatique vers
    /// openh264 si l'init MFT échoue.
    #[serde(default = "default_h264_backend")]
    pub h264_backend: H264BackendChoice,
    /// Index du moniteur à partager (0 = primaire). Si la valeur référence un
    /// moniteur qui n'existe plus, on retombe silencieusement sur 0.
    #[serde(default)]
    pub video_screen_idx: u32,
}

fn default_h264_backend() -> H264BackendChoice {
    H264BackendChoice::MediaFoundation
}

/// Choix utilisateur pour le backend H.264.
///
/// Les variantes sont sérialisées avec un nom **stable** via `#[serde(rename)]` :
/// ça permet de renommer les variantes Rust sans casser les `config.json`
/// existants chez les utilisateurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum H264BackendChoice {
    /// Cisco openh264 (CPU pur).
    #[serde(rename = "openh264", alias = "Openh264")]
    Openh264,
    /// MFT Microsoft (Windows, plus rapide grâce SIMD).
    #[serde(rename = "media-foundation", alias = "MediaFoundation")]
    MediaFoundation,
}

/// Position et taille de la fenêtre, persistées entre sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    /// Coordonnée X de la fenêtre (espace écran physique).
    pub x: i32,
    /// Coordonnée Y de la fenêtre.
    pub y: i32,
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            language: detect_language(),
            theme: Theme::System,
            bind_addr: "[::]".to_string(),
            start_minimized: false,
            autostart: false,
            discovery_broadcast: true,
            discovery_mdns: true,
            redact_logs: true,
            window_state: None,
            h264_backend: default_h264_backend(),
            video_screen_idx: 0,
        }
    }
}

/// Thème UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    /// Suit le thème Windows.
    System,
    /// Forcer clair.
    Light,
    /// Forcer sombre.
    Dark,
}

/// Profil d'un pair connu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerProfile {
    /// Identité.
    pub device_id: DeviceId,
    /// Empreinte humaine cachée.
    pub fingerprint: Fingerprint,
    /// Nom convivial choisi par l'utilisateur.
    pub display_name: String,
    /// Permissions accordées.
    pub permissions: Permission,
    /// Adresse MAC pour Wake-on-LAN (optionnelle).
    pub wol_mac: Option<String>,
    /// Dernier `tcp_port` annoncé par ce pair.
    pub last_tcp_port: Option<u16>,
    /// `last_seen` en ms unix.
    pub last_seen_ms: Option<u64>,
}

/// Renvoie le répertoire de configuration.
///
/// **Par défaut** : `%APPDATA%\OneClickKVM\`.
///
/// **Override pour multi-instance** : si la variable d'environnement
/// `OKVM_INSTANCE` est définie (et non vide), le nom devient
/// `OneClickKVM-{instance}`. Permet de lancer 2 instances OneClick KVM sur
/// la même machine sans qu'elles écrasent mutuellement leurs configs
/// `config.json`, `peers.json`, `identity.dpapi`.
///
/// Le nom d'instance est **sanitisé** : seul `[a-zA-Z0-9_-]` est conservé,
/// max 32 caractères. Évite de créer des paths invalides Windows si
/// l'utilisateur passe un mauvais nom.
///
/// Exemple : `$env:OKVM_INSTANCE = "alice"` → `%APPDATA%\OneClickKVM-alice\`.
///
/// # Erreur
/// Renvoie [`okvm_core::Error::Config`] si on ne peut pas déterminer le
/// répertoire utilisateur.
pub fn config_dir() -> Result<PathBuf> {
    let suffix = std::env::var("OKVM_INSTANCE")
        .ok()
        .map(|s| sanitize_instance_name(&s))
        .filter(|s| !s.is_empty());
    let app_name = match &suffix {
        Some(s) => format!("OneClickKVM-{s}"),
        None => "OneClickKVM".to_string(),
    };
    let pd = ProjectDirs::from("io", "OneClick", &app_name)
        .ok_or_else(|| okvm_core::Error::Config("aucun ProjectDirs disponible".into()))?;
    Ok(pd.config_dir().to_path_buf())
}

/// Garde uniquement `[a-zA-Z0-9_-]`, plafonné à 32 caractères. Tout ce qui
/// est rejeté est silencieusement supprimé.
fn sanitize_instance_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect()
}

/// Charge `AppConfig` depuis disque, ou la valeur par défaut si absent.
pub fn load_app_config() -> Result<AppConfig> {
    let path = config_dir()?.join("config.json");
    load_json_or_default(&path)
}

/// Sauvegarde `AppConfig` sur disque.
pub fn save_app_config(cfg: &AppConfig) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.json");
    write_json_atomic(&path, cfg)
}

/// Charge la liste des pairs depuis disque.
pub fn load_peers() -> Result<Vec<PeerProfile>> {
    let path = config_dir()?.join("peers.json");
    load_json_or_default(&path)
}

/// Sauvegarde la liste des pairs.
pub fn save_peers(peers: &[PeerProfile]) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("peers.json");
    write_json_atomic(&path, &peers)
}

/// Charge l'identité Ed25519 depuis disque, ou en génère une et la persiste
/// si elle n'existe pas encore.
///
/// **Stockage** :
/// - Nouveau format : `%APPDATA%\OneClickKVM\identity.dpapi` (blob DPAPI,
///   chiffré par la session utilisateur Windows courante).
/// - Ancien format (compat retro) : `identity.seed` (32 octets bruts).
///   S'il existe, on le migre automatiquement vers DPAPI puis on le supprime.
///
/// # Erreur
/// Erreurs I/O ou cryptographiques (RNG OS, DPAPI).
pub fn load_or_create_identity() -> Result<okvm_core::IdentityKeypair> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    let dpapi_path = dir.join("identity.dpapi");
    let legacy_path = dir.join("identity.seed");

    // 1. Format DPAPI ?
    if dpapi_path.exists() {
        let cipher = std::fs::read(&dpapi_path)?;
        let seed = dpapi::unprotect(&cipher)?;
        if seed.len() != 32 {
            return Err(okvm_core::Error::Config(format!(
                "identity.dpapi: longueur dechiffree invalide {} (attendu 32)",
                seed.len()
            )));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&seed);
        return Ok(okvm_core::IdentityKeypair {
            public: derive_public(&arr)?,
            secret_seed: arr,
        });
    }

    // 2. Ancien format en clair ? On migre vers DPAPI puis on supprime.
    if legacy_path.exists() {
        let seed = std::fs::read(&legacy_path)?;
        if seed.len() != 32 {
            return Err(okvm_core::Error::Config(format!(
                "identity.seed (legacy): longueur invalide {} (attendu 32)",
                seed.len()
            )));
        }
        let cipher = dpapi::protect(&seed)?;
        write_atomic(&dpapi_path, &cipher)?;
        let _ = std::fs::remove_file(&legacy_path);
        tracing::info!("identity migree depuis le format clair vers DPAPI");
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&seed);
        return Ok(okvm_core::IdentityKeypair {
            public: derive_public(&arr)?,
            secret_seed: arr,
        });
    }

    // 3. Premiere execution : genere une nouvelle identite.
    let mut seed = [0u8; 32];
    getrandom_compat(&mut seed)?;
    let cipher = dpapi::protect(&seed)?;
    write_atomic(&dpapi_path, &cipher)?;
    Ok(okvm_core::IdentityKeypair {
        public: derive_public(&seed)?,
        secret_seed: seed,
    })
}

/// Derive la cle publique Ed25519 a partir d'une seed 32 octets, sans
/// dependre de `okvm-crypto` (cycle de deps a eviter ici).
fn derive_public(seed: &[u8; 32]) -> Result<okvm_core::DeviceId> {
    use ed25519_dalek::SigningKey;
    let sk = SigningKey::from_bytes(seed);
    Ok(okvm_core::DeviceId(sk.verifying_key().to_bytes()))
}

fn getrandom_compat(out: &mut [u8]) -> Result<()> {
    use rand_core::{OsRng, RngCore};
    OsRng.fill_bytes(out);
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Réinitialise toute la configuration (supprime config + peers + identité).
///
/// **Destructif** : doit être appelé après confirmation utilisateur.
pub fn reset_all() -> Result<()> {
    let dir = config_dir()?;
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

fn load_json_or_default<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let raw = std::fs::read_to_string(path)?;
    serde_json::from_str(&raw).map_err(|e| okvm_core::Error::Serde(e.to_string()))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let s =
        serde_json::to_string_pretty(value).map_err(|e| okvm_core::Error::Serde(e.to_string()))?;
    std::fs::write(&tmp, s)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Détecte la langue préférée de l'utilisateur depuis Windows.
///
/// Utilise `GetUserDefaultLocaleName` qui renvoie un BCP-47 (ex: `"fr-FR"`,
/// `"en-US"`, `"de-DE"`, `"zh-CN"`). On garde seulement le préfixe de langue
/// (avant le `-`) et on le mappe vers une de nos langues supportées. Tout
/// locale inconnu retombe sur l'anglais.
#[cfg(windows)]
fn detect_language() -> String {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    // `LOCALE_NAME_MAX_LENGTH` = 85 dans winnls.h. Pas exporté par windows-rs.
    const LOCALE_NAME_MAX_LENGTH: usize = 85;
    let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH];
    // SAFETY: buf.len() == LOCALE_NAME_MAX_LENGTH, l'API écrit au plus ce nombre
    // de wchar (terminateur compris) et retourne le nombre écrit (>0 si succès).
    let written = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if written <= 0 {
        return "en".into();
    }
    let raw = String::from_utf16_lossy(&buf[..(written as usize).saturating_sub(1)]);
    let lang = raw.split('-').next().unwrap_or("").to_ascii_lowercase();
    map_supported(&lang)
}

/// Mappe un préfixe de langue 2-lettres vers une de nos langues supportées,
/// avec fallback `"en"`.
fn map_supported(lang: &str) -> String {
    match lang {
        "fr" | "en" | "de" | "es" | "it" | "pt" | "nl" | "ja" | "zh" => lang.to_string(),
        _ => "en".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_sane() {
        let c = AppConfig::default();
        assert!(c.redact_logs);
        assert_eq!(c.bind_addr, "[::]");
    }

    #[test]
    fn sanitize_instance_name_keeps_safe_chars() {
        assert_eq!(sanitize_instance_name("alice"), "alice");
        assert_eq!(sanitize_instance_name("alice_bob-42"), "alice_bob-42");
        // Chars dangereux supprimés silencieusement.
        assert_eq!(sanitize_instance_name("../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_instance_name("a\\b/c"), "abc");
        assert_eq!(sanitize_instance_name("a b c"), "abc");
        // Plafonné à 32 chars.
        let long = "x".repeat(100);
        assert_eq!(sanitize_instance_name(&long).len(), 32);
    }

    #[test]
    fn h264_backend_legacy_pascal_case_still_parses() {
        // Les anciens config.json (avant le serde rename) écrivaient
        // "Openh264" / "MediaFoundation" en PascalCase. L'alias doit garantir
        // la rétro-compat.
        let v: H264BackendChoice = serde_json::from_str("\"Openh264\"").unwrap();
        assert_eq!(v, H264BackendChoice::Openh264);
        let v: H264BackendChoice = serde_json::from_str("\"MediaFoundation\"").unwrap();
        assert_eq!(v, H264BackendChoice::MediaFoundation);
        // Nouveau format kebab-case.
        let v: H264BackendChoice = serde_json::from_str("\"openh264\"").unwrap();
        assert_eq!(v, H264BackendChoice::Openh264);
        let v: H264BackendChoice = serde_json::from_str("\"media-foundation\"").unwrap();
        assert_eq!(v, H264BackendChoice::MediaFoundation);
    }

    #[test]
    fn h264_backend_serializes_kebab_case() {
        let s = serde_json::to_string(&H264BackendChoice::Openh264).unwrap();
        assert_eq!(s, "\"openh264\"");
        let s = serde_json::to_string(&H264BackendChoice::MediaFoundation).unwrap();
        assert_eq!(s, "\"media-foundation\"");
    }
}

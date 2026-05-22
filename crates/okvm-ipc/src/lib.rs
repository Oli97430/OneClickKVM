//! `okvm-ipc` — couche IPC entre backend Rust et UI Tauri.
//!
//! Cette crate déclare les **DTOs** (Data Transfer Objects) sérialisables
//! échangés entre le frontend (Web) et le backend. Chaque `command` Tauri du
//! `app/src-tauri/` reçoit ces structures en entrée et renvoie ces structures
//! en sortie, ce qui découple la logique métier de Tauri (et facilite les
//! tests sans Tauri).
//!
//! Les commandes elles-mêmes (annotées `#[tauri::command]`) vivront dans le
//! crate `src-tauri/` que Tauri générera.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]

use serde::{Deserialize, Serialize};

use okvm_core::{DeviceId, Fingerprint};

// ===========================================================================
// DTO : Sessions et pairs
// ===========================================================================

/// Pair affiché dans la liste de l'UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerView {
    /// Identité.
    pub device_id: DeviceId,
    /// Empreinte humaine.
    pub fingerprint: Fingerprint,
    /// Nom affiché.
    pub name: String,
    /// `true` si appairé et accepté.
    pub paired: bool,
    /// `true` si actuellement connecté.
    pub online: bool,
    /// `true` si visible dans la découverte LAN en ce moment.
    pub discovered: bool,
    /// Adresse IP/port observée la dernière fois.
    pub last_addr: Option<String>,
}

/// Statut global de l'application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStatus {
    /// Identité publique de **ce** PC.
    pub self_identity: DeviceId,
    /// Empreinte humaine de **ce** PC.
    pub self_fingerprint: Fingerprint,
    /// Le hostname courant.
    pub self_hostname: String,
    /// Nombre de pairs actuellement connectés.
    pub connected_peers: u32,
    /// État du listener (true = écoute, false = arrêté).
    pub listening: bool,
}

/// Demande d'appairage avec un pair distant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequest {
    /// Adresse `ip:port` du pair distant à contacter.
    pub address: String,
    /// PIN à 6 chiffres affiché côté distant.
    pub pin: String,
}

/// Résultat d'une tentative d'appairage.
#[allow(missing_docs)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PairResult {
    /// Appairage réussi.
    Success { device_id: DeviceId, fingerprint: Fingerprint, name: String },
    /// Échec.
    Failure { reason: String },
}

// ===========================================================================
// DTO : Événements émis par le backend vers le frontend
// ===========================================================================

/// Événement diffusé du backend vers l'UI via Tauri events.
///
/// Variants documentés inline. Les champs internes reprennent les noms des
/// DTOs (`status`, `peer`, `device_id`, `progress`, ...) ; documentation
/// individuelle redondante avec la doc des types pointés.
#[allow(missing_docs)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BackendEvent {
    /// Mise à jour du statut global.
    StatusChanged { status: AppStatus },
    /// Un pair a été découvert (mDNS / broadcast).
    PeerDiscovered { peer: PeerView },
    /// Un pair s'est connecté.
    PeerConnected { device_id: DeviceId },
    /// Un pair s'est déconnecté.
    PeerDisconnected { device_id: DeviceId, reason: String },
    /// Notification UI à afficher (toast).
    Notification { level: NotificationLevel, title: String, body: String },
    /// Demande de confirmation utilisateur (prompt ACL, par exemple).
    ConfirmationRequested { request_id: String, prompt: String },
    /// Progression d'un transfert de fichier (envoi ou réception).
    TransferProgress { progress: TransferProgressView },
}

/// Vue d'un transfert de fichier pour l'UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferProgressView {
    /// Identifiant unique du transfert.
    pub transfer_id: String,
    /// Direction : `"outbound"` ou `"inbound"`.
    pub direction: String,
    /// Nom du pair distant (peut être vide si inconnu).
    pub peer_name: String,
    /// Nom du fichier en cours (peut être vide si plusieurs fichiers).
    pub current_file: String,
    /// Octets transférés jusqu'ici (total tous fichiers).
    pub bytes_done: u64,
    /// Octets totaux du transfert.
    pub bytes_total: u64,
    /// État (`"running"`, `"done"`, `"error"`, `"cancelled"`).
    pub state: String,
    /// Message d'erreur si state == "error".
    pub error: Option<String>,
}

/// Sévérité d'une notification UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    /// Information.
    Info,
    /// Succès.
    Success,
    /// Avertissement.
    Warn,
    /// Erreur.
    Error,
}

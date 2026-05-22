//! HKDF-SHA256 — dérivation de clés de session depuis un secret X25519 partagé.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroize;

/// Étiquette d'application séparant les usages de clé.
///
/// Fournie en `info` à HKDF pour interdire toute confusion entre clés
/// (data plane, contrôle, futur).
const INFO_DATA: &[u8] = b"OCKV-1/data";
const INFO_CTRL: &[u8] = b"OCKV-1/ctrl";

/// Paire de clés AES-256-GCM dérivées : une par direction.
#[derive(Clone)]
pub struct DerivedKeys {
    /// Clé utilisée par le **client** pour chiffrer ses envois (et par le serveur pour déchiffrer).
    pub client_to_server: [u8; 32],
    /// Clé utilisée par le **serveur** pour chiffrer ses envois.
    pub server_to_client: [u8; 32],
}

impl Drop for DerivedKeys {
    fn drop(&mut self) {
        self.client_to_server.zeroize();
        self.server_to_client.zeroize();
    }
}

impl std::fmt::Debug for DerivedKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DerivedKeys")
            .field("client_to_server", &"[redacted]")
            .field("server_to_client", &"[redacted]")
            .finish()
    }
}

/// Dérive les clés AES-256-GCM pour une session à partir :
///
/// - du secret partagé X25519 (`shared_secret`, 32 octets),
/// - du `transcript_hash` (32 octets) qui scelle le handshake,
/// - d'un compteur `epoch` (incrémenté à chaque rotation de clé).
///
/// Le `transcript_hash` joue le rôle de `salt`. L'`epoch` est intégré à
/// l'`info` pour qu'une rotation de clé produise nécessairement un matériau
/// différent même si le secret de base ne changeait pas (cas pathologique).
#[must_use]
pub fn derive_session_keys(
    shared_secret: &[u8; 32],
    transcript_hash: &[u8; 32],
    epoch: u32,
) -> DerivedKeys {
    let hk = Hkdf::<Sha256>::new(Some(transcript_hash.as_ref()), shared_secret);

    let mut info_c2s = Vec::with_capacity(INFO_DATA.len() + 4 + 4);
    info_c2s.extend_from_slice(INFO_DATA);
    info_c2s.extend_from_slice(b"/c2s");
    info_c2s.extend_from_slice(&epoch.to_be_bytes());

    let mut info_s2c = Vec::with_capacity(INFO_DATA.len() + 4 + 4);
    info_s2c.extend_from_slice(INFO_DATA);
    info_s2c.extend_from_slice(b"/s2c");
    info_s2c.extend_from_slice(&epoch.to_be_bytes());

    let mut k1 = [0u8; 32];
    let mut k2 = [0u8; 32];
    hk.expand(&info_c2s, &mut k1).expect("HKDF expand 32B");
    hk.expand(&info_s2c, &mut k2).expect("HKDF expand 32B");
    DerivedKeys {
        client_to_server: k1,
        server_to_client: k2,
    }
}

/// Variante pour dériver une clé de **contrôle** distincte (rarement utilisée,
/// réservée à des messages hors-bande type rotation négociée).
#[must_use]
pub fn derive_ctrl_key(
    shared_secret: &[u8; 32],
    transcript_hash: &[u8; 32],
    epoch: u32,
) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(transcript_hash.as_ref()), shared_secret);
    let mut info = Vec::with_capacity(INFO_CTRL.len() + 4);
    info.extend_from_slice(INFO_CTRL);
    info.extend_from_slice(&epoch.to_be_bytes());
    let mut out = [0u8; 32];
    hk.expand(&info, &mut out).expect("HKDF expand 32B");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_same_inputs() {
        let s = [1u8; 32];
        let t = [2u8; 32];
        let a = derive_session_keys(&s, &t, 0);
        let b = derive_session_keys(&s, &t, 0);
        assert_eq!(a.client_to_server, b.client_to_server);
        assert_eq!(a.server_to_client, b.server_to_client);
    }

    #[test]
    fn different_epochs_yield_different_keys() {
        let s = [1u8; 32];
        let t = [2u8; 32];
        let a = derive_session_keys(&s, &t, 0);
        let b = derive_session_keys(&s, &t, 1);
        assert_ne!(a.client_to_server, b.client_to_server);
    }

    #[test]
    fn c2s_ne_s2c() {
        let s = [1u8; 32];
        let t = [2u8; 32];
        let a = derive_session_keys(&s, &t, 0);
        assert_ne!(a.client_to_server, a.server_to_client);
    }
}

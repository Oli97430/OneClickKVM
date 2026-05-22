//! Machine d'état du handshake.
//!
//! Implémente la séquence décrite dans `docs/PROTOCOL.md` §2 :
//!
//! 1. **`ClientHello`** : nonce 32B, X25519 éphémère, Ed25519 identité, caps.
//! 2. **`ServerHello`** : pareil + signature Ed25519 du transcript.
//! 3. **`ClientFinished`** (chiffré) : signature Ed25519 du transcript final.
//! 4. **`ServerFinished`** (chiffré) : accept/reject.
//!
//! Cette crate ne s'occupe **que** du calcul cryptographique. La sérialisation
//! des messages ([`PROTOCOL.md`]) et leur transmission TCP sont la
//! responsabilité de `okvm-protocol` et `okvm-net` respectivement.

use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use rand_core::OsRng;
use sha2::{Digest, Sha256};
use thiserror::Error;
use x25519_dalek::{EphemeralSecret, PublicKey};
use zeroize::Zeroize;

use okvm_core::{DeviceId, IdentityKeypair};

use crate::{aead::AeadKey, identity::signing_key, kdf::derive_session_keys};

/// Taille du transcript hash (SHA-256).
pub const TRANSCRIPT_HASH_SIZE: usize = 32;

/// Rôle d'un participant au handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeRole {
    /// Initiateur (envoie `ClientHello` en premier).
    Client,
    /// Receveur (répond avec `ServerHello`).
    Server,
}

/// Erreurs du handshake.
#[derive(Debug, Error)]
pub enum HandshakeError {
    /// Signature Ed25519 invalide.
    #[error("invalid signature")]
    BadSignature,
    /// Clé publique X25519 invalide ou faible.
    #[error("invalid x25519 public key")]
    BadX25519,
    /// Clé publique Ed25519 invalide.
    #[error("invalid ed25519 public key")]
    BadEd25519,
    /// L'état actuel n'autorise pas cette transition.
    #[error("handshake state mismatch: {0}")]
    BadState(&'static str),
    /// PIN d'appairage invalide.
    #[error("pairing PIN mismatch")]
    BadPin,
}

/// Secrets dérivés à la fin du handshake.
///
/// Le `shared_secret` X25519 est zeroized au drop. Les clés AEAD sont prêtes
/// à être utilisées dans `okvm-crypto::aead::AeadSession`.
pub struct SessionSecrets {
    /// Clé pour les envois client→serveur.
    pub key_c2s: AeadKey,
    /// Clé pour les envois serveur→client.
    pub key_s2c: AeadKey,
    /// Hash du transcript complet (pour debug / pinning).
    pub transcript_hash: [u8; TRANSCRIPT_HASH_SIZE],
    /// Epoch initial (0 à la création).
    pub epoch: u32,
    /// Clé publique de l'identité distante (peut être épinglée par l'app).
    pub remote_identity: DeviceId,
}

impl std::fmt::Debug for SessionSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSecrets")
            .field("transcript_hash", &"[redacted]")
            .field("epoch", &self.epoch)
            .field("remote_identity", &self.remote_identity)
            .finish()
    }
}

/// État de la machine de handshake.
///
/// L'usage typique côté client :
/// ```ignore
/// let mut h = HandshakeState::start_client(my_identity);
/// let client_hello_bytes = h.export_client_hello()?;          // 1
/// // ... envoie sur le réseau, reçoit server_hello_bytes ...
/// h.recv_server_hello(server_hello_bytes)?;                    // 2
/// let client_finished = h.export_client_finished()?;          // 3
/// // ... envoie, reçoit server_finished ...
/// let secrets = h.finalize_client(server_finished)?;          // 4
/// ```
///
/// Côté serveur, le miroir : `start_server`, `recv_client_hello`,
/// `export_server_hello`, `recv_client_finished`, `export_server_finished`,
/// `finalize_server`.
///
/// **Note** : cette crate ne sérialise pas les messages — elle expose les
/// champs (X25519 pub, signature, etc.) que `okvm-protocol` met sur le wire.
pub struct HandshakeState {
    role: HandshakeRole,
    identity: IdentityKeypair,
    eph_secret: Option<EphemeralSecret>,
    eph_public: PublicKey,
    transcript: Sha256,
    /// État pour le calcul du transcript : on en garde un clone à chaque étape
    /// pour pouvoir produire le hash au moment des signatures.
    remote_identity: Option<DeviceId>,
    remote_eph: Option<PublicKey>,
    shared: Option<[u8; 32]>,
}

impl HandshakeState {
    /// Crée un état initial. La paire X25519 éphémère est générée immédiatement.
    fn new(role: HandshakeRole, identity: IdentityKeypair) -> Self {
        let eph = EphemeralSecret::random_from_rng(OsRng);
        let eph_public = PublicKey::from(&eph);
        Self {
            role,
            identity,
            eph_secret: Some(eph),
            eph_public,
            transcript: Sha256::new(),
            remote_identity: None,
            remote_eph: None,
            shared: None,
        }
    }

    /// Démarre un handshake côté client.
    #[must_use]
    pub fn start_client(identity: IdentityKeypair) -> Self {
        Self::new(HandshakeRole::Client, identity)
    }

    /// Démarre un handshake côté serveur.
    #[must_use]
    pub fn start_server(identity: IdentityKeypair) -> Self {
        Self::new(HandshakeRole::Server, identity)
    }

    /// Clé publique X25519 éphémère locale (à envoyer dans `ClientHello` / `ServerHello`).
    #[must_use]
    pub fn local_eph_public(&self) -> [u8; 32] {
        self.eph_public.to_bytes()
    }

    /// Identité publique locale.
    #[must_use]
    pub fn local_identity(&self) -> DeviceId {
        self.identity.public
    }

    /// **Côté serveur** : enregistre les éléments du `ClientHello`.
    ///
    /// `client_hello_bytes` est l'encodage complet du message tel qu'il
    /// arrive sur le wire ; il est ajouté au transcript hash brut. Les champs
    /// extraits (clé X25519, identité) sont fournis séparément pour rester
    /// indépendant du format wire.
    pub fn recv_client_hello(
        &mut self,
        client_hello_bytes: &[u8],
        client_eph_pub: [u8; 32],
        client_identity_pub: [u8; 32],
    ) -> Result<(), HandshakeError> {
        if self.role != HandshakeRole::Server {
            return Err(HandshakeError::BadState("recv_client_hello côté client"));
        }
        self.transcript.update(client_hello_bytes);
        self.remote_eph = Some(PublicKey::from(client_eph_pub));
        self.remote_identity = Some(
            DeviceId::from_slice(&client_identity_pub).map_err(|_| HandshakeError::BadEd25519)?,
        );
        // Vérifie syntaxiquement la clé Ed25519 (rejette les bytes non valides).
        VerifyingKey::from_bytes(&client_identity_pub).map_err(|_| HandshakeError::BadEd25519)?;
        self.compute_shared()?;
        Ok(())
    }

    /// **Côté client** : enregistre les éléments du `ServerHello` et vérifie
    /// la signature Ed25519 du serveur sur le transcript.
    pub fn recv_server_hello(
        &mut self,
        server_hello_unsigned_bytes: &[u8],
        server_eph_pub: [u8; 32],
        server_identity_pub: [u8; 32],
        server_signature: &[u8; 64],
        // On feed séparément la partie signature pour qu'elle entre dans le
        // transcript après vérification.
        server_signature_bytes: &[u8],
    ) -> Result<(), HandshakeError> {
        if self.role != HandshakeRole::Client {
            return Err(HandshakeError::BadState("recv_server_hello côté serveur"));
        }
        // 1. Transcript jusqu'ici = ClientHello (déjà feed via export_client_hello).
        // 2. On ajoute ServerHello sans la signature, calcule le hash, vérifie sig.
        self.transcript.update(server_hello_unsigned_bytes);
        let h = self.transcript.clone().finalize();

        let server_pk = VerifyingKey::from_bytes(&server_identity_pub)
            .map_err(|_| HandshakeError::BadEd25519)?;
        let sig = Signature::from_bytes(server_signature);
        server_pk
            .verify(&h, &sig)
            .map_err(|_| HandshakeError::BadSignature)?;

        // 3. On peut maintenant feed la signature dans le transcript pour
        //    qu'elle apparaisse dans le transcript final.
        self.transcript.update(server_signature_bytes);
        self.remote_eph = Some(PublicKey::from(server_eph_pub));
        self.remote_identity = Some(
            DeviceId::from_slice(&server_identity_pub).map_err(|_| HandshakeError::BadEd25519)?,
        );
        self.compute_shared()?;
        Ok(())
    }

    /// **Côté client** : intègre `ClientHello` dans le transcript (à appeler
    /// juste après avoir sérialisé et envoyé `ClientHello`).
    pub fn feed_self_client_hello(
        &mut self,
        client_hello_bytes: &[u8],
    ) -> Result<(), HandshakeError> {
        if self.role != HandshakeRole::Client {
            return Err(HandshakeError::BadState(
                "feed_self_client_hello côté serveur",
            ));
        }
        self.transcript.update(client_hello_bytes);
        Ok(())
    }

    /// **Côté serveur** : produit la signature Ed25519 sur le transcript
    /// `ClientHello || ServerHello(sans signature)` à insérer dans le wire.
    pub fn sign_server_hello(
        &mut self,
        server_hello_unsigned_bytes: &[u8],
    ) -> Result<[u8; 64], HandshakeError> {
        if self.role != HandshakeRole::Server {
            return Err(HandshakeError::BadState("sign_server_hello côté client"));
        }
        self.transcript.update(server_hello_unsigned_bytes);
        let h = self.transcript.clone().finalize();
        let sk = signing_key(&self.identity);
        let sig: Signature = sk.sign(&h);
        Ok(sig.to_bytes())
    }

    /// **Côté serveur** : feed la signature dans le transcript après l'avoir produite.
    pub fn feed_self_server_signature(&mut self, signature_bytes: &[u8]) {
        self.transcript.update(signature_bytes);
    }

    /// Calcule le hash du transcript courant **sans le clore**.
    /// Sert à signer le `ClientFinished`.
    #[must_use]
    pub fn current_transcript_hash(&self) -> [u8; TRANSCRIPT_HASH_SIZE] {
        let mut out = [0u8; TRANSCRIPT_HASH_SIZE];
        out.copy_from_slice(&self.transcript.clone().finalize());
        out
    }

    /// Signe le transcript courant avec l'identité Ed25519 locale.
    /// Utilisé pour produire la signature dans `ClientFinished` (et symétriquement
    /// si on adopte un schéma authentifié mutuellement strictement sur le finished).
    #[must_use]
    pub fn sign_transcript(&self) -> [u8; 64] {
        let h = self.current_transcript_hash();
        let sk = signing_key(&self.identity);
        sk.sign(&h).to_bytes()
    }

    /// Vérifie une signature Ed25519 reçue contre le transcript courant.
    pub fn verify_remote_transcript_sig(&self, signature: &[u8; 64]) -> Result<(), HandshakeError> {
        let id = self
            .remote_identity
            .ok_or(HandshakeError::BadState("remote identity inconnue"))?;
        let pk = VerifyingKey::from_bytes(&id.0).map_err(|_| HandshakeError::BadEd25519)?;
        let h = self.current_transcript_hash();
        let sig = Signature::from_bytes(signature);
        pk.verify(&h, &sig)
            .map_err(|_| HandshakeError::BadSignature)
    }

    /// Derive les cles de session **sans consommer** la machine de handshake.
    ///
    /// Permet de chiffrer les `ClientFinished` / `ServerFinished` avant la
    /// destruction de l'etat. Le caller doit s'engager a ne plus modifier
    /// le transcript apres cet appel (sinon les cles ne correspondront plus
    /// a ce qui sera envoye).
    ///
    /// L'`epoch` initial vaut 0 ; passez `epoch > 0` lors d'une rotation.
    pub fn derive_session_keys_now(&self, epoch: u32) -> Result<SessionSecrets, HandshakeError> {
        let shared = self.shared.ok_or(HandshakeError::BadState(
            "shared secret manquant — compute_shared non appelé",
        ))?;
        let remote_identity = self
            .remote_identity
            .ok_or(HandshakeError::BadState("remote identity inconnue"))?;
        let mut th = [0u8; 32];
        th.copy_from_slice(&self.transcript.clone().finalize());
        let dk = crate::kdf::derive_session_keys(&shared, &th, epoch);
        Ok(SessionSecrets {
            key_c2s: AeadKey::from_bytes(dk.client_to_server),
            key_s2c: AeadKey::from_bytes(dk.server_to_client),
            transcript_hash: th,
            epoch,
            remote_identity,
        })
    }

    /// Finalise et produit les clés de session AEAD.
    ///
    /// Doit être appelée après que les deux côtés ont feed tout le transcript
    /// (`ClientHello`, ServerHello+sig, `ClientFinished`, `ServerFinished`).
    pub fn finalize(self) -> Result<SessionSecrets, HandshakeError> {
        let shared = self.shared.ok_or(HandshakeError::BadState(
            "shared secret manquant — compute_shared non appelé",
        ))?;
        let remote_identity = self
            .remote_identity
            .ok_or(HandshakeError::BadState("remote identity inconnue"))?;
        // Le hasher est consommé par finalize() ; on clone car HandshakeState a Drop.
        let mut th = [0u8; 32];
        th.copy_from_slice(&self.transcript.clone().finalize());
        let dk = derive_session_keys(&shared, &th, 0);
        // Zeroize shared est fait via Drop sur le tableau du KDF.
        Ok(SessionSecrets {
            key_c2s: AeadKey::from_bytes(dk.client_to_server),
            key_s2c: AeadKey::from_bytes(dk.server_to_client),
            transcript_hash: th,
            epoch: 0,
            remote_identity,
        })
    }

    fn compute_shared(&mut self) -> Result<(), HandshakeError> {
        let eph = self
            .eph_secret
            .take()
            .ok_or(HandshakeError::BadState("eph_secret déjà consommé"))?;
        let remote_eph = self
            .remote_eph
            .ok_or(HandshakeError::BadState("remote_eph inconnu"))?;
        let shared = eph.diffie_hellman(&remote_eph);
        let mut s = [0u8; 32];
        s.copy_from_slice(shared.as_bytes());
        self.shared = Some(s);
        // shared (X25519 SharedSecret) sera dropé/zeroized par le crate dalek.
        Ok(())
    }
}

impl Drop for HandshakeState {
    fn drop(&mut self) {
        if let Some(mut s) = self.shared.take() {
            s.zeroize();
        }
    }
}

impl std::fmt::Debug for HandshakeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandshakeState")
            .field("role", &self.role)
            .field("eph_public", &hex_short(&self.eph_public.to_bytes()))
            .field("remote_identity", &self.remote_identity)
            .finish()
    }
}

fn hex_short(b: &[u8]) -> String {
    let mut s = String::with_capacity(16);
    for byte in b.iter().take(6) {
        use std::fmt::Write;
        let _ = write!(&mut s, "{byte:02x}");
    }
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::generate_identity;

    /// Simule un handshake complet client/serveur via des slices d'octets
    /// arbitraires (pas le vrai format wire — c'est `okvm-protocol` qui le
    /// fait). On vérifie uniquement que les clés dérivées coïncident.
    #[test]
    fn full_handshake_keys_match() {
        let id_c = generate_identity().unwrap();
        let id_s = generate_identity().unwrap();

        let mut client = HandshakeState::start_client(id_c.clone());
        let mut server = HandshakeState::start_server(id_s.clone());

        // ===== ClientHello =====
        let client_eph = client.local_eph_public();
        let client_pk = client.local_identity();
        // Simulation : on imagine que le wire est `[magic | eph | id_pub]`
        let mut ch_bytes = Vec::new();
        ch_bytes.extend_from_slice(b"CH--");
        ch_bytes.extend_from_slice(&client_eph);
        ch_bytes.extend_from_slice(&client_pk.0);

        client.feed_self_client_hello(&ch_bytes).unwrap();
        server
            .recv_client_hello(&ch_bytes, client_eph, client_pk.0)
            .unwrap();

        // ===== ServerHello =====
        let server_eph = server.local_eph_public();
        let server_pk = server.local_identity();
        let mut sh_unsigned = Vec::new();
        sh_unsigned.extend_from_slice(b"SH--");
        sh_unsigned.extend_from_slice(&server_eph);
        sh_unsigned.extend_from_slice(&server_pk.0);

        let server_sig = server.sign_server_hello(&sh_unsigned).unwrap();
        // Note : sign_server_hello a déjà feedé sh_unsigned. Maintenant feed la sig.
        server.feed_self_server_signature(&server_sig);

        // Côté client : reçoit sh_unsigned + sig
        client
            .recv_server_hello(
                &sh_unsigned,
                server_eph,
                server_pk.0,
                &server_sig,
                &server_sig,
            )
            .unwrap();

        // ===== ClientFinished (simulé : juste signature transcript) =====
        let cf_sig = client.sign_transcript();
        // Feed côté client
        client.transcript.update(cf_sig);
        // Côté serveur : vérifie
        server.verify_remote_transcript_sig(&cf_sig).unwrap();
        server.transcript.update(cf_sig);

        // ===== ServerFinished (simulé : payload "ok") =====
        let sf = b"ok";
        client.transcript.update(sf);
        server.transcript.update(sf);

        // ===== Finalize =====
        let secrets_c = client.finalize().unwrap();
        let secrets_s = server.finalize().unwrap();

        // Les clés c2s/s2c doivent matcher entre les deux pairs.
        // (key_c2s.bytes() n'est pas exposé ; on chiffre puis déchiffre.)
        use crate::aead::AeadSession;
        let mut send = AeadSession::new(&secrets_c.key_c2s, 0);
        let mut recv = AeadSession::new(&secrets_s.key_c2s, 0);
        let aad = &[0u8; 9];
        let (seq, ct) = send.seal(aad, b"hello").unwrap();
        let pt = recv.open(seq, aad, &ct).unwrap();
        assert_eq!(pt, b"hello");
    }
}

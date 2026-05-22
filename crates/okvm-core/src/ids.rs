//! Identifiants : [`DeviceId`] (clé publique Ed25519 brute), [`Fingerprint`]
//! (empreinte humaine), [`PeerId`] (UUID de session).

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::ZeroizeOnDrop;

use crate::error::{Error, Result};

/// Clé publique Ed25519 long-terme d'un PC (32 octets bruts).
///
/// L'égalité et le `Hash` sont définis byte à byte. La représentation textuelle
/// est `base64url` sans padding pour faciliter l'usage dans des URL et fichiers
/// de config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(#[serde(with = "serde_bytes_array")] pub [u8; 32]);

impl DeviceId {
    /// Construit depuis un slice brut.
    ///
    /// # Erreur
    /// Renvoie [`Error::Protocol`] si la taille n'est pas exactement 32 octets.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            return Err(Error::Protocol(format!(
                "DeviceId attend 32 octets, reçu {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }

    /// Empreinte SHA-256 tronquée à 16 octets, présentée sous forme humaine.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        use sha2_like::Sha256;
        let mut hasher = Sha256::new();
        hasher.update(&self.0);
        let digest = hasher.finalize();
        let mut out = [0u8; 16];
        out.copy_from_slice(&digest[..16]);
        Fingerprint(out)
    }

    /// Représentation base64url sans padding (43 caractères).
    #[must_use]
    pub fn to_base64(&self) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        URL_SAFE_NO_PAD.encode(self.0)
    }
}

/// Empreinte humainement lisible : 16 octets affichés en 8 mots de 4 hex
/// séparés par des espaces, p. ex. `abcd 1234 ef56 7890 1122 3344 5566 7788`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fingerprint(#[serde(with = "serde_bytes_array16")] pub [u8; 16]);

impl Fingerprint {
    /// Construit depuis un slice brut.
    ///
    /// # Erreur
    /// Renvoie [`Error::Protocol`] si la taille n'est pas exactement 16 octets.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 16 {
            return Err(Error::Protocol(format!(
                "Fingerprint attend 16 octets, reçu {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 16];
        out.copy_from_slice(bytes);
        Ok(Self(out))
    }
}

impl std::fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, chunk) in self.0.chunks(2).enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{:02x}{:02x}", chunk[0], chunk[1])?;
        }
        Ok(())
    }
}

/// Identifiant **éphémère** d'un pair pour une session donnée (UUID v7).
///
/// À ne pas confondre avec [`DeviceId`] (identité long-terme) ni avec
/// [`Fingerprint`] (empreinte). `PeerId` est généré par le pair initiateur
/// à chaque nouvelle session, et permet de désambiguïser plusieurs sessions
/// simultanées avec le même `DeviceId` (machine multi-utilisateur, tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerId(pub Uuid);

impl PeerId {
    /// Génère un nouvel identifiant unique basé sur l'horloge (UUID v7).
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for PeerId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Paire de clé brute (publique + privée) Ed25519 utilisée pour l'identité
/// long-terme. La partie privée est `ZeroizeOnDrop`.
///
/// Cette struct vit dans `okvm-core` pour éviter une dépendance circulaire
/// avec `okvm-crypto`, mais sa génération et son utilisation cryptographique
/// vivent dans `okvm-crypto`.
#[derive(ZeroizeOnDrop, Clone)]
pub struct IdentityKeypair {
    /// Partie publique exposée aux pairs.
    #[zeroize(skip)]
    pub public: DeviceId,
    /// Partie privée — 32 octets, zeroized au drop.
    pub secret_seed: [u8; 32],
}

impl std::fmt::Debug for IdentityKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityKeypair")
            .field("public", &self.public)
            .field("secret_seed", &"[redacted]")
            .finish()
    }
}

// --------- helpers internes -----------------------------------------------

mod sha2_like {
    //! Mini-trait pour décrire un hash SHA-256 sans pull en `sha2` ici.
    //! L'implémentation concrète vit dans `okvm-crypto` (qui dépend de `sha2`),
    //! mais `okvm-core` veut éviter cette dépendance. On utilise un trait + une
    //! impl locale minimaliste en SHA-256 software simple.
    //!
    //! Note : ce mini-SHA-256 n'est utilisé que pour calculer des empreintes
    //! d'identité (16 octets affichés à l'humain). Pour toute crypto, voir
    //! `okvm-crypto`.

    pub(super) struct Sha256 {
        state: [u32; 8],
        buf: [u8; 64],
        buf_len: usize,
        total_bits: u64,
    }

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    impl Sha256 {
        pub(super) fn new() -> Self {
            Self {
                state: H0,
                buf: [0; 64],
                buf_len: 0,
                total_bits: 0,
            }
        }

        pub(super) fn update(&mut self, data: &[u8]) {
            self.total_bits = self.total_bits.wrapping_add((data.len() as u64) * 8);
            let mut i = 0;
            if self.buf_len > 0 {
                let take = (64 - self.buf_len).min(data.len());
                self.buf[self.buf_len..self.buf_len + take]
                    .copy_from_slice(&data[..take]);
                self.buf_len += take;
                i = take;
                if self.buf_len == 64 {
                    let block = self.buf;
                    self.process(&block);
                    self.buf_len = 0;
                }
            }
            while i + 64 <= data.len() {
                self.process(&data[i..i + 64].try_into().unwrap());
                i += 64;
            }
            if i < data.len() {
                let rest = data.len() - i;
                self.buf[..rest].copy_from_slice(&data[i..]);
                self.buf_len = rest;
            }
        }

        pub(super) fn finalize(mut self) -> [u8; 32] {
            // Padding : 1 puis zéros puis longueur 64-bit BE
            let bits = self.total_bits;
            self.buf[self.buf_len] = 0x80;
            self.buf_len += 1;
            if self.buf_len > 56 {
                while self.buf_len < 64 {
                    self.buf[self.buf_len] = 0;
                    self.buf_len += 1;
                }
                let block = self.buf;
                self.process(&block);
                self.buf_len = 0;
            }
            while self.buf_len < 56 {
                self.buf[self.buf_len] = 0;
                self.buf_len += 1;
            }
            self.buf[56..64].copy_from_slice(&bits.to_be_bytes());
            let block = self.buf;
            self.process(&block);
            let mut out = [0u8; 32];
            for (i, w) in self.state.iter().enumerate() {
                out[i * 4..(i + 1) * 4].copy_from_slice(&w.to_be_bytes());
            }
            out
        }

        fn process(&mut self, block: &[u8; 64]) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(block[i * 4..(i + 1) * 4].try_into().unwrap());
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let mut a = self.state[0];
            let mut b = self.state[1];
            let mut c = self.state[2];
            let mut d = self.state[3];
            let mut e = self.state[4];
            let mut f = self.state[5];
            let mut g = self.state[6];
            let mut h = self.state[7];
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = h
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let mj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(mj);
                h = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            self.state[0] = self.state[0].wrapping_add(a);
            self.state[1] = self.state[1].wrapping_add(b);
            self.state[2] = self.state[2].wrapping_add(c);
            self.state[3] = self.state[3].wrapping_add(d);
            self.state[4] = self.state[4].wrapping_add(e);
            self.state[5] = self.state[5].wrapping_add(f);
            self.state[6] = self.state[6].wrapping_add(g);
            self.state[7] = self.state[7].wrapping_add(h);
        }
    }
}

// Helpers serde pour [u8; N] (par défaut serde sérialise byte par byte
// pour les tableaux fixes, mais on veut un tableau d'octets compact).
mod serde_bytes_array {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(bytes)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let v: Vec<u8> = Deserialize::deserialize(d)?;
        if v.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "attendu 32 octets, reçu {}",
                v.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&v);
        Ok(out)
    }
}

mod serde_bytes_array16 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8; 16], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(bytes)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 16], D::Error> {
        let v: Vec<u8> = Deserialize::deserialize(d)?;
        if v.len() != 16 {
            return Err(serde::de::Error::custom(format!(
                "attendu 16 octets, reçu {}",
                v.len()
            )));
        }
        let mut out = [0u8; 16];
        out.copy_from_slice(&v);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_format() {
        let fp = Fingerprint([
            0xab, 0xcd, 0x12, 0x34, 0xef, 0x56, 0x78, 0x90,
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        ]);
        assert_eq!(
            fp.to_string(),
            "abcd 1234 ef56 7890 1122 3344 5566 7788"
        );
    }

    #[test]
    fn device_id_fingerprint_stable() {
        let id = DeviceId([42u8; 32]);
        let fp1 = id.fingerprint();
        let fp2 = id.fingerprint();
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn peer_id_unique() {
        let a = PeerId::new();
        let b = PeerId::new();
        assert_ne!(a, b);
    }
}

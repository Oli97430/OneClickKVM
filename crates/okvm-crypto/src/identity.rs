//! Génération et chargement d'une identité Ed25519 long-terme.
//!
//! La persistance disque (DPAPI) vit dans `okvm-config` ; cette crate ne fait
//! que générer une nouvelle identité ou recharger depuis une seed brute.

use ed25519_dalek::{SigningKey, VerifyingKey};
use thiserror::Error;

use okvm_core::{DeviceId, IdentityKeypair};

/// Erreurs possibles lors de la manipulation d'identité.
#[derive(Debug, Error)]
pub enum IdentityError {
    /// L'OS n'a pas fourni assez d'entropie.
    #[error("getrandom: {0}")]
    Random(#[from] getrandom::Error),
    /// La seed fournie n'a pas la taille attendue.
    #[error("seed length invalide (attendu 32 octets)")]
    BadSeedLength,
}

/// Génère une nouvelle identité Ed25519 à partir du RNG de l'OS.
///
/// La seed est tirée via `getrandom` (BCryptGenRandom sur Windows) puis
/// transformée en `SigningKey`. La clé publique correspondante est exposée
/// comme [`DeviceId`].
///
/// # Erreur
/// Renvoie [`IdentityError::Random`] si l'OS ne peut fournir d'entropie.
pub fn generate_identity() -> Result<IdentityKeypair, IdentityError> {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed)?;
    Ok(from_seed(&seed).expect("seed taille fixe"))
}

/// Charge une identité depuis une seed Ed25519 (32 octets).
///
/// La seed devrait provenir d'un stockage sécurisé (DPAPI sur Windows).
///
/// # Erreur
/// Renvoie [`IdentityError::BadSeedLength`] si la seed n'a pas 32 octets.
pub fn from_seed(seed: &[u8]) -> Result<IdentityKeypair, IdentityError> {
    if seed.len() != 32 {
        return Err(IdentityError::BadSeedLength);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(seed);
    let signing = SigningKey::from_bytes(&arr);
    let verifying: VerifyingKey = signing.verifying_key();
    Ok(IdentityKeypair {
        public: DeviceId(verifying.to_bytes()),
        secret_seed: arr,
    })
}

/// Reconstruit le `SigningKey` Ed25519 à partir d'une identité.
///
/// La clé est intentionnellement reconstruite à la volée plutôt que stockée
/// pour minimiser la durée pendant laquelle un objet exposant la clé privée
/// non zeroizée existe en mémoire.
#[must_use]
pub fn signing_key(id: &IdentityKeypair) -> SigningKey {
    SigningKey::from_bytes(&id.secret_seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let id = generate_identity().unwrap();
        let id2 = from_seed(&id.secret_seed).unwrap();
        assert_eq!(id.public, id2.public);
    }

    #[test]
    fn signing_works() {
        let id = generate_identity().unwrap();
        let sk = signing_key(&id);
        use ed25519_dalek::Signer;
        let sig = sk.sign(b"test message");
        let vk = VerifyingKey::from_bytes(&id.public.0).unwrap();
        use ed25519_dalek::Verifier;
        vk.verify(b"test message", &sig).unwrap();
    }
}

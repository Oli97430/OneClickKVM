//! Helpers `#[serde(with = "...")]` pour les tableaux fixes d'octets.
//!
//! Serde n'expose pas d'`impl Serialize/Deserialize` automatique pour
//! `[u8; N]` au-dela de N = 32 (limitation historique). On fournit ici des
//! modules de serialisation pour les tailles utilisees par le protocole :
//! 4, 16, 32 et 64.
//!
//! Serialisation : appel a `serialize_bytes` (compatible bincode `Vec<u8>`).
//! Deserialisation : on lit un `Vec<u8>` puis on verifie la longueur.

pub(crate) mod bytes4 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(b: &[u8; 4], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(b)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 4], D::Error> {
        let v: Vec<u8> = Deserialize::deserialize(d)?;
        if v.len() != 4 {
            return Err(serde::de::Error::custom(format!(
                "[u8; 4] attendu, recu {}",
                v.len()
            )));
        }
        let mut out = [0u8; 4];
        out.copy_from_slice(&v);
        Ok(out)
    }
}

// bytes16 supprimé (was dead code) — si besoin futur, restaurer depuis git
// history. Aucune struct du protocole n'utilise actuellement de `[u8; 16]`
// serializable.

pub(crate) mod bytes32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(b: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(b)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let v: Vec<u8> = Deserialize::deserialize(d)?;
        if v.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "[u8; 32] attendu, recu {}",
                v.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&v);
        Ok(out)
    }
}

pub(crate) mod bytes64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(b: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(b)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let v: Vec<u8> = Deserialize::deserialize(d)?;
        if v.len() != 64 {
            return Err(serde::de::Error::custom(format!(
                "[u8; 64] attendu, recu {}",
                v.len()
            )));
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&v);
        Ok(out)
    }
}

pub(crate) mod opt_bytes32 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        opt: &Option<[u8; 32]>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        opt.as_ref().map(|b| b.to_vec()).serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<[u8; 32]>, D::Error> {
        let opt: Option<Vec<u8>> = Deserialize::deserialize(d)?;
        match opt {
            None => Ok(None),
            Some(v) => {
                if v.len() != 32 {
                    return Err(serde::de::Error::custom(format!(
                        "Option<[u8; 32]>: attendu 32 octets, recu {}",
                        v.len()
                    )));
                }
                let mut out = [0u8; 32];
                out.copy_from_slice(&v);
                Ok(Some(out))
            }
        }
    }
}

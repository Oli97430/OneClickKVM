//! Wrapper Windows DPAPI (`CryptProtectData` / `CryptUnprotectData`).
//!
//! Encapsule un blob de secret pour le persister sur disque de telle sorte
//! que **seul l'utilisateur courant**, sur **cette machine**, puisse le
//! relire. Le decryptage echoue automatiquement si :
//!
//! - le fichier est lu depuis un autre profil utilisateur Windows,
//! - le fichier est copie sur une autre machine,
//! - la clef de session Windows a ete reinitialisee (logon credentials reset).
//!
//! Voir <https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata>.

use okvm_core::{Error, Result};

#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB},
    },
};

/// Etiquette decrivant le blob (utile pour `CryptUnprotectData` rendant
/// l'optionnel `pdescrout`).
const DESCRIPTION: &str = "OneClickKVM-identity-v1";

/// Chiffre `plain` avec DPAPI scope utilisateur courant.
#[cfg(windows)]
pub fn protect(plain: &[u8]) -> Result<Vec<u8>> {
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(plain.len())
            .map_err(|_| Error::Crypto("plain trop grand pour DPAPI".into()))?,
        pbData: plain.as_ptr().cast_mut(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB::default();

    let desc_w: Vec<u16> = DESCRIPTION
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: CryptProtectData attend des pointeurs valides pour la duree de
    // l'appel ; in_blob.pbData pointe sur `plain` (vivant ici) et out_blob est
    // une struct correctement initialisee. La memoire allouee dans out_blob.pbData
    // doit etre liberee via LocalFree, ce qu'on fait juste apres avoir copie.
    unsafe {
        CryptProtectData(
            &raw const in_blob,
            PCWSTR(desc_w.as_ptr()),
            None,
            None,
            None,
            0,
            &raw mut out_blob,
        )
        .map_err(|e| Error::Crypto(format!("CryptProtectData: {e}")))?;

        // Copie immediatement out_blob → Vec.
        if out_blob.pbData.is_null() || out_blob.cbData == 0 {
            return Err(Error::Crypto("CryptProtectData output vide".into()));
        }
        let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let v = slice.to_vec();

        // Libere la memoire allouee par DPAPI.
        let _ = LocalFree(windows::Win32::Foundation::HLOCAL(out_blob.pbData.cast()));
        Ok(v)
    }
}

/// Dechiffre un blob produit par [`protect`].
#[cfg(windows)]
pub fn unprotect(cipher: &[u8]) -> Result<Vec<u8>> {
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(cipher.len())
            .map_err(|_| Error::Crypto("cipher trop grand pour DPAPI".into()))?,
        pbData: cipher.as_ptr().cast_mut(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB::default();

    // SAFETY: identique a protect ; on libere out_blob.pbData via LocalFree.
    unsafe {
        CryptUnprotectData(
            &raw const in_blob,
            None,
            None,
            None,
            None,
            0,
            &raw mut out_blob,
        )
        .map_err(|e| Error::Crypto(format!("CryptUnprotectData: {e}")))?;

        if out_blob.pbData.is_null() || out_blob.cbData == 0 {
            return Err(Error::Crypto("CryptUnprotectData output vide".into()));
        }
        let slice = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let v = slice.to_vec();

        let _ = LocalFree(windows::Win32::Foundation::HLOCAL(out_blob.pbData.cast()));
        Ok(v)
    }
}

#[cfg(not(windows))]
pub fn protect(plain: &[u8]) -> Result<Vec<u8>> {
    // Fallback no-op pour les autres OS (a remplacer par equivalent platform).
    Ok(plain.to_vec())
}

#[cfg(not(windows))]
pub fn unprotect(cipher: &[u8]) -> Result<Vec<u8>> {
    Ok(cipher.to_vec())
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let plain = b"secret 32-byte seed for ed25519x";
        let cipher = protect(plain).unwrap();
        assert_ne!(cipher, plain.to_vec(), "le blob doit etre chiffre");
        assert!(cipher.len() > plain.len(), "DPAPI ajoute des headers");
        let back = unprotect(&cipher).unwrap();
        assert_eq!(back, plain);
    }

    #[test]
    fn tampered_rejected() {
        let plain = b"my secret";
        let mut cipher = protect(plain).unwrap();
        // Corrompt un octet au milieu.
        let mid = cipher.len() / 2;
        cipher[mid] ^= 0xFF;
        assert!(unprotect(&cipher).is_err(), "blob altere doit etre rejete");
    }
}

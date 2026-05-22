//! `okvm-i18n` — internationalisation via Fluent.
//!
//! Statut : squelette minimal. L'intégration de `fluent` + `fluent-bundle` se
//! fera en phase 5 ; pour l'instant on expose une API stable basée sur un
//! lookup `HashMap` afin que les autres crates puissent dépendre de cette
//! interface sans tirer Fluent dans l'arbre de dépendances trop tôt.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]

use std::collections::HashMap;

use parking_lot::RwLock;

/// Langues supportées par `OneClick` KVM.
pub const SUPPORTED_LANGUAGES: &[&str] = &["fr", "en", "de", "es", "it", "pt", "nl", "ja", "zh"];

/// Catalogue de traductions pour une langue donnée.
#[derive(Debug, Default, Clone)]
pub struct Catalog {
    /// Code BCP-47 (`"fr"`, `"en-US"`, ...).
    pub lang: String,
    /// Lookup `clé → traduction`.
    pub entries: HashMap<String, String>,
}

/// Gestionnaire global d'i18n.
pub struct I18n {
    /// Langue active.
    active: RwLock<String>,
    /// Catalogues chargés indexés par lang code.
    catalogs: RwLock<HashMap<String, Catalog>>,
    /// Fallback : si une clé manque dans la langue active, on tombe sur celui-ci.
    fallback: RwLock<String>,
}

impl I18n {
    /// Crée un gestionnaire vide avec `lang` comme langue active et `en` en fallback.
    #[must_use]
    pub fn new(lang: impl Into<String>) -> Self {
        Self {
            active: RwLock::new(lang.into()),
            catalogs: RwLock::new(HashMap::new()),
            fallback: RwLock::new("en".into()),
        }
    }

    /// Ajoute ou remplace un catalogue.
    pub fn load_catalog(&self, cat: Catalog) {
        self.catalogs.write().insert(cat.lang.clone(), cat);
    }

    /// Change la langue active.
    pub fn set_language(&self, lang: impl Into<String>) {
        *self.active.write() = lang.into();
    }

    /// Renvoie la traduction de `key` ou la clé brute si manquante.
    #[must_use]
    pub fn t(&self, key: &str) -> String {
        let active = self.active.read().clone();
        let fallback = self.fallback.read().clone();
        let cats = self.catalogs.read();
        if let Some(c) = cats.get(&active) {
            if let Some(v) = c.entries.get(key) {
                return v.clone();
            }
        }
        if let Some(c) = cats.get(&fallback) {
            if let Some(v) = c.entries.get(key) {
                return v.clone();
            }
        }
        key.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_works() {
        let i = I18n::new("fr");
        let mut en = Catalog::default();
        en.lang = "en".into();
        en.entries.insert("hello".into(), "Hello".into());
        i.load_catalog(en);
        // pas de catalogue FR, on tombe sur EN
        assert_eq!(i.t("hello"), "Hello");
        // clé inconnue : on rend la clé brute
        assert_eq!(i.t("unknown"), "unknown");
    }

    #[test]
    fn fr_overrides_en() {
        let i = I18n::new("fr");
        let mut en = Catalog::default();
        en.lang = "en".into();
        en.entries.insert("hello".into(), "Hello".into());
        let mut fr = Catalog::default();
        fr.lang = "fr".into();
        fr.entries.insert("hello".into(), "Bonjour".into());
        i.load_catalog(en);
        i.load_catalog(fr);
        assert_eq!(i.t("hello"), "Bonjour");
    }
}

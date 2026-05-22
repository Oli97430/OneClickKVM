//! Horodatages monotones et wall-clock utilisés dans le protocole.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Timestamp Unix en millisecondes depuis l'epoch.
///
/// Utilisé pour les `Ping/Pong`, heartbeats, événements input et timestamps
/// d'événements clipboard. Pour les flux audio/vidéo on utilise plutôt un
/// timestamp microseconde par stream (champ dédié, voir `okvm-protocol`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// Horodatage courant en ms depuis 1970-01-01 UTC.
    ///
    /// # Panics
    /// Panique uniquement si l'horloge système est antérieure à 1970,
    /// situation considérée comme impossible en production.
    #[must_use]
    pub fn now_unix_ms() -> Self {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("horloge système antérieure à 1970");
        // u128 → u64 : on est largement sous 2^64 ms même au prochain millénaire.
        let ms = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        Self(ms)
    }

    /// Différence en millisecondes (signée).
    #[must_use]
    pub fn delta_ms(self, other: Self) -> i64 {
        (i128::from(self.0) - i128::from(other.0)) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn now_monotonic_enough() {
        let a = Timestamp::now_unix_ms();
        sleep(Duration::from_millis(2));
        let b = Timestamp::now_unix_ms();
        assert!(b.0 >= a.0);
    }
}

//! Types d'événements input et clipboard partagés par les crates capture,
//! injection et switch.

use serde::{Deserialize, Serialize};

/// Bouton de souris physique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum MouseButton {
    /// Bouton gauche.
    Left = 0,
    /// Bouton droit.
    Right = 1,
    /// Bouton du milieu / molette enfoncée.
    Middle = 2,
    /// Bouton latéral 1 (souvent « Précédent »).
    X1 = 3,
    /// Bouton latéral 2 (souvent « Suivant »).
    X2 = 4,
}

/// État instantané d'un bouton (clavier ou souris).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ButtonState {
    /// Appui (descente).
    Down = 0,
    /// Relâche (montée).
    Up = 1,
}

/// Côté d'un écran utilisé pour la détection de bord (switch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Edge {
    /// Bord gauche.
    Left = 0,
    /// Bord droit.
    Right = 1,
    /// Bord haut.
    Top = 2,
    /// Bord bas.
    Bottom = 3,
}

impl Edge {
    /// Renvoie le bord opposé (utile pour le réinjection côté pair distant).
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

/// Phase d'un événement tactile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum TouchPhase {
    /// Doigt posé.
    Began = 0,
    /// Doigt déplacé.
    Moved = 1,
    /// Doigt levé.
    Ended = 2,
    /// Suivi annulé par le système (autre app prend le focus, etc.).
    Cancelled = 3,
}

/// Format reconnu sur le presse-papier partagé.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ClipboardFormat {
    /// Texte UTF-8 brut.
    Text = 0,
    /// Texte enrichi RTF.
    Rtf = 1,
    /// Fragment HTML (souvent envoyé avec plaintext alternatif).
    Html = 2,
    /// Image bitmap PNG.
    Png = 3,
    /// Liste de chemins de fichiers (drag & drop classique).
    FileList = 4,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_opposite_involution() {
        for e in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            assert_eq!(e.opposite().opposite(), e);
        }
    }
}

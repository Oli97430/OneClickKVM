//! `okvm-switch` — logique de basculement entre PCs.
//!
//! Trois mecanismes complementaires :
//!
//! - **Detection de bord** : quand le curseur sort par le bord X du bureau
//!   virtuel local, on bascule vers le pair voisin de cote X dans la grille.
//! - **Hotkeys** : raccourci clavier (par defaut `Ctrl+Alt+Win+1..9`) pour
//!   sauter directement a un pair par index.
//! - **Tactile** : un evenement tactile sur l'ecran d'un pair peut declencher
//!   la bascule (gere cote pair via un message `SwitchEnter`).
//!
//! Le composant central est [`SwitchEngine`] qui traite chaque
//! [`InputMessage::MouseMove`] et eventuellement chaque [`InputMessage::KeyEvent`]
//! pour decider du pair cible.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions, clippy::missing_errors_doc)]

#[cfg(windows)]
pub mod screens;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use okvm_core::{ButtonState, Edge, ScreenInfo};
use okvm_protocol::InputMessage;

/// Identifiant local d'un pair dans la grille (UUID de session ou alias).
pub type GridPeerId = Uuid;

/// Rectangle dans le plan global de la grille.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    /// X minimum (inclusif).
    pub x: i32,
    /// Y minimum (inclusif).
    pub y: i32,
    /// Largeur.
    pub w: i32,
    /// Hauteur.
    pub h: i32,
}

impl Rect {
    /// `true` si `(px, py)` est strictement a l'exterieur par le bord donne.
    #[must_use]
    pub fn outside_edge(self, px: i32, py: i32, edge: Edge) -> bool {
        match edge {
            Edge::Left => px < self.x,
            Edge::Right => px >= self.x + self.w,
            Edge::Top => py < self.y,
            Edge::Bottom => py >= self.y + self.h,
        }
    }

    /// Renvoie le bord par lequel `(px, py)` sort, s'il sort.
    #[must_use]
    pub fn exit_edge(self, px: i32, py: i32) -> Option<Edge> {
        if self.outside_edge(px, py, Edge::Left) {
            Some(Edge::Left)
        } else if self.outside_edge(px, py, Edge::Right) {
            Some(Edge::Right)
        } else if self.outside_edge(px, py, Edge::Top) {
            Some(Edge::Top)
        } else if self.outside_edge(px, py, Edge::Bottom) {
            Some(Edge::Bottom)
        } else {
            None
        }
    }

    /// Coordonnees du centre.
    #[must_use]
    pub fn center(self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// Description d'un pair dans la grille.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridPeer {
    /// Identifiant.
    pub id: GridPeerId,
    /// Nom convivial.
    pub name: String,
    /// Ecrans de ce pair.
    pub screens: Vec<ScreenInfo>,
    /// Position de l'origine du pair (coin haut-gauche du bureau virtuel local
    /// du pair) dans la grille.
    pub origin_in_grid: (i32, i32),
    /// Index ordinal pour les hotkeys (1..=9).
    pub hotkey_index: Option<u8>,
}

impl GridPeer {
    /// Bounding box du pair dans la grille (union de ses ecrans).
    #[must_use]
    pub fn bbox(&self) -> Rect {
        if self.screens.is_empty() {
            return Rect {
                x: self.origin_in_grid.0,
                y: self.origin_in_grid.1,
                w: 0,
                h: 0,
            };
        }
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for s in &self.screens {
            let sx = self.origin_in_grid.0 + s.origin_x;
            let sy = self.origin_in_grid.1 + s.origin_y;
            min_x = min_x.min(sx);
            min_y = min_y.min(sy);
            max_x = max_x.max(sx + s.width_px as i32);
            max_y = max_y.max(sy + s.height_px as i32);
        }
        Rect {
            x: min_x,
            y: min_y,
            w: max_x - min_x,
            h: max_y - min_y,
        }
    }
}

/// Grille spatiale des pairs.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Grid {
    /// Pairs indexes par leur ID.
    pub peers: HashMap<GridPeerId, GridPeer>,
}

impl Grid {
    /// Trouve le pair dont la bbox contient (`px`, `py`).
    #[must_use]
    pub fn locate(&self, px: i32, py: i32) -> Option<&GridPeer> {
        self.peers
            .values()
            .find(|p| p.bbox().exit_edge(px, py).is_none())
    }

    /// Pair par hotkey index (1..=9).
    #[must_use]
    pub fn by_hotkey(&self, index: u8) -> Option<&GridPeer> {
        self.peers.values().find(|p| p.hotkey_index == Some(index))
    }

    /// Donne le pair voisin par le bord depuis un point donne dans `from`.
    ///
    /// Strategie : on cherche un pair dont la bbox **chevauche** la zone qui
    /// s'etend a l'infini dans la direction `edge` depuis `(px, py)`. Si
    /// plusieurs candidats, on prend celui dont le centre est le plus proche
    /// du point d'origine.
    #[must_use]
    pub fn neighbor(&self, from: GridPeerId, edge: Edge, px: i32, py: i32) -> Option<&GridPeer> {
        let mut best: Option<(&GridPeer, i64)> = None;
        for p in self.peers.values() {
            if p.id == from {
                continue;
            }
            let bb = p.bbox();
            let matches = match edge {
                Edge::Left => bb.x + bb.w <= px && overlaps_y(bb, py),
                Edge::Right => bb.x >= px && overlaps_y(bb, py),
                Edge::Top => bb.y + bb.h <= py && overlaps_x(bb, px),
                Edge::Bottom => bb.y >= py && overlaps_x(bb, px),
            };
            if !matches {
                continue;
            }
            let (cx, cy) = bb.center();
            let dist = ((cx - px) as i64).pow(2) + ((cy - py) as i64).pow(2);
            if best.map_or(true, |(_, d)| dist < d) {
                best = Some((p, dist));
            }
        }
        best.map(|(p, _)| p)
    }
}

fn overlaps_x(bb: Rect, px: i32) -> bool {
    px >= bb.x && px < bb.x + bb.w
}

fn overlaps_y(bb: Rect, py: i32) -> bool {
    py >= bb.y && py < bb.y + bb.h
}

// ===========================================================================
// SwitchEngine
// ===========================================================================

/// Decision prise par le [`SwitchEngine`] apres un evenement input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchDecision {
    /// Aucune action : on reste sur le pair courant.
    Stay,
    /// Bascule vers `target` par le bord `edge`, en entrant a `(enter_x, enter_y)`.
    SwitchTo {
        /// Pair cible.
        target: GridPeerId,
        /// Bord d'entree cote pair cible.
        edge: Edge,
        /// Coordonnees d'entree dans le bureau virtuel local du pair cible.
        enter_x: i32,
        /// Coordonnees d'entree.
        enter_y: i32,
    },
}

/// Moteur de basculement maintenant l'etat "ou est le curseur".
pub struct SwitchEngine {
    /// Grille spatiale.
    pub grid: Grid,
    /// Pair courant ; `None` = local.
    pub current: Option<GridPeerId>,
    /// Si on est local, le curseur est-il a un bord ?
    last_edge_hit: Option<(Edge, Instant)>,
    /// Cooldown entre deux switchs pour eviter le rebond a la bordure.
    pub cooldown: Duration,
    last_switch: Instant,
    /// Bitmask de touches modificatrices requises pour le hotkey de switch
    /// (defaut : Ctrl+Alt+Win = 2 | 4 | 8 = 14).
    pub hotkey_modifiers: u16,
}

impl SwitchEngine {
    /// Cree un moteur "tout local" (curseur ici).
    #[must_use]
    pub fn new(grid: Grid) -> Self {
        Self {
            grid,
            current: None,
            last_edge_hit: None,
            cooldown: Duration::from_millis(80),
            last_switch: Instant::now() - Duration::from_secs(60),
            hotkey_modifiers: 2 | 4 | 8,
        }
    }

    /// Traite un message d'input local et decide d'une bascule.
    ///
    /// `local_bbox` est la bbox du bureau virtuel local dans la grille (pour
    /// detecter quand le curseur tente de sortir).
    pub fn on_input(&mut self, msg: &InputMessage, local_bbox: Rect) -> SwitchDecision {
        match msg {
            InputMessage::MouseMove {
                x_global, y_global, ..
            } => self.on_mouse_move(*x_global, *y_global, local_bbox),
            InputMessage::KeyEvent {
                vk,
                state: ButtonState::Down,
                modifiers,
                ..
            } => self.on_hotkey(*vk, *modifiers),
            _ => SwitchDecision::Stay,
        }
    }

    fn on_mouse_move(&mut self, x: i32, y: i32, local_bbox: Rect) -> SwitchDecision {
        if self.current.is_some() {
            // Le curseur est cote pair distant ; ce sera au pair de detecter
            // sa propre sortie et de nous renvoyer un SwitchLeave. Ici on ne
            // fait rien sur la base d'un MouseMove local (qui est suspendu).
            return SwitchDecision::Stay;
        }
        let now = Instant::now();
        if now.duration_since(self.last_switch) < self.cooldown {
            return SwitchDecision::Stay;
        }
        let Some(edge) = local_bbox.exit_edge(x, y) else {
            self.last_edge_hit = None;
            return SwitchDecision::Stay;
        };

        // Anti-rebond : on attend au moins 20 ms d'edge hit continu avant de bouger.
        let confirmed = match self.last_edge_hit {
            Some((e, when)) if e == edge => now.duration_since(when) >= Duration::from_millis(20),
            _ => {
                self.last_edge_hit = Some((edge, now));
                false
            }
        };
        if !confirmed {
            return SwitchDecision::Stay;
        }

        // Cherche le voisin.
        // local_peer_id = ID logique du PC local ; on utilise Nil UUID comme convention.
        let nil = Uuid::nil();
        let Some(target) = self.grid.neighbor(nil, edge, x, y) else {
            return SwitchDecision::Stay;
        };
        let target_id = target.id;
        let tb = target.bbox();
        // Coordonnees d'entree dans le pair distant (mirror du bord).
        let (enter_x, enter_y) = match edge {
            Edge::Left => (tb.x + tb.w - 1, y.clamp(tb.y, tb.y + tb.h - 1)),
            Edge::Right => (tb.x, y.clamp(tb.y, tb.y + tb.h - 1)),
            Edge::Top => (x.clamp(tb.x, tb.x + tb.w - 1), tb.y + tb.h - 1),
            Edge::Bottom => (x.clamp(tb.x, tb.x + tb.w - 1), tb.y),
        };

        self.last_switch = now;
        self.last_edge_hit = None;
        self.current = Some(target_id);
        SwitchDecision::SwitchTo {
            target: target_id,
            edge: edge.opposite(),
            enter_x,
            enter_y,
        }
    }

    fn on_hotkey(&mut self, vk: u16, modifiers: u16) -> SwitchDecision {
        if modifiers & self.hotkey_modifiers != self.hotkey_modifiers {
            return SwitchDecision::Stay;
        }
        // VK_0=0x30, VK_1=0x31, ..., VK_9=0x39
        if !(0x30..=0x39).contains(&vk) {
            return SwitchDecision::Stay;
        }
        let digit = (vk - 0x30) as u8;
        if digit == 0 {
            // 0 = retour au local.
            self.current = None;
            return SwitchDecision::Stay;
        }
        let Some(target) = self.grid.by_hotkey(digit) else {
            return SwitchDecision::Stay;
        };
        let target_id = target.id;
        let (cx, cy) = target.bbox().center();
        self.current = Some(target_id);
        self.last_switch = Instant::now();
        SwitchDecision::SwitchTo {
            target: target_id,
            edge: Edge::Left, // pas pertinent pour un hotkey
            enter_x: cx,
            enter_y: cy,
        }
    }

    /// Force le retour au local (le pair distant nous signale qu'il sort par
    /// son cote).
    pub fn return_local(&mut self) {
        self.current = None;
        self.last_switch = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_screen(w: u32, h: u32) -> ScreenInfo {
        ScreenInfo {
            index: 0,
            is_primary: true,
            width_px: w,
            height_px: h,
            dpi: 96,
            origin_x: 0,
            origin_y: 0,
        }
    }

    fn make_peer(name: &str, x: i32, y: i32, w: u32, h: u32, hotkey: Option<u8>) -> GridPeer {
        let mut s = make_screen(w, h);
        s.origin_x = 0;
        s.origin_y = 0;
        GridPeer {
            id: Uuid::new_v4(),
            name: name.into(),
            screens: vec![s],
            origin_in_grid: (x, y),
            hotkey_index: hotkey,
        }
    }

    #[test]
    fn rect_exit_edge() {
        let r = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        assert_eq!(r.exit_edge(50, 50), None);
        assert_eq!(r.exit_edge(-1, 50), Some(Edge::Left));
        assert_eq!(r.exit_edge(100, 50), Some(Edge::Right));
        assert_eq!(r.exit_edge(50, -1), Some(Edge::Top));
        assert_eq!(r.exit_edge(50, 100), Some(Edge::Bottom));
    }

    #[test]
    fn neighbor_to_the_right() {
        let mut g = Grid::default();
        let p1 = make_peer("Left", 0, 0, 1920, 1080, None);
        let p2 = make_peer("Right", 1920, 0, 1920, 1080, None);
        let p1id = p1.id;
        let p2id = p2.id;
        g.peers.insert(p1id, p1);
        g.peers.insert(p2id, p2);
        let nb = g.neighbor(p1id, Edge::Right, 1920, 500);
        assert!(nb.is_some());
        assert_eq!(nb.unwrap().id, p2id);
    }

    #[test]
    fn engine_switches_on_right_edge_after_dwell() {
        let mut g = Grid::default();
        let local_bbox = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let p_right = make_peer("R", 1920, 0, 1920, 1080, None);
        let rid = p_right.id;
        g.peers.insert(rid, p_right);

        let mut engine = SwitchEngine::new(g);
        engine.cooldown = Duration::from_millis(0);

        // Premier evenement : touche le bord droit, attend dwell.
        let msg = InputMessage::MouseMove {
            x_global: 1920,
            y_global: 500,
            dx: 1,
            dy: 0,
            screen_idx: 0,
        };
        let d1 = engine.on_input(&msg, local_bbox);
        assert_eq!(d1, SwitchDecision::Stay);
        std::thread::sleep(Duration::from_millis(25));
        let d2 = engine.on_input(&msg, local_bbox);
        match d2 {
            SwitchDecision::SwitchTo { target, edge, .. } => {
                assert_eq!(target, rid);
                assert_eq!(edge, Edge::Left); // opposite de Right
                assert_eq!(engine.current, Some(rid));
            }
            other => panic!("attendu SwitchTo, recu {other:?}"),
        }
    }

    #[test]
    fn engine_hotkey_jumps() {
        let mut g = Grid::default();
        let p = make_peer("hk", 1920, 0, 1920, 1080, Some(2));
        let pid = p.id;
        g.peers.insert(pid, p);
        let mut engine = SwitchEngine::new(g);
        // Ctrl+Alt+Win+2 (= vk 0x32, modifiers 14)
        let msg = InputMessage::KeyEvent {
            vk: 0x32,
            scancode: 0,
            state: ButtonState::Down,
            extended: false,
            modifiers: 14,
        };
        let d = engine.on_input(
            &msg,
            Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
        );
        match d {
            SwitchDecision::SwitchTo { target, .. } => assert_eq!(target, pid),
            other => panic!("attendu SwitchTo, recu {other:?}"),
        }
    }

    #[test]
    fn engine_hotkey_zero_returns_local() {
        let g = Grid::default();
        let mut engine = SwitchEngine::new(g);
        engine.current = Some(Uuid::new_v4());
        let msg = InputMessage::KeyEvent {
            vk: 0x30,
            scancode: 0,
            state: ButtonState::Down,
            extended: false,
            modifiers: 14,
        };
        let _ = engine.on_input(
            &msg,
            Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
        );
        assert!(engine.current.is_none());
    }
}

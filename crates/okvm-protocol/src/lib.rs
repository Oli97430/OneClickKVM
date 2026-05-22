//! `okvm-protocol` — sérialisation et framing du protocole réseau `OneClick` KVM.
//!
//! Voir `docs/PROTOCOL.md` pour le wire format complet. Cette crate :
//!
//! - définit les **structures** des messages handshake (Hello/Finished).
//! - définit les **enums** des messages applicatifs par canal.
//! - implémente le **framing** binaire `[len][channel][nonce_counter][AEAD]`.
//! - fournit `encode_*` et `decode_*` purement fonctionnels.
//!
//! Elle ne s'occupe **pas** du transport (TCP/UDP) ni du chiffrement effectif
//! (voir `okvm-crypto`). Les fonctions de framing prennent en argument un
//! `AeadSession` mutable.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

pub mod consts;
pub mod frame;
pub mod handshake_msg;
pub mod messages;
mod serde_helpers;

pub use consts::{
    Channel, MAX_FRAME_BYTES, MAX_INPUT_EVENTS_PER_S, PROTOCOL_VERSION, TCP_PORT_DEFAULT,
    UDP_DISCOVERY_PORT,
};
pub use frame::{decode_tcp_frame, encode_tcp_frame, FrameError, FrameHeader, FRAME_HEADER_SIZE};
pub use handshake_msg::{
    ClientFinished, ClientHello, ServerFinished, ServerHello, HANDSHAKE_MAGIC,
};
pub use messages::{
    AudioMessage, ChannelDesc, ClipboardItem, Compression, CtrlMessage, DiscoveryBeacon, FileEntry,
    FileMessage, InputMessage, KeyframeReason, RejectReason, Transport, VideoMessage,
};

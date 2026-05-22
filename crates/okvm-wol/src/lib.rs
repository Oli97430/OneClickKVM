//! `okvm-wol` — Wake-on-LAN.
//!
//! Émet un *magic packet* (6 × 0xFF suivi de 16 × adresse MAC) en broadcast UDP
//! sur le port 9 (ou 7) afin de réveiller le PC distant.
//!
//! L'API est synchrone et complète dès maintenant — c'est un module simple.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use okvm_core::{Error, Result};

/// Adresse MAC (6 octets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// Parse un MAC string `aa:bb:cc:dd:ee:ff` ou `aa-bb-cc-dd-ee-ff`.
    pub fn parse(s: &str) -> Result<Self> {
        let cleaned: Vec<&str> = s.split(|c| c == ':' || c == '-').collect();
        if cleaned.len() != 6 {
            return Err(Error::other(format!("MAC invalide: {s}")));
        }
        let mut out = [0u8; 6];
        for (i, byte_str) in cleaned.iter().enumerate() {
            out[i] = u8::from_str_radix(byte_str, 16)
                .map_err(|_| Error::other(format!("byte MAC invalide: {byte_str}")))?;
        }
        Ok(Self(out))
    }
}

impl std::fmt::Display for MacAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

/// Construit le magic packet de 102 octets.
#[must_use]
pub fn build_magic_packet(mac: MacAddr) -> [u8; 102] {
    let mut buf = [0u8; 102];
    for b in buf.iter_mut().take(6) {
        *b = 0xFF;
    }
    for i in 0..16 {
        let off = 6 + i * 6;
        buf[off..off + 6].copy_from_slice(&mac.0);
    }
    buf
}

/// Envoie un magic packet en broadcast IPv4.
///
/// `bind_addr` est l'adresse locale à utiliser (`0.0.0.0` par défaut).
/// `port` est typiquement 9 (Discard Protocol).
pub fn send_magic_packet(mac: MacAddr, bind_addr: IpAddr, port: u16) -> Result<()> {
    let sock = UdpSocket::bind(SocketAddr::new(bind_addr, 0))?;
    sock.set_broadcast(true)?;
    let pkt = build_magic_packet(mac);
    let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), port);
    sock.send_to(&pkt, target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_colon() {
        let m = MacAddr::parse("AA:bb:CC:11:22:33").unwrap();
        assert_eq!(m.0, [0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33]);
    }

    #[test]
    fn parse_dash() {
        let m = MacAddr::parse("aa-bb-cc-dd-ee-ff").unwrap();
        assert_eq!(m.0, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn parse_invalid() {
        assert!(MacAddr::parse("not-a-mac").is_err());
        assert!(MacAddr::parse("aa:bb:cc").is_err());
        assert!(MacAddr::parse("zz:bb:cc:dd:ee:ff").is_err());
    }

    #[test]
    fn magic_packet_structure() {
        let m = MacAddr([0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01]);
        let p = build_magic_packet(m);
        assert_eq!(&p[..6], &[0xFF; 6]);
        for i in 0..16 {
            assert_eq!(&p[6 + i * 6..6 + i * 6 + 6], &m.0);
        }
    }

    #[test]
    fn display_format() {
        let m = MacAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        assert_eq!(m.to_string(), "01:02:03:04:05:06");
    }
}

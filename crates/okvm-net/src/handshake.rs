//! Driver du handshake 4-messages.
//!
//! Combine [`okvm_crypto::HandshakeState`] (transcript + signature) avec
//! [`okvm_protocol::handshake_msg`] (serialisation bincode) sur un
//! `AsyncRead + AsyncWrite` (typiquement `TcpStream`).
//!
//! L'API renvoie un [`HandshakeOutcome`] qui inclut les `AeadSession` deja
//! avancees des compteurs utilises pour ClientFinished/ServerFinished, afin
//! que la couche [`crate::session::Session`] reprenne au bon endroit.
//!
//! Voir `docs/PROTOCOL.md` §2 et `docs/SECURITY.md` §5.

use std::time::Duration;

use bincode::config::Configuration;
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use okvm_core::{Capabilities, DeviceId, IdentityKeypair};
use okvm_crypto::{AeadSession, HandshakeError, HandshakeState};
use okvm_protocol::{
    handshake_msg::{ClientFinished, ClientHello, ServerFinished, ServerHello, HANDSHAKE_MAGIC},
    messages::{ChannelDesc, RejectReason},
    Channel, PROTOCOL_VERSION,
};

fn bincode_cfg() -> Configuration {
    bincode::config::standard()
}

/// Erreurs du driver de handshake.
#[derive(Debug, Error)]
pub enum DriverError {
    /// I/O sous-jacente.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Echec encode/decode bincode ou trame.
    #[error("encode/decode: {0}")]
    Codec(String),
    /// Echec cryptographique.
    #[error("crypto: {0}")]
    Crypto(#[from] HandshakeError),
    /// Magic constant invalide.
    #[error("magic invalide")]
    BadMagic,
    /// Version protocole non supportee.
    #[error("version protocole non supportee: pair = {0}, attendu {1}")]
    BadVersion(u16, u16),
    /// Le pair a refuse la session (cote serveur uniquement).
    #[error("handshake refuse: {0:?}")]
    Rejected(Option<RejectReason>),
    /// Timeout.
    #[error("handshake timeout")]
    Timeout,
}

/// Resultat d'un handshake reussi.
///
/// Le caller recoit :
/// - les `AeadSession` deja prepares (compteurs avances de 1 pour Finished) ;
/// - le hash du transcript final (utile pour pinning / debug) ;
/// - l'identite et les capabilities du pair distant ;
/// - la liste des canaux negocies.
pub struct HandshakeOutcome {
    /// AEAD pour les envois sortants de **ce** pair.
    pub aead_send: AeadSession,
    /// AEAD pour les receptions entrantes vers **ce** pair.
    pub aead_recv: AeadSession,
    /// Identite long-terme du pair distant.
    pub remote_identity: DeviceId,
    /// Capacites annoncees par le pair distant.
    pub remote_capabilities: Capabilities,
    /// Canaux negocies (extrait de `ClientFinished`).
    pub channels: Vec<ChannelDesc>,
    /// Hash du transcript a la fin du handshake.
    pub transcript_hash: [u8; 32],
}

// ===========================================================================
// Helpers : lecture / ecriture d'un message bincode prefixe par u32 BE
// ===========================================================================

async fn write_msg<W, T>(w: &mut W, msg: &T) -> Result<Vec<u8>, DriverError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = bincode::serde::encode_to_vec(msg, bincode_cfg())
        .map_err(|e| DriverError::Codec(e.to_string()))?;
    let len =
        u32::try_from(bytes.len()).map_err(|_| DriverError::Codec("message > u32::MAX".into()))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(bytes)
}

async fn read_msg<R, T>(r: &mut R) -> Result<(T, Vec<u8>), DriverError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > okvm_protocol::MAX_FRAME_BYTES {
        return Err(DriverError::Codec(format!("msg trop grand: {len}")));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let (msg, _): (T, usize) = bincode::serde::decode_from_slice(&buf, bincode_cfg())
        .map_err(|e| DriverError::Codec(e.to_string()))?;
    Ok((msg, buf))
}

// Envoi / reception d'un message AEAD via les helpers d'okvm-protocol.
async fn send_encrypted_msg<W>(
    w: &mut W,
    aead: &mut AeadSession,
    channel: Channel,
    plaintext: &[u8],
) -> Result<(), DriverError>
where
    W: AsyncWrite + Unpin,
{
    let frame = okvm_protocol::encode_tcp_frame(aead, channel, plaintext)
        .map_err(|e| DriverError::Codec(e.to_string()))?;
    w.write_all(&frame).await?;
    w.flush().await?;
    Ok(())
}

async fn recv_encrypted_msg<R>(
    r: &mut R,
    aead: &mut AeadSession,
    expected: Channel,
) -> Result<Vec<u8>, DriverError>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > okvm_protocol::MAX_FRAME_BYTES {
        return Err(DriverError::Codec(format!("frame trop grande: {len}")));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let (hdr, pt) = okvm_protocol::decode_tcp_frame(aead, &buf)
        .map_err(|e| DriverError::Codec(e.to_string()))?;
    if hdr.channel != expected {
        return Err(DriverError::Codec(format!(
            "canal inattendu: recu {:?}, attendu {:?}",
            hdr.channel, expected
        )));
    }
    Ok(pt)
}

// ===========================================================================
// ServerHello sign-and-patch
// ===========================================================================

/// Encode un `ServerHello` avec une signature placeholder, puis renvoie
/// `(bytes complets, offset de la signature 64 octets)`.
///
/// Le layout bincode standard pour `[u8; 64]` via `serialize_bytes` est :
/// `varint(len=64) || 64 octets bruts`. `varint(64)` tient sur 1 octet (0x40)
/// dans l'encodage standard.
fn encode_server_hello_placeholder(sh: &ServerHello) -> Result<(Vec<u8>, usize), DriverError> {
    let bytes = bincode::serde::encode_to_vec(sh, bincode_cfg())
        .map_err(|e| DriverError::Codec(e.to_string()))?;
    if bytes.len() < 65 {
        return Err(DriverError::Codec("ServerHello trop court".into()));
    }
    let sig_offset = bytes.len() - 64;
    debug_assert_eq!(
        bytes[sig_offset - 1],
        0x40,
        "varint pour len=64 attendu en 1 octet 0x40"
    );
    Ok((bytes, sig_offset))
}

// ===========================================================================
// Driver client
// ===========================================================================

/// Conduit le handshake **cote client** (initiateur).
pub async fn drive_client<S>(
    stream: &mut S,
    identity: IdentityKeypair,
    capabilities: Capabilities,
    desired_channels: Vec<ChannelDesc>,
    pairing_pin: Option<&str>,
    handshake_timeout: Duration,
) -> Result<HandshakeOutcome, DriverError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    timeout(
        handshake_timeout,
        drive_client_inner(
            stream,
            identity,
            capabilities,
            desired_channels,
            pairing_pin,
        ),
    )
    .await
    .map_err(|_| DriverError::Timeout)?
}

async fn drive_client_inner<S>(
    stream: &mut S,
    identity: IdentityKeypair,
    capabilities: Capabilities,
    desired_channels: Vec<ChannelDesc>,
    pairing_pin: Option<&str>,
) -> Result<HandshakeOutcome, DriverError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut hs = HandshakeState::start_client(identity);
    let nonce = random_32();
    let eph_pub = hs.local_eph_public();
    let id_pub = hs.local_identity().0;

    let pin_hash = pairing_pin.map(|pin| {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(pin.as_bytes());
        h.update(nonce);
        let out = h.finalize();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&out);
        arr
    });

    let ch = ClientHello {
        magic: HANDSHAKE_MAGIC,
        protocol_version: PROTOCOL_VERSION,
        flags: 0,
        nonce,
        ephemeral_pub: eph_pub,
        identity_pub: id_pub,
        capabilities,
        pairing_pin_hash: pin_hash,
    };

    // === 1. ClientHello (en clair) ===
    let ch_bytes = write_msg(stream, &ch).await?;
    hs.feed_self_client_hello(&ch_bytes)?;

    // === 2. ServerHello (en clair, contient une signature en fin) ===
    let (sh, sh_bytes): (ServerHello, _) = read_msg(stream).await?;
    if sh.magic != HANDSHAKE_MAGIC {
        return Err(DriverError::BadMagic);
    }
    if sh.protocol_version != PROTOCOL_VERSION {
        return Err(DriverError::BadVersion(
            sh.protocol_version,
            PROTOCOL_VERSION,
        ));
    }
    // unsigned_len = total - 64 octets sig. Le varint 0x40 est en `unsigned_len - 1`,
    // mais on inclut ce varint dans la partie "unsigned" pour rester coherent avec
    // l'algo cote serveur (qui signe sur unsigned_part = bytes[..len-64]).
    if sh_bytes.len() < 65 {
        return Err(DriverError::Codec("ServerHello trop court".into()));
    }
    let unsigned_len = sh_bytes.len() - 64;
    debug_assert_eq!(
        sh_bytes[unsigned_len - 1],
        0x40,
        "varint signature attendu = 0x40"
    );
    let unsigned_part = &sh_bytes[..unsigned_len];
    let sig_tail = &sh_bytes[unsigned_len..]; // 64 octets bruts
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(sig_tail);
    // recv_server_hello a deux roles : verifier la sig sur transcript+=unsigned,
    // puis feed signature_bytes dans transcript. On feed les 64 octets de sig
    // (la representation interne du transcript ne se soucie pas du varint, c'est
    // un hash; on choisit ici de feeder exactement les memes octets que ceux
    // que le serveur a feedes — donc 64 bruts SANS le varint, voir cote serveur).
    hs.recv_server_hello(
        unsigned_part,
        sh.ephemeral_pub,
        sh.identity_pub,
        &sig_arr,
        &sig_arr, // feed 64 octets bruts
    )?;

    // === 3. Derive les cles AEAD au point "post ServerHello" ===
    let secrets = hs.derive_session_keys_now(0)?;
    let mut aead_send = AeadSession::new(&secrets.key_c2s, secrets.epoch);
    let mut aead_recv = AeadSession::new(&secrets.key_s2c, secrets.epoch);

    // === 4. ClientFinished (chiffre) ===
    let cf = ClientFinished {
        transcript_signature: hs.sign_transcript(),
        selected_channels: desired_channels,
    };
    let cf_bytes = bincode::serde::encode_to_vec(&cf, bincode_cfg())
        .map_err(|e| DriverError::Codec(e.to_string()))?;
    send_encrypted_msg(stream, &mut aead_send, Channel::Ctrl, &cf_bytes).await?;

    // === 5. ServerFinished (chiffre) ===
    let sf_plain = recv_encrypted_msg(stream, &mut aead_recv, Channel::Ctrl).await?;
    let (sf, _): (ServerFinished, usize) =
        bincode::serde::decode_from_slice(&sf_plain, bincode_cfg())
            .map_err(|e| DriverError::Codec(e.to_string()))?;
    if !sf.accepted {
        return Err(DriverError::Rejected(sf.reason));
    }

    Ok(HandshakeOutcome {
        aead_send,
        aead_recv,
        remote_identity: secrets.remote_identity,
        remote_capabilities: sh.capabilities,
        channels: cf.selected_channels,
        transcript_hash: secrets.transcript_hash,
    })
}

// ===========================================================================
// Driver serveur
// ===========================================================================

/// Conduit le handshake **cote serveur** (receveur).
///
/// `accept_predicate` est appele apres le `ClientHello` et permet de refuser
/// la session selon l'ACL (par exemple : pair inconnu et pairing desactive).
pub async fn drive_server<S, F>(
    stream: &mut S,
    identity: IdentityKeypair,
    capabilities: Capabilities,
    accept_predicate: F,
    handshake_timeout: Duration,
) -> Result<HandshakeOutcome, DriverError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(&ClientHello) -> Result<(), RejectReason> + Send,
{
    timeout(
        handshake_timeout,
        drive_server_inner(stream, identity, capabilities, accept_predicate),
    )
    .await
    .map_err(|_| DriverError::Timeout)?
}

async fn drive_server_inner<S, F>(
    stream: &mut S,
    identity: IdentityKeypair,
    capabilities: Capabilities,
    accept_predicate: F,
) -> Result<HandshakeOutcome, DriverError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce(&ClientHello) -> Result<(), RejectReason>,
{
    let mut hs = HandshakeState::start_server(identity);

    // === 1. ClientHello ===
    let (ch, ch_bytes): (ClientHello, _) = read_msg(stream).await?;
    if ch.magic != HANDSHAKE_MAGIC {
        return Err(DriverError::BadMagic);
    }
    if ch.protocol_version != PROTOCOL_VERSION {
        return Err(DriverError::BadVersion(
            ch.protocol_version,
            PROTOCOL_VERSION,
        ));
    }
    let decision = accept_predicate(&ch);
    hs.recv_client_hello(&ch_bytes, ch.ephemeral_pub, ch.identity_pub)?;

    // === 2. ServerHello : on calcule la signature en patchant le buffer ===
    let nonce = random_32();
    let eph_pub = hs.local_eph_public();
    let id_pub = hs.local_identity().0;
    let mut sh = ServerHello {
        magic: HANDSHAKE_MAGIC,
        protocol_version: PROTOCOL_VERSION,
        flags: 0,
        nonce,
        ephemeral_pub: eph_pub,
        identity_pub: id_pub,
        capabilities: capabilities.clone(),
        pairing_required: false,
        pairing_pin_hash: None,
        signature: [0u8; 64],
    };
    let (mut sh_bytes, sig_offset) = encode_server_hello_placeholder(&sh)?;
    let unsigned_part = &sh_bytes[..sig_offset];
    let signature = hs.sign_server_hello(unsigned_part)?;
    sh.signature = signature;
    // Patch les 64 octets de signature dans le buffer.
    sh_bytes[sig_offset..].copy_from_slice(&signature);
    // Feed la signature (64 octets bruts) dans le transcript local.
    hs.feed_self_server_signature(&signature);

    // Envoie sur le wire.
    let len = u32::try_from(sh_bytes.len())
        .map_err(|_| DriverError::Codec("ServerHello > u32::MAX".into()))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&sh_bytes).await?;
    stream.flush().await?;

    // === 3. Cles AEAD ===
    let secrets = hs.derive_session_keys_now(0)?;
    let mut aead_send = AeadSession::new(&secrets.key_s2c, secrets.epoch);
    let mut aead_recv = AeadSession::new(&secrets.key_c2s, secrets.epoch);

    // === 4. ClientFinished (chiffre) ===
    let cf_plain = recv_encrypted_msg(stream, &mut aead_recv, Channel::Ctrl).await?;
    let (cf, _): (ClientFinished, usize) =
        bincode::serde::decode_from_slice(&cf_plain, bincode_cfg())
            .map_err(|e| DriverError::Codec(e.to_string()))?;
    hs.verify_remote_transcript_sig(&cf.transcript_signature)?;

    // === 5. ServerFinished (chiffre) ===
    let accepted = decision.is_ok();
    let sf = ServerFinished {
        accepted,
        reason: decision.err(),
        udp_ports: Vec::new(),
    };
    let sf_bytes = bincode::serde::encode_to_vec(&sf, bincode_cfg())
        .map_err(|e| DriverError::Codec(e.to_string()))?;
    send_encrypted_msg(stream, &mut aead_send, Channel::Ctrl, &sf_bytes).await?;

    if !accepted {
        return Err(DriverError::Rejected(sf.reason));
    }

    Ok(HandshakeOutcome {
        aead_send,
        aead_recv,
        remote_identity: secrets.remote_identity,
        remote_capabilities: ch.capabilities,
        channels: cf.selected_channels,
        transcript_hash: secrets.transcript_hash,
    })
}

// ===========================================================================
// Helpers RNG
// ===========================================================================

fn random_32() -> [u8; 32] {
    use rand_core::{OsRng, RngCore};
    let mut out = [0u8; 32];
    OsRng.fill_bytes(&mut out);
    out
}

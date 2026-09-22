//! Bounded document-name guard for Hocuspocus 3.4.4 collaboration frames.
//!
//! Standalone and `std`-only so it can be compiled and tested on its own:
//!
//! ```text
//! rustc --test /tmp/neev-frame-guard.rs -o /tmp/neev-frame-guard && /tmp/neev-frame-guard
//! ```
//!
//! # Why this exists
//!
//! A Hocuspocus socket is **not** one document. Every binary frame begins with
//! the document name, and the server routes on it per frame — at the pinned
//! version, `@hocuspocus/server` 3.4.4 `src/ClientConnection.ts:388-405` reads
//! the leading string into a `documentConnections` map (declared line 36) and
//! hands an unseen name to `setUpNewConnection` (line 233), which opens a second
//! document **on the same socket**. The client writes the name first in all
//! eight outgoing message types (`provider/src/OutgoingMessages/*.ts`, e.g.
//! `SyncStepOneMessage.ts:19-20`) and dispatches inbound frames by it
//! (`provider/src/HocuspocusProviderWebsocket.ts:371-379`, `providerMap` keyed
//! by document name).
//!
//! So a proxy that authorizes one resource at the handshake and then relays
//! bytes blindly is authorizing a socket, not a document. This guard makes the
//! relayed frame agree with the authorization.
//!
//! # What it is not
//!
//! Not a protocol parser. It reads exactly one leading length-prefixed string
//! and compares it; everything after that byte range is opaque and untouched.
//! There is deliberately no message-type switch, no Yjs decoding and no state:
//! a parser would rot the moment upstream changed a message body, and this field
//! is the only one the proxy needs an opinion about.
//!
//! # Caller's obligations (the guard cannot express these itself)
//!
//! * Apply it to **binary** frames in **both** directions.
//! * Refuse **text** frames outright. The collaboration protocol is binary; a
//!   text frame is either a different protocol or an attempt to sidestep this
//!   check, and neither is worth relaying.
//! * Leave **ping/pong/close** alone: they are websocket controls, carry no
//!   document name, and dropping them breaks keepalive rather than securing it.
//! * Keep `?room=<id>` pinned on the upstream URL. Docs' y-provider independently
//!   refuses `documentName != room`
//!   (`servers/y-provider/src/servers/hocuspocusServer.ts:19-36`). That is a
//!   second line rather than a substitute: the image is digest-pinned in
//!   `deploy/lasuite/compose.yaml`, so the check is stable for a given
//!   deployment, but it is an upstream control — re-check it on every version
//!   bump, and do not let it stand in for this one.
//! * Keep re-checking Neev's answer for the authorized resource. This guard says
//!   "this frame is about the document you authorized" — not "that authorization
//!   is still valid".

/// Longest document name accepted before the comparison even starts.
///
/// A canonical UUIDv4 is 36 bytes. The cap exists so a hostile length prefix
/// cannot make the guard read (or reserve) anything large: the work done on a
/// malicious frame is bounded by this constant, not by the claimed length.
pub(crate) const MAX_NAME_LEN: usize = 64;

/// Bytes of varint permitted for the length prefix.
///
/// lib0 writes a varUint as 7-bit groups with the high bit as continuation, and
/// its reader (`lib0/decoding.js:241-260`) performs **no minimality check** — it
/// accumulates groups until a byte without the continuation bit. So `[0xa4,
/// 0x00]` is a legitimate, if padded, encoding of 36, and a real
/// `@hocuspocus/server` 3.4.4 accepts a frame carrying it and answers normally
/// (measured, and kept as a fixture below).
///
/// This guard therefore accepts the same thing, deliberately. Rejecting a
/// padded length would close sockets upstream considers valid, and — the reason
/// that matters — it would mean this guard and upstream's decoder disagree about
/// where the name ends. Reading the name by different rules than the server that
/// acts on it is the one failure this guard exists to prevent.
///
/// Two bytes is where the agreement stops: every length this guard can accept is
/// at most [`MAX_NAME_LEN`], which needs one byte minimally and two with a
/// single pad group, so the cap costs nothing legitimate. A deeper pad
/// (`[0xa4, 0x80, 0x00]`) is refused as malformed — upstream would accept it,
/// and the only thing that emits one is something probing the parser, so the
/// disagreement is in the safe direction: this end closes, and nothing is
/// relayed on a reading the server might not share.
const MAX_LENGTH_BYTES: usize = 2;

/// Why a frame was refused. Every variant means "close the socket"; they are
/// distinguished for logs, not for policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameError {
    /// No bytes at all.
    Empty,
    /// The length prefix did not terminate within [`MAX_LENGTH_BYTES`].
    MalformedLength,
    /// The claimed name length exceeds [`MAX_NAME_LEN`].
    LengthTooLarge,
    /// The frame ended inside the name, or carried a name and nothing after it.
    Truncated,
    /// A well-formed frame naming a document this socket was not authorized for.
    WrongDocument,
}

/// Read the leading document name without copying or validating UTF-8.
///
/// Returns the name's bytes and the offset just past it. UTF-8 is deliberately
/// not decoded: the comparison is byte-for-byte against a name the caller
/// already holds, so decoding would add a failure mode without adding a check.
pub(crate) fn document_name(frame: &[u8]) -> Result<(&[u8], usize), FrameError> {
    if frame.is_empty() {
        return Err(FrameError::Empty);
    }

    let mut length: usize = 0;
    let mut shift = 0;
    let mut cursor = 0;
    loop {
        if cursor >= MAX_LENGTH_BYTES || cursor >= frame.len() {
            return Err(FrameError::MalformedLength);
        }
        let byte = frame[cursor];
        cursor += 1;
        length |= ((byte & 0x7f) as usize) << shift;
        if length > MAX_NAME_LEN {
            // Refused on the claimed length, before any read of that size.
            return Err(FrameError::LengthTooLarge);
        }
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }

    let end = cursor + length;
    // A frame that is exactly a name carries no message: upstream reads a type
    // immediately after, so there is nothing legitimate to relay here.
    if end >= frame.len() {
        return Err(FrameError::Truncated);
    }
    Ok((&frame[cursor..end], end))
}

/// Does this binary frame concern exactly the authorized document?
///
/// `authorized` is the canonical resource id the socket was opened for. The
/// comparison is exact: a prefix, a suffix or a differently-cased spelling of
/// that id is a different document.
pub(crate) fn check_binary_frame(frame: &[u8], authorized: &str) -> Result<(), FrameError> {
    let (name, _rest) = document_name(frame)?;
    if name == authorized.as_bytes() {
        Ok(())
    } else {
        Err(FrameError::WrongDocument)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document every fixture below names.
    const DOC: &str = "4101fd21-09ca-49a3-b977-9ca742a4a3a3";
    const OTHER: &str = "11111111-2222-4333-8444-555555555555";

    /// Frames captured from the pinned 3.4.4 packages, not composed by hand.
    ///
    /// The two client frames were produced by calling lib0's `writeVarString` /
    /// `writeVarUint` and `y-protocols`' `writeSyncStep1` in the exact order the
    /// pinned `AuthenticationMessage.ts:19-22` and `SyncStepOneMessage.ts:19-21`
    /// call them, and were then **accepted and answered** by a real
    /// `@hocuspocus/server` 3.4.4 over a real websocket — so the layout is
    /// upstream's, confirmed by upstream's own decoder rather than asserted.
    ///
    /// The three server frames are that server's replies, byte for byte, which
    /// is why they are proof that the inbound direction is prefixed too.
    fn hex(bytes: &str) -> Vec<u8> {
        (0..bytes.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&bytes[i..i + 2], 16).expect("valid hex"))
            .collect()
    }

    // client -> server, Authentication ("probe-token")
    const CLIENT_AUTH: &str = "2434313031666432312d303963612d343961332d623937372d39636137343261346133613302000b70726f62652d746f6b656e";
    // client -> server, first sync step
    const CLIENT_SYNC_STEP_ONE: &str = "2434313031666432312d303963612d343961332d623937372d39636137343261346133613300000701a59b8acc0601";
    // server -> client, Authenticated ("read-write")
    const SERVER_AUTHENTICATED: &str = "2434313031666432312d303963612d343961332d623937372d39636137343261346133613302020a726561642d7772697465";
    // server -> client, sync
    const SERVER_SYNC: &str = "2434313031666432312d303963612d343961332d623937372d39636137343261346133613300000100";
    // server -> client, awareness
    const SERVER_AWARENESS: &str = "2434313031666432312d303963612d343961332d623937372d3963613734326134613361330001020000";
    /// client -> server, Authentication, with the length written as `[0xa4, 0x00]`
    /// instead of `[0x24]`. lib0 reads 36 from both, and a real 3.4.4 server
    /// accepted this exact frame and replied with the usual Authenticated
    /// message — so it is a valid frame, not a malformed one.
    const CLIENT_AUTH_PADDED_LENGTH: &str = "a40034313031666432312d303963612d343961332d623937372d39636137343261346133613302000b70726f62652d746f6b656e";

    fn fixtures() -> [(&'static str, Vec<u8>); 5] {
        [
            ("client auth", hex(CLIENT_AUTH)),
            ("client sync step one", hex(CLIENT_SYNC_STEP_ONE)),
            ("server authenticated", hex(SERVER_AUTHENTICATED)),
            ("server sync", hex(SERVER_SYNC)),
            ("server awareness", hex(SERVER_AWARENESS)),
        ]
    }

    #[test]
    fn real_frames_from_both_directions_name_the_document() {
        for (label, frame) in fixtures() {
            assert_eq!(
                check_binary_frame(&frame, DOC),
                Ok(()),
                "rejected a genuine frame: {label}"
            );
        }
    }

    #[test]
    fn the_layout_is_what_the_fixtures_show() {
        // Documented rather than assumed: one length byte (36 = 0x24), then the
        // name, then the message type. If upstream ever moves the name, this is
        // the test that says so.
        let frame = hex(CLIENT_AUTH);
        assert_eq!(frame[0], 0x24);
        let (name, rest) = document_name(&frame).unwrap();
        assert_eq!(name, DOC.as_bytes());
        assert_eq!(rest, 1 + DOC.len());
        assert!(rest < frame.len(), "a message type must follow the name");
    }

    #[test]
    fn a_genuine_frame_for_another_document_is_refused() {
        for (label, frame) in fixtures() {
            assert_eq!(
                check_binary_frame(&frame, OTHER),
                Err(FrameError::WrongDocument),
                "accepted a frame for the wrong document: {label}"
            );
        }
    }

    #[test]
    fn swapping_the_name_inside_a_real_frame_is_caught() {
        // Exactly the attack: take a frame the socket is allowed to send and
        // rename it to a document it is not.
        let mut frame = hex(CLIENT_SYNC_STEP_ONE);
        frame.splice(1..1 + DOC.len(), OTHER.bytes());
        assert_eq!(
            check_binary_frame(&frame, DOC),
            Err(FrameError::WrongDocument)
        );
        // …and the same frame is of course fine for the document it now names.
        assert_eq!(check_binary_frame(&frame, OTHER), Ok(()));
    }

    #[test]
    fn near_misses_are_not_near_enough() {
        let body = &hex(CLIENT_AUTH)[1 + DOC.len()..];
        for name in [
            &DOC[..35],                                   // truncated
            "4101FD21-09CA-49A3-B977-9CA742A4A3A3",       // upper case
            " 4101fd21-09ca-49a3-b977-9ca742a4a3a3",      // leading space
            "4101fd21-09ca-49a3-b977-9ca742a4a3a3 ",      // trailing space
            "4101fd21-09ca-49a3-b977-9ca742a4a3a33",      // one byte longer
        ] {
            let mut frame = vec![name.len() as u8];
            frame.extend_from_slice(name.as_bytes());
            frame.extend_from_slice(body);
            assert_eq!(
                check_binary_frame(&frame, DOC),
                Err(FrameError::WrongDocument),
                "accepted {name:?}"
            );
        }
    }

    #[test]
    fn malformed_frames_are_refused_without_reading_far() {
        assert_eq!(check_binary_frame(&[], DOC), Err(FrameError::Empty));
        // Length prefix and nothing else.
        assert_eq!(check_binary_frame(&[0x24], DOC), Err(FrameError::Truncated));
        // Name cut short.
        let mut short = vec![0x24];
        short.extend_from_slice(&DOC.as_bytes()[..10]);
        assert_eq!(check_binary_frame(&short, DOC), Err(FrameError::Truncated));
        // Full name, but no message type after it.
        let mut nameless = vec![0x24];
        nameless.extend_from_slice(DOC.as_bytes());
        assert_eq!(
            check_binary_frame(&nameless, DOC),
            Err(FrameError::Truncated)
        );
        // Empty name.
        assert_eq!(
            check_binary_frame(&[0x00, 0x02], DOC),
            Err(FrameError::WrongDocument)
        );
    }

    #[test]
    fn a_padded_length_parses_exactly_as_upstream_parses_it() {
        // Not a curiosity: lib0 does not require minimal encodings, so refusing
        // this would close a socket the server is happy with — and would mean
        // the two ends disagree about where the name ends.
        let frame = hex(CLIENT_AUTH_PADDED_LENGTH);
        assert_eq!(check_binary_frame(&frame, DOC), Ok(()));
        let (name, rest) = document_name(&frame).unwrap();
        assert_eq!(name, DOC.as_bytes());
        assert_eq!(rest, 2 + DOC.len(), "the pad byte is part of the prefix");
        // And it is still only valid for the document it names.
        assert_eq!(
            check_binary_frame(&frame, OTHER),
            Err(FrameError::WrongDocument)
        );
    }

    #[test]
    fn padding_deeper_than_the_cap_is_refused_rather_than_guessed() {
        // `[0xa4, 0x80, 0x00]` is also 36 to lib0. This guard stops at two bytes
        // and closes instead, which is the safe side of a disagreement: nothing
        // is relayed on a reading upstream might not share.
        let mut frame = vec![0xa4, 0x80, 0x00];
        frame.extend_from_slice(DOC.as_bytes());
        frame.push(0x02);
        assert_eq!(
            check_binary_frame(&frame, DOC),
            Err(FrameError::MalformedLength)
        );
    }

    #[test]
    fn a_hostile_length_prefix_is_bounded_work() {
        // Claims ~268 MB. Refused on the claimed length, before any read.
        assert_eq!(
            check_binary_frame(&[0xff, 0xff, 0xff, 0x7f, 0x00], DOC),
            Err(FrameError::LengthTooLarge)
        );
        // Claims 128 (> MAX_NAME_LEN) in two bytes.
        assert_eq!(
            check_binary_frame(&[0x80, 0x01, 0x00], DOC),
            Err(FrameError::LengthTooLarge)
        );
        // Continuation bit set forever: refused as malformed, never looped on.
        assert_eq!(
            check_binary_frame(&[0x80, 0x80, 0x80, 0x80], DOC),
            Err(FrameError::MalformedLength)
        );
        // Continuation bit set on the last available byte.
        assert_eq!(check_binary_frame(&[0x80], DOC), Err(FrameError::MalformedLength));
    }

    #[test]
    fn a_name_that_is_not_utf8_is_refused_rather_than_panicking() {
        let mut frame = vec![0x04, 0xff, 0xfe, 0xfd, 0xfc, 0x02];
        assert_eq!(
            check_binary_frame(&frame, DOC),
            Err(FrameError::WrongDocument)
        );
        frame.truncate(3);
        assert_eq!(check_binary_frame(&frame, DOC), Err(FrameError::Truncated));
    }

    #[test]
    fn the_bound_holds_for_every_single_byte_prefix() {
        // Exhaustive over the first byte: no input panics, and nothing above the
        // cap is ever accepted.
        for first in 0u8..=255 {
            let mut frame = vec![first];
            frame.extend_from_slice(DOC.as_bytes());
            frame.push(0x02);
            match check_binary_frame(&frame, DOC) {
                Ok(()) => assert_eq!(first, DOC.len() as u8),
                Err(FrameError::WrongDocument)
                | Err(FrameError::Truncated)
                | Err(FrameError::LengthTooLarge)
                | Err(FrameError::MalformedLength) => {}
                Err(other) => panic!("unexpected {:?} for length byte {}", other, first),
            }
        }
    }
}

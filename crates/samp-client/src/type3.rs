//! Arizona "type 3" anti-cheat attestation responder (inbound RPC 186 → outbound RPC 187).
//!
//! The server periodically challenges the client to prove it is a real, unmodified game install by
//! HMAC-ing chosen byte regions of its own image. Reversed from `core.asi`
//! (`custom_packet_protocol_handler @0x1001EC20`) and verified byte-exact against live captures
//! (see memory `arizona-type3-attestation`): in practice every observed challenge only samples three
//! **static `RT_RCDATA` resources embedded in `core.asi`** (never live process memory), so a headless
//! client can answer it from embedded copies of those resources.
//!
//! Challenge body (RPC id already stripped):
//!   `[u8 3][16 key][16 nonce][u8 n] n×{u8 type, u8 flag, u32 offset, u16 size} [u8 trailerFlags]`
//! Response body (sent as RPC 187):
//!   `[u8 3][16 nonce][32 HMAC][u16 trailerLen][trailer]`
//! where `HMAC = HMAC_SHA256(key, nonce ‖ SHA256(region_1) ‖ … ‖ SHA256(region_n) ‖ trailer)`.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// Inbound type-3 challenge RPC id.
pub const RPC_TYPE3_CHALLENGE: u8 = 186;
/// Outbound type-3 response RPC id.
pub const RPC_TYPE3_RESPONSE: u8 = 187;

// The three `RT_RCDATA` blobs the server samples, extracted from `core.asi` (flag → resource):
//   flag 0x64 → #100 (32 B), 0x65 → #101 (32 KB), 0xC0 → #57024 (64 B).
const RES_100: &[u8] = include_bytes!("type3_res/res_100.bin");
const RES_101: &[u8] = include_bytes!("type3_res/res_101.bin");
const RES_57024: &[u8] = include_bytes!("type3_res/res_57024.bin");

const FIELD_TYPE_RESOURCE: u8 = 4;

fn resource(flag: u8) -> Option<&'static [u8]> {
    match flag {
        0x64 => Some(RES_100),
        0x65 => Some(RES_101),
        0xC0 => Some(RES_57024),
        _ => None,
    }
}

struct Challenge {
    key: [u8; 16],
    nonce: [u8; 16],
    field_hashes: Vec<[u8; 32]>,
    trailer_flags: u8,
}

/// Parse a challenge and hash each requested region. Returns `None` if the challenge is malformed or
/// requests anything we can't answer statically (a non-resource field type or an unknown resource) —
/// the caller then declines to respond rather than send a wrong answer.
fn parse(body: &[u8]) -> Option<Challenge> {
    if body.first() != Some(&3) || body.len() < 34 {
        return None;
    }
    let key: [u8; 16] = body[1..17].try_into().ok()?;
    let nonce: [u8; 16] = body[17..33].try_into().ok()?;
    let n = body[33] as usize;
    if n > 16 {
        return None;
    }
    let mut field_hashes = Vec::with_capacity(n);
    let mut p = 34;
    for _ in 0..n {
        let field = body.get(p..p + 8)?;
        let (ftype, flag) = (field[0], field[1]);
        let offset = u32::from_le_bytes(field[2..6].try_into().ok()?) as usize;
        let size = u16::from_le_bytes(field[6..8].try_into().ok()?) as usize;
        p += 8;
        if ftype != FIELD_TYPE_RESOURCE {
            return None; // live-memory (type 0/1) reads are not statically answerable
        }
        let region = resource(flag)?.get(offset..offset.checked_add(size)?)?;
        field_hashes.push(Sha256::digest(region).into());
    }
    let trailer_flags = *body.get(p)?;
    Some(Challenge {
        key,
        nonce,
        field_hashes,
        trailer_flags,
    })
}

/// Build the trailer (anti-debug / process-state probes selected by `trailerFlags`). We report a
/// "clean" attestation: the anti-debug words are zero (no debugger) and the `0x40` counter is zero.
/// The trailer is fed to the HMAC and echoed verbatim in the response, so it always self-verifies; the
/// server inspects it only for a clean signal. Bit order matches `sub_1001F970`.
fn build_trailer(flags: u8) -> Vec<u8> {
    let mut t = Vec::new();
    for bit in [0x04u8, 0x08, 0x10] {
        if flags & bit != 0 {
            t.extend_from_slice(&0u32.to_le_bytes());
        }
    }
    if flags & 0x40 != 0 {
        t.extend_from_slice(&0u16.to_le_bytes());
    }
    t
}

fn assemble(
    key: &[u8; 16],
    nonce: &[u8; 16],
    field_hashes: &[[u8; 32]],
    trailer: &[u8],
) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(nonce);
    for h in field_hashes {
        mac.update(h);
    }
    mac.update(trailer);
    let tag = mac.finalize().into_bytes();

    let mut resp = Vec::with_capacity(1 + 16 + 32 + 2 + trailer.len());
    resp.push(3);
    resp.extend_from_slice(nonce);
    resp.extend_from_slice(&tag);
    resp.extend_from_slice(&(trailer.len() as u16).to_le_bytes());
    resp.extend_from_slice(trailer);
    resp
}

/// Build the RPC 187 response body for a type-3 challenge (RPC 186 body, id already stripped), or
/// `None` if the challenge can't be answered from static resources.
pub fn respond(body: &[u8]) -> Option<Vec<u8>> {
    let c = parse(body)?;
    let trailer = build_trailer(c.trailer_flags);
    Some(assemble(&c.key, &c.nonce, &c.field_hashes, &trailer))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    /// Real captured challenge/response pair (Bumble Bee live, RPC 186→187). Verifies parse + per-field
    /// SHA256 + HMAC reproduce the genuine client's response when fed the same trailer.
    #[test]
    fn matches_real_capture() {
        let challenge = hx("03 32 A6 EF 02 A1 F6 D7 FD D2 B7 58 16 53 7E 74 98 \
             C6 0D AE 70 0A 5B 82 24 42 51 D8 73 1B 3A FA 20 \
             03 04 64 00 00 00 00 20 00 04 C0 00 00 00 00 40 00 04 65 00 00 00 00 00 01 44");
        let expected = hx(
            "03 C6 0D AE 70 0A 5B 82 24 42 51 D8 73 1B 3A FA 20 \
             F8 DC 70 F0 8F 17 26 4A 3F 4D 89 19 7D 67 CB 1A 5F 34 96 94 CF 08 1A AA 8D 60 69 4C E7 22 44 69 \
             06 00 01 00 00 00 5C 01",
        );
        let c = parse(&challenge).expect("parse");
        // Use the real client's trailer (process-state) so the HMAC is over identical input.
        let trailer = hx("01 00 00 00 5C 01");
        let resp = assemble(&c.key, &c.nonce, &c.field_hashes, &trailer);
        assert_eq!(resp, expected);
    }

    #[test]
    fn declines_live_memory_challenge() {
        // [3][16 key][16 nonce][1 field][type=0 (gta_sa.exe memory), flag, off, size][trailer].
        // A type-0 live-memory read can't be answered statically → None.
        let challenge = hx("03 \
             00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF \
             00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF \
             01 00 64 00 00 00 00 20 00 44");
        assert!(respond(&challenge).is_none());
    }
}

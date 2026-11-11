// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! The post-quantum shapes chain-viz reads (the quantum line, `rpcversion` 7; the workspace's
//! `docs/plans/yellowback-quantum-spec.md`): the two output templates that name a PQ key, the
//! owner's `pqkeyid`, the PQ address encodings and the chunked PQ signature stack of a scriptSig.
//! Display only: the node stays the authority (`Solver`, `ycash-dd/src/script/standard.cpp`).
//!
//! - `TX_PQPKH` (spec §2.1): `20 <keyHash:32> 51|52 c2`, 35 bytes (`OP_1`/`OP_2` is the scheme).
//! - `TX_PQCHANNEL` (spec header, D-Q-19): `63 20<h> 5n c2 69 21<serverKey:33> ac 67 <refundHeight>
//!   b1 75 20<h> 5n c2 68`, both client slots byte-identical, 112–117 bytes.
//! - A `pqkeyid` is `scheme ‖ keyHash` (66 hex digits), the bytes a V pushes and a v4 MINT carries.
//! - Addresses are Base58Check(version ‖ keyHash), 53 characters, one 3-byte version per chain,
//!   coin and scheme (decision D-9, spec §4.1, `rpcversion` 9): the plain-YEC PQ address
//!   (`sqf…`/`sqs…`, `tqf…`/`tqs…`, `rqf…`/`rqs…`, D-Q-20, what `getblock`'s
//!   `scriptPubKey.addresses` spells for a PQPKH output, `key_io.cpp`) and the PQ Yellowback
//!   address (`ybf…`/`ybs…`, `ytf…`/`yts…`, `yrf…`/`yrs…`, spec §4 and D-Q-21, what `ownerAddress`
//!   and the wallet RPC that makes a new owner key spell).

use sha2::{Digest, Sha256};

pub const SCHEME_SLH_DSA_SHA2_128S: u8 = 0x01;
pub const SCHEME_FN_DSA_512: u8 = 0x02;
const OP_1: u8 = 0x51;
const OP_2: u8 = 0x52;
const OP_IF: u8 = 0x63;
const OP_ELSE: u8 = 0x67;
const OP_ENDIF: u8 = 0x68;
const OP_VERIFY: u8 = 0x69;
const OP_DROP: u8 = 0x75;
const OP_CHECKSIG: u8 = 0xac;
const OP_CHECKLOCKTIMEVERIFY: u8 = 0xb1;
pub const OP_CHECKPQSIG: u8 = 0xc2;

/// `pq::PubKeySize` / `pq::SigSize` without the hashtype byte (`src/crypto/pq/scheme.h`).
pub fn pubkey_size(scheme: u8) -> usize {
    match scheme {
        SCHEME_SLH_DSA_SHA2_128S => 32,
        SCHEME_FN_DSA_512 => 897,
        _ => 0,
    }
}
pub fn sig_size(scheme: u8) -> usize {
    match scheme {
        SCHEME_SLH_DSA_SHA2_128S => 7856,
        SCHEME_FN_DSA_512 => 666,
        _ => 0,
    }
}

/// The scheme's display name; `None` for an unregistered scheme.
pub fn scheme_name(scheme: u8) -> Option<&'static str> {
    match scheme {
        SCHEME_SLH_DSA_SHA2_128S => Some("SLH-DSA-SHA2-128s"),
        SCHEME_FN_DSA_512 => Some("FN-DSA-512"),
        _ => None,
    }
}

fn scheme_op(b: u8) -> Option<u8> {
    match b {
        OP_1 => Some(SCHEME_SLH_DSA_SHA2_128S),
        OP_2 => Some(SCHEME_FN_DSA_512),
        _ => None,
    }
}

/// A `TX_PQPKH` script's scheme and key hash (exact match, spec §2.1).
pub fn pqpkh(script: &[u8]) -> Option<(u8, [u8; 32])> {
    if script.len() != 35 || script[0] != 0x20 || script[34] != OP_CHECKPQSIG {
        return None;
    }
    Some((scheme_op(script[33])?, script[1..33].try_into().ok()?))
}

/// The parts of a `TX_PQCHANNEL` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PqChannel {
    pub scheme: u8,
    pub client_hash: [u8; 32],
    pub server_key: [u8; 33],
    pub refund_height: u32,
}

/// A `TX_PQCHANNEL` script (D-Q-19), else `None`. The refund height is a minimal positive
/// CScriptNum below 500,000,000; the server key a compressed-key shape (02/03, not checked on the curve).
pub fn pqchannel(s: &[u8]) -> Option<PqChannel> {
    if s.len() < 112 || s.len() > 117 {
        return None;
    }
    if s[0] != OP_IF || s[1] != 0x20 || s[35] != OP_CHECKPQSIG || s[36] != OP_VERIFY || s[37] != 0x21 || s[71] != OP_CHECKSIG || s[72] != OP_ELSE {
        return None;
    }
    let scheme = scheme_op(s[34])?;
    if s[38] != 0x02 && s[38] != 0x03 {
        return None;
    }
    let n = s[73] as usize;
    if !(1..=5).contains(&n) || s.len() != 112 + n {
        return None;
    }
    let num = &s[74..74 + n];
    // minimal and positive: the top byte carries no sign bit and is not a needless 0x00
    if num[n - 1] & 0x80 != 0 || (num[n - 1] == 0 && (n == 1 || num[n - 2] & 0x80 == 0)) {
        return None;
    }
    let height = num.iter().rev().fold(0u64, |acc, b| (acc << 8) | *b as u64);
    if height == 0 || height >= 500_000_000 {
        return None;
    }
    let t = &s[74 + n..];
    // b1 75 20 <h> 5n c2 68
    if t.len() != 38 || t[0] != OP_CHECKLOCKTIMEVERIFY || t[1] != OP_DROP || t[2] != 0x20 || t[35] != s[34] || t[36] != OP_CHECKPQSIG || t[37] != OP_ENDIF || t[3..35] != s[2..34] {
        return None;
    }
    Some(PqChannel { scheme, client_hash: s[2..34].try_into().ok()?, server_key: s[38..71].try_into().ok()?, refund_height: height as u32 })
}

/// A `pqkeyid` (`scheme ‖ keyHash`, 66 hex digits) of a registered scheme.
pub fn parse_pqkeyid(hex_id: &str) -> Option<(u8, [u8; 32])> {
    let b = hex::decode(hex_id).ok()?;
    if b.len() != 33 || scheme_name(b[0]).is_none() {
        return None;
    }
    Some((b[0], b[1..].try_into().ok()?))
}

/// Which post-quantum address a version names: the plain-YEC form (`sq…`/`tq…`/`rq…`, D-Q-20, a
/// `TX_PQPKH` payee) or the PQ Yellowback form (`yb…`/`yt…`/`yr…`, spec §4 and D-Q-21, an owner).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PqCoin {
    Yec,
    Yed,
}

/// Every PQ address version (quantum spec §4.1, decision D-9): one 3-byte version per
/// (`getblockchaininfo.chain`, coin, scheme), so the prefix names the scheme (`f` FN-DSA-512, `s`
/// SLH-DSA-SHA2-128s). One table for both directions; the node's `PlainPQAddressVersions` and
/// Yellowback `pqAddressVersions` (ycash-dd / ycash6 `up/b2-pq-prefix`). The pre-D-9 form
/// (2-byte version ‖ scheme ‖ keyHash) is not in it and does not decode.
pub const PQ_VERSIONS: [([u8; 3], &str, PqCoin, u8); 12] = [
    ([0x4D, 0xDD, 0x04], "main", PqCoin::Yec, SCHEME_FN_DSA_512),           // sqf…
    ([0x4D, 0xDE, 0x6A], "main", PqCoin::Yec, SCHEME_SLH_DSA_SHA2_128S),    // sqs…
    ([0x4F, 0x65, 0x19], "test", PqCoin::Yec, SCHEME_FN_DSA_512),           // tqf…
    ([0x4F, 0x66, 0x7F], "test", PqCoin::Yec, SCHEME_SLH_DSA_SHA2_128S),    // tqs…
    ([0x4C, 0x54, 0xEF], "regtest", PqCoin::Yec, SCHEME_FN_DSA_512),        // rqf…
    ([0x4C, 0x56, 0x55], "regtest", PqCoin::Yec, SCHEME_SLH_DSA_SHA2_128S), // rqs…
    ([0x56, 0xAE, 0xDE], "main", PqCoin::Yed, SCHEME_FN_DSA_512),           // ybf…
    ([0x56, 0xB0, 0x44], "main", PqCoin::Yed, SCHEME_SLH_DSA_SHA2_128S),    // ybs…
    ([0x57, 0x21, 0xC9], "test", PqCoin::Yed, SCHEME_FN_DSA_512),           // ytf…
    ([0x57, 0x23, 0x30], "test", PqCoin::Yed, SCHEME_SLH_DSA_SHA2_128S),    // yts…
    ([0x57, 0x14, 0x44], "regtest", PqCoin::Yed, SCHEME_FN_DSA_512),        // yrf…
    ([0x57, 0x15, 0xAA], "regtest", PqCoin::Yed, SCHEME_SLH_DSA_SHA2_128S), // yrs…
];

/// The 3-byte version of a PQ address on `chain` (`getblockchaininfo.chain`); `None` for another
/// chain or an unregistered scheme.
pub fn version(chain: &str, coin: PqCoin, scheme: u8) -> Option<[u8; 3]> {
    PQ_VERSIONS.iter().find(|(_, c, k, s)| *c == chain && *k == coin && *s == scheme).map(|(v, ..)| *v)
}

/// Whether `chain` has PQ addresses (a known `getblockchaininfo.chain`).
pub fn has_versions(chain: &str) -> bool {
    PQ_VERSIONS.iter().any(|(_, c, ..)| *c == chain)
}

/// Base58Check(version ‖ keyHash), 53 characters; `None` where [`version`] is.
pub fn encode(chain: &str, coin: PqCoin, scheme: u8, hash: &[u8; 32]) -> Option<String> {
    let mut p = Vec::with_capacity(39);
    p.extend_from_slice(&version(chain, coin, scheme)?);
    p.extend_from_slice(hash);
    let check = Sha256::digest(Sha256::digest(&p));
    p.extend_from_slice(&check[..4]);
    Some(bs58::encode(p).into_string())
}

/// A PQ address's chain, coin, scheme and key hash: 35 bytes under a version of [`PQ_VERSIONS`],
/// checksum checked; `None` for anything else.
pub fn decode(addr: &str) -> Option<(&'static str, PqCoin, u8, [u8; 32])> {
    let b = bs58::decode(addr).into_vec().ok()?;
    if b.len() != 39 {
        return None;
    }
    let check = Sha256::digest(Sha256::digest(&b[..35]));
    if check[..4] != b[35..] {
        return None;
    }
    let (_, chain, coin, scheme) = PQ_VERSIONS.iter().find(|(v, ..)| v[..] == b[..3])?;
    Some((chain, *coin, *scheme, b[3..35].try_into().ok()?))
}

fn pushes(script: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    // (opcode, data) for every push-only element; None at the first non-push opcode
    let mut out = Vec::new();
    let mut i = 0;
    while i < script.len() {
        let op = script[i];
        let (len, off) = match op {
            0x00 => (0, 1),
            1..=75 => (op as usize, 1),
            0x4c => (*script.get(i + 1)? as usize, 2),
            0x4d => (u16::from_le_bytes([*script.get(i + 1)?, *script.get(i + 2)?]) as usize, 3),
            0x4e => return None,
            0x4f | 0x51..=0x60 => (0, 1),
            _ => return None,
        };
        let data = script.get(i + off..i + off + len)?;
        out.push((op, data));
        i += off + len;
    }
    Some(out)
}

fn small_int(op: u8) -> Option<usize> {
    (OP_1..=0x60).contains(&op).then(|| (op - 0x50) as usize)
}

/// The scheme of a chunked PQ signature stack (`<sig_1>…<sig_s> <s> <pk_1>…<pk_p> <p>`, quantum
/// plan §4.2) found anywhere in a push-only scriptSig: a PQPKH spend, a vault or intent owner spend
/// (selector after it), a channel close (server signature before it, selector after). `None` when
/// the scriptSig carries no such stack.
pub fn scriptsig_scheme(script_sig: &[u8]) -> Option<u8> {
    let el = pushes(script_sig)?;
    for j in 0..el.len() {
        let Some(p) = small_int(el[j].0) else { continue };
        let Some(pk_start) = j.checked_sub(p) else { continue };
        let pk_len: usize = el[pk_start..j].iter().map(|e| e.1.len()).sum();
        let scheme = match (p, pk_len) {
            (1, 32) => SCHEME_SLH_DSA_SHA2_128S,
            (2, 897) => SCHEME_FN_DSA_512,
            _ => continue,
        };
        let Some(s_at) = pk_start.checked_sub(1) else { continue };
        let Some(s) = small_int(el[s_at].0) else { continue };
        let Some(sig_start) = s_at.checked_sub(s) else { continue };
        let sig_len: usize = el[sig_start..s_at].iter().map(|e| e.1.len()).sum();
        if sig_len == sig_size(scheme) + 1 {
            return Some(scheme);
        }
    }
    None
}

/// The scriptSig bytes of a block's inputs that carry a PQ signature stack (from `getblock … 2`).
pub fn block_pq_bytes(b: &crate::rpc::BlockFull) -> u64 {
    let mut n = 0u64;
    for t in &b.tx {
        for vin in &t.vin {
            let Some(h) = vin.get("scriptSig").and_then(|x| x.get("hex")).and_then(|x| x.as_str()) else { continue };
            // the smallest PQ stack (Falcon, 1,577 bytes) is far above any EC scriptSig: skip those unparsed
            if h.len() < 2 * 1500 {
                continue;
            }
            if let Some(bytes) = hex::decode(h).ok().filter(|b| scriptsig_scheme(b).is_some()) {
                n += bytes.len() as u64;
            }
        }
    }
    n
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn pqpkh_script(scheme: u8, hash: [u8; 32]) -> Vec<u8> {
        let mut s = vec![0x20];
        s.extend_from_slice(&hash);
        s.extend_from_slice(&[0x50 + scheme, OP_CHECKPQSIG]);
        s
    }

    pub fn channel_script(scheme: u8, hash: [u8; 32], refund: &[u8]) -> Vec<u8> {
        let mut s = vec![OP_IF, 0x20];
        s.extend_from_slice(&hash);
        s.extend_from_slice(&[0x50 + scheme, OP_CHECKPQSIG, OP_VERIFY, 0x21, 0x02]);
        s.extend_from_slice(&[0x11; 32]);
        s.extend_from_slice(&[OP_CHECKSIG, OP_ELSE, refund.len() as u8]);
        s.extend_from_slice(refund);
        s.extend_from_slice(&[OP_CHECKLOCKTIMEVERIFY, OP_DROP, 0x20]);
        s.extend_from_slice(&hash);
        s.extend_from_slice(&[0x50 + scheme, OP_CHECKPQSIG, OP_ENDIF]);
        s
    }

    fn push(s: &mut Vec<u8>, d: &[u8]) {
        match d.len() {
            n @ 1..=75 => s.push(n as u8),
            n @ 76..=255 => s.extend_from_slice(&[0x4c, n as u8]),
            n => s.extend_from_slice(&[0x4d, n as u8, (n >> 8) as u8]),
        }
        s.extend_from_slice(d);
    }

    /// The chunked stack (every chunk but the last exactly 520 bytes), then `tail` opcodes.
    pub fn pq_scriptsig(scheme: u8, head: &[&[u8]], tail: &[u8]) -> Vec<u8> {
        let mut s = Vec::new();
        for h in head {
            push(&mut s, h);
        }
        let sig = vec![0xa5; sig_size(scheme) + 1];
        let pk = vec![0x5a; pubkey_size(scheme)];
        for c in sig.chunks(520) {
            push(&mut s, c);
        }
        s.push(0x50 + sig.chunks(520).count() as u8);
        for c in pk.chunks(520) {
            push(&mut s, c);
        }
        s.push(0x50 + pk.chunks(520).count() as u8);
        s.extend_from_slice(tail);
        s
    }

    #[test]
    fn pqpkh_shapes() {
        let s = pqpkh_script(1, [7; 32]);
        assert_eq!(s.len(), 35);
        assert_eq!(pqpkh(&s), Some((1, [7; 32])));
        assert_eq!(pqpkh(&pqpkh_script(2, [7; 32])), Some((2, [7; 32])));
        // scheme 3, the 36-byte `01 01` form and a short script are not PQPKH
        assert_eq!(pqpkh(&pqpkh_script(3, [7; 32])), None);
        let mut long = vec![0x20];
        long.extend_from_slice(&[7; 32]);
        long.extend_from_slice(&[0x01, 0x01, OP_CHECKPQSIG]);
        assert_eq!(pqpkh(&long), None);
        assert_eq!(pqpkh(&s[..34]), None);
    }

    #[test]
    fn channel_shapes() {
        let s = channel_script(1, [9; 32], &[0x90, 0x01]); // 400
        assert_eq!(s.len(), 114);
        let c = pqchannel(&s).unwrap();
        assert_eq!((c.scheme, c.client_hash, c.refund_height), (1, [9; 32], 400));
        assert_eq!(c.server_key[0], 0x02);
        assert_eq!(pqchannel(&channel_script(2, [9; 32], &[0x10])).unwrap().refund_height, 16);
        assert_eq!(channel_script(2, [9; 32], &[0x10]).len(), 113);
        // non-minimal, negative, zero, two different client slots
        assert!(pqchannel(&channel_script(1, [9; 32], &[0x10, 0x00])).is_none());
        assert!(pqchannel(&channel_script(1, [9; 32], &[0x81])).is_none());
        assert!(pqchannel(&channel_script(1, [9; 32], &[0x00])).is_none());
        let mut other = channel_script(1, [9; 32], &[0x10]);
        let n = other.len();
        other[n - 5] ^= 1;
        assert!(pqchannel(&other).is_none());
        // a scheme mismatch between the slots
        let mut mixed = channel_script(1, [9; 32], &[0x10]);
        let n = mixed.len();
        mixed[n - 3] = OP_2;
        assert!(pqchannel(&mixed).is_none());
    }

    #[test]
    fn addresses() {
        // every PQ address is 53 characters and its first three characters name chain, coin and scheme
        for chain in ["main", "test", "regtest"] {
            for (scheme, h) in [(1u8, [0u8; 32]), (2, [0xff; 32])] {
                let y = encode(chain, PqCoin::Yed, scheme, &h).unwrap();
                let p = encode(chain, PqCoin::Yec, scheme, &h).unwrap();
                assert_eq!((y.len(), p.len()), (53, 53), "{} {}", y, p);
                let f = if scheme == 2 { "f" } else { "s" };
                let want_y = match chain {
                    "main" => "yb",
                    "test" => "yt",
                    _ => "yr",
                };
                let want_p = match chain {
                    "main" => "sq",
                    "test" => "tq",
                    _ => "rq",
                };
                assert!(y.starts_with(&format!("{want_y}{f}")) && p.starts_with(&format!("{want_p}{f}")), "{} {}", y, p);
                assert_eq!(decode(&y), Some((chain, PqCoin::Yed, scheme, h)));
                assert_eq!(decode(&p), Some((chain, PqCoin::Yec, scheme, h)));
            }
        }
        assert_eq!(encode("main", PqCoin::Yed, 3, &[0; 32]), None);
        assert_eq!(encode("other", PqCoin::Yec, 1, &[0; 32]), None);
        // the pre-D-9 form (0x5710 ‖ scheme ‖ keyHash; 0x4DD9 ‖ …) does not decode
        assert!(decode("yr2Tp5VfX9aERxy2khaRBpd7c3FY7nnPNXFgWE9ix6TPjLE4fqeRP").is_none());
        assert!(decode("sq4couNBfnJoRxqXF89SWjiPh5wktFi5bb5S4F89xMMsCrqRf2L4z").is_none());
        assert!(decode("smJQihTy3t6QqGdNimrrWhN1aF8SbwrWmT4").is_none());
        assert_eq!(parse_pqkeyid(&format!("01{}", "ab".repeat(32))), Some((1, [0xab; 32])));
        assert_eq!(parse_pqkeyid(&format!("03{}", "ab".repeat(32))), None);
    }

    #[test]
    fn scriptsig_stacks() {
        // PQPKH spends: SLH 16 sig chunks (15 × 520 + 57), Falcon 2 + 2
        let slh = pq_scriptsig(1, &[], &[]);
        assert_eq!(slh.len(), 7938);
        assert_eq!(scriptsig_scheme(&slh), Some(1));
        let fal = pq_scriptsig(2, &[], &[]);
        assert_eq!(fal.len(), 1577);
        assert_eq!(scriptsig_scheme(&fal), Some(2));
        // a vault owner spend (selector OP_2 / OP_3 after the stack), a channel close (server sig before)
        assert_eq!(scriptsig_scheme(&pq_scriptsig(1, &[], &[0x52])), Some(1));
        assert_eq!(scriptsig_scheme(&pq_scriptsig(2, &[&[0x30; 71]], &[0x51])), Some(2));
        // an ECDSA P2PKH scriptSig, an empty one, a non-push
        let mut ec = vec![71];
        ec.extend_from_slice(&[0x30; 71]);
        ec.push(33);
        ec.extend_from_slice(&[0x02; 33]);
        assert_eq!(scriptsig_scheme(&ec), None);
        assert_eq!(scriptsig_scheme(&[]), None);
        assert_eq!(scriptsig_scheme(&[0x76]), None);
        // a block: one SLH owner spend and one EC input count the SLH scriptSig only
        let b: crate::rpc::BlockFull = serde_json::from_value(serde_json::json!({"hash": "h", "height": 1, "confirmations": 1, "size": 9000, "time": 0, "chainwork": "",
            "tx": [{"txid": "a", "vin": [{"scriptSig": {"hex": hex::encode(pq_scriptsig(1, &[], &[0x52]))}}, {"scriptSig": {"hex": hex::encode(&ec)}}], "vout": []},
                   {"txid": "cb", "vin": [{"coinbase": "00"}], "vout": []}]}))
        .unwrap();
        assert_eq!(block_pq_bytes(&b), 7939);
    }
}

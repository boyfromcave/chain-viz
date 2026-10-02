// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! Recognise a Yellowback transaction WITHOUT the index (plan §3.2.3), from a decoded
//! transaction's outputs alone, so the collector spends one `yed_*` call per Yellowback tx and
//! none per ordinary tx.
//!
//! The marker is the payload output, mirrored from the node's own detector:
//!
//! - `FindOpReturn` (`ycash-dd/src/yellowback/payload.cpp:398-408`): exactly one output whose
//!   script starts with `OP_RETURN` (0x6a); two or more => non-Yellowback.
//! - `ExtractOpReturnData` (`payload.cpp:386-396`): that script is `OP_RETURN <one data push>`
//!   and nothing else; the push is a direct 1..75-byte push or `OP_PUSHDATA1/2/4`, never `OP_0`,
//!   `OP_1..16`; the data is 4..80 bytes (`MIN_PAYLOAD`/`MAX_PAYLOAD`, `params.h:49-50`).
//! - `DecodePayload` (`payload.cpp:364-378`, layout in `payload.h:19-45`): magic `0x59 0x42`
//!   ("YB", `params.h:43-44`), version `0x03` (`params.h:45`; versions 1, 2 and later are
//!   non-Yellowback, V23), then the type byte: 0x01 MINT, 0x02 TRANSFER, 0x03 REDEEM,
//!   0x05 ATTESTOR_REGISTER, 0x06 CLAIM_NOTICE, 0x07 EQUIVOCATION, 0x08 ATTESTOR_REVIVE; any
//!   other type is non-Yellowback (the forward-compatibility rule). Bodies are fixed-width
//!   little-endian; a wrong body length is malformed (=> non-Yellowback for the node too).
//!
//! What this module decodes is the part the UI shows before any RPC answers (type, cents,
//! assignments, the fee vouts); the node's `yed_decodepayload` / `yed_gettxinfo` stay the
//! authority and the collector attaches their answer beside it.

use serde::{Deserialize, Serialize};

use crate::rpc::RawTransaction;

pub const MAGIC: [u8; 2] = [0x59, 0x42];
pub const VERSION: u8 = 0x03;
pub const MIN_PAYLOAD: usize = 4;
pub const MAX_PAYLOAD: usize = 80;
const OP_RETURN: u8 = 0x6a;
const OP_PUSHDATA1: u8 = 0x4c;
const OP_PUSHDATA2: u8 = 0x4d;
const OP_PUSHDATA4: u8 = 0x4e;

/// The payload type names as `yed_gettxinfo`/`yed_decodepayload` spell them
/// (`PayloadTypeName`, `payload.cpp:432-442`; `TypeLower`, `rpc/yellowback.cpp:126-138`).
pub fn type_name(code: u8) -> Option<&'static str> {
    Some(match code {
        0x01 => "mint",
        0x02 => "transfer",
        0x03 => "redeem",
        0x05 => "register",
        0x06 => "notice",
        0x07 => "equivocation",
        0x08 => "revive",
        _ => return None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Assignment {
    pub vout: u8,
    pub cents: u32,
}

/// What the payload says on its own.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Payload {
    /// `mint`, `transfer`, `redeem`, `register`, `notice`, `equivocation`, `revive`.
    #[serde(rename = "type")]
    pub tx_type: String,
    pub version: u8,
    /// Index of the `OP_RETURN` output.
    #[serde(rename = "payloadVout")]
    pub payload_vout: u32,
    /// The pushed bytes, hex (what `yed_decodepayload` takes).
    pub hex: String,
    /// MINT: minted cents; TRANSFER/REDEEM: the sum of the assignments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cents: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "termClass")]
    pub term_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "lockHeight")]
    pub lock_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "refHeight")]
    pub ref_height: Option<u32>,
    /// `0xFF` in the payload means "no such output" and is reported as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "feeVout")]
    pub fee_vout: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "attestFeeVout")]
    pub attest_fee_vout: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assignments: Vec<Assignment>,
    /// CLAIM_NOTICE: the vault outpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<String>,
}

/// `OP_RETURN <one data push>` exactly, else `None` (`ExtractOpReturnData`).
pub fn op_return_data(script: &[u8]) -> Option<&[u8]> {
    if script.first() != Some(&OP_RETURN) {
        return None;
    }
    let rest = &script[1..];
    let (len, off): (usize, usize) = match *rest.first()? {
        n @ 1..=75 => (n as usize, 1),
        OP_PUSHDATA1 => (*rest.get(1)? as usize, 2),
        OP_PUSHDATA2 => (u16::from_le_bytes([*rest.get(1)?, *rest.get(2)?]) as usize, 3),
        OP_PUSHDATA4 => (u32::from_le_bytes([*rest.get(1)?, *rest.get(2)?, *rest.get(3)?, *rest.get(4)?]) as usize, 5),
        _ => return None,
    };
    if len == 0 || rest.len() != off + len {
        return None;
    }
    let data = &rest[off..];
    if !(MIN_PAYLOAD..=MAX_PAYLOAD).contains(&data.len()) {
        return None;
    }
    Some(data)
}

/// Decode a payload's bytes (`DecodePayload`); `None` for anything the node calls non-Yellowback.
pub fn decode(data: &[u8], payload_vout: u32) -> Option<Payload> {
    if data.len() < MIN_PAYLOAD || data.len() > MAX_PAYLOAD || data[..2] != MAGIC || data[2] != VERSION {
        return None;
    }
    let name = type_name(data[3])?;
    let body = &data[4..];
    let mut p = Payload {
        tx_type: name.to_string(),
        version: data[2],
        payload_vout,
        hex: hex::encode(data),
        cents: None,
        term_class: None,
        lock_height: None,
        ref_height: None,
        fee_vout: None,
        attest_fee_vout: None,
        assignments: Vec::new(),
        vault: None,
    };
    let u32le = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let vout = |b: u8| if b == 0xff { None } else { Some(b) };
    match data[3] {
        0x01 => {
            // termClass u8, cents u32, lockHeight u32, refHeight u32, ownerPubKey 33, feeVout u8, attestFeeVout u8 (48)
            if body.len() != 48 {
                return None;
            }
            p.term_class = Some(char::from(b'A' + body[0]).to_string());
            p.cents = Some(u32le(&body[1..5]) as u64);
            p.lock_height = Some(u32le(&body[5..9]));
            p.ref_height = Some(u32le(&body[9..13]));
            p.fee_vout = vout(body[46]);
            p.attest_fee_vout = vout(body[47]);
        }
        0x02 => {
            // count u8, count x (vout u8, cents u32)
            let n = *body.first()? as usize;
            if n == 0 || n > 15 || body.len() != 1 + 5 * n {
                return None;
            }
            p.assignments = assignments(&body[1..], n)?;
            p.cents = Some(p.assignments.iter().map(|a| a.cents as u64).sum());
        }
        0x03 => {
            // refHeight u32, feeVout u8, attestFeeVout u8, count u8, count x (vout u8, cents u32)
            if body.len() < 7 {
                return None;
            }
            let n = body[6] as usize;
            if n > 13 || body.len() != 7 + 5 * n {
                return None;
            }
            p.ref_height = Some(u32le(&body[0..4]));
            p.fee_vout = vout(body[4]);
            p.attest_fee_vout = vout(body[5]);
            p.assignments = assignments(&body[7..], n)?;
            p.cents = Some(p.assignments.iter().map(|a| a.cents as u64).sum());
        }
        0x05 => {
            if body.len() != 71 {
                return None;
            }
        }
        0x06 => {
            // vaultTxid 32, vaultVout u8, refHeight u32
            if body.len() != 37 {
                return None;
            }
            let mut txid = body[..32].to_vec();
            txid.reverse();
            p.vault = Some(format!("{}:{}", hex::encode(txid), body[32]));
            p.ref_height = Some(u32le(&body[33..37]));
        }
        0x07 => {
            if !body.is_empty() {
                return None;
            }
        }
        0x08 => {
            if body.len() != 74 {
                return None;
            }
        }
        _ => return None,
    }
    Some(p)
}

fn assignments(b: &[u8], n: usize) -> Option<Vec<Assignment>> {
    let mut out = Vec::with_capacity(n);
    let mut seen = [false; 256];
    for i in 0..n {
        let a = Assignment { vout: b[5 * i], cents: u32::from_le_bytes([b[5 * i + 1], b[5 * i + 2], b[5 * i + 3], b[5 * i + 4]]) };
        if a.cents == 0 || std::mem::replace(&mut seen[a.vout as usize], true) {
            return None;
        }
        out.push(a);
    }
    Some(out)
}

/// `FindPayload` over raw output scripts (hex-decoded): exactly one `OP_RETURN` output, of the
/// required shape, that decodes, whose assigned vouts exist and are not the payload itself.
pub fn find_payload_in_scripts(scripts: &[Vec<u8>]) -> Option<Payload> {
    let mut found = None;
    for (i, s) in scripts.iter().enumerate() {
        if s.first() == Some(&OP_RETURN) {
            if found.is_some() {
                return None;
            }
            found = Some(i);
        }
    }
    let idx = found?;
    let p = decode(op_return_data(&scripts[idx])?, idx as u32)?;
    for a in &p.assignments {
        if a.vout as usize >= scripts.len() || a.vout as usize == idx {
            return None;
        }
    }
    Some(p)
}

/// The same over a decoded transaction (`getrawtransaction … 1`, `getblock … 2`): reads each
/// `vout[i].scriptPubKey.hex`.
pub fn find_payload(tx: &RawTransaction) -> Option<Payload> {
    let scripts: Vec<Vec<u8>> = tx.vout.iter().map(|o| o.script_pub_key.get("hex").and_then(|h| h.as_str()).and_then(|h| hex::decode(h).ok()).unwrap_or_default()).collect();
    find_payload_in_scripts(&scripts)
}

pub fn is_yellowback(tx: &RawTransaction) -> bool {
    find_payload(tx).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(data: &[u8]) -> Vec<u8> {
        let mut s = vec![OP_RETURN, data.len() as u8];
        s.extend_from_slice(data);
        s
    }

    fn mint_payload() -> Vec<u8> {
        let mut d = vec![0x59, 0x42, 0x03, 0x01, 0x00];
        d.extend_from_slice(&100_000u32.to_le_bytes());
        d.extend_from_slice(&380u32.to_le_bytes());
        d.extend_from_slice(&329u32.to_le_bytes());
        d.extend_from_slice(&[0x02; 33]);
        d.extend_from_slice(&[3, 4]);
        d
    }

    #[test]
    fn mint_decodes() {
        let p = find_payload_in_scripts(&[vec![0x76], vec![0x76], script(&mint_payload()), vec![0x76], vec![0x76]]).unwrap();
        assert_eq!(p.tx_type, "mint");
        assert_eq!(p.payload_vout, 2);
        assert_eq!(p.cents, Some(100_000));
        assert_eq!(p.term_class.as_deref(), Some("A"));
        assert_eq!((p.lock_height, p.ref_height, p.fee_vout, p.attest_fee_vout), (Some(380), Some(329), Some(3), Some(4)));
    }

    #[test]
    fn transfer_and_redeem() {
        let d = [0x59, 0x42, 0x03, 0x02, 2, 1, 0x10, 0x27, 0, 0, 2, 0x20, 0x4e, 0, 0];
        let p = find_payload_in_scripts(&[vec![0x76], vec![0x76], vec![0x76], script(&d)]).unwrap();
        assert_eq!(p.tx_type, "transfer");
        assert_eq!(p.assignments.len(), 2);
        assert_eq!(p.cents, Some(10_000 + 20_000));
        // an assignment to the payload vout itself is non-Yellowback
        assert!(find_payload_in_scripts(&[vec![0x76], script(&d), vec![0x76]]).is_none());
        // assignment to a vout that does not exist
        assert!(find_payload_in_scripts(&[vec![0x76], vec![0x76], script(&d)]).is_none());
        let mut r = vec![0x59, 0x42, 0x03, 0x03];
        r.extend_from_slice(&329u32.to_le_bytes());
        r.extend_from_slice(&[2, 0xff, 1, 0, 0x50, 0xc3, 0, 0]);
        let p = find_payload_in_scripts(&[vec![0x76], vec![0x76], vec![0x76], script(&r)]).unwrap();
        assert_eq!(p.tx_type, "redeem");
        assert_eq!((p.ref_height, p.fee_vout, p.attest_fee_vout), (Some(329), Some(2), None));
        assert_eq!(p.cents, Some(50_000));
    }

    #[test]
    fn notice_equivocation_register_revive() {
        let mut n = vec![0x59, 0x42, 0x03, 0x06];
        n.extend_from_slice(&[0xab; 32]);
        n.push(1);
        n.extend_from_slice(&7u32.to_le_bytes());
        let p = decode(&n, 0).unwrap();
        assert_eq!(p.tx_type, "notice");
        assert_eq!(p.vault.as_deref(), Some(&format!("{}:1", "ab".repeat(32))[..]));
        assert_eq!(decode(&[0x59, 0x42, 0x03, 0x07], 0).unwrap().tx_type, "equivocation");
        let mut r = vec![0x59, 0x42, 0x03, 0x05];
        r.extend_from_slice(&[0; 71]);
        assert_eq!(decode(&r, 0).unwrap().tx_type, "register");
        let mut v = vec![0x59, 0x42, 0x03, 0x08];
        v.extend_from_slice(&[0; 74]);
        assert_eq!(decode(&v, 0).unwrap().tx_type, "revive");
        assert_eq!(v.len(), 78);
    }

    #[test]
    fn non_yellowback_shapes() {
        // wrong magic / version / type
        assert!(decode(&[0x59, 0x43, 0x03, 0x07], 0).is_none());
        assert!(decode(&[0x59, 0x42, 0x02, 0x07], 0).is_none());
        assert!(decode(&[0x59, 0x42, 0x03, 0x04], 0).is_none());
        assert!(decode(&[0x59, 0x42, 0x03, 0x10], 0).is_none());
        // short body, trailing bytes
        assert!(decode(&mint_payload()[..40], 0).is_none());
        let mut long = mint_payload();
        long.push(0);
        assert!(decode(&long, 0).is_none());
        // two OP_RETURN outputs
        assert!(find_payload_in_scripts(&[script(&[0x59, 0x42, 0x03, 0x07]), script(&[0x59, 0x42, 0x03, 0x07])]).is_none());
        // OP_RETURN with OP_1 instead of a push, with two pushes, or a bare OP_RETURN
        assert!(op_return_data(&[OP_RETURN, 0x51]).is_none());
        assert!(op_return_data(&[OP_RETURN, 1, 0x59, 1, 0x42]).is_none());
        assert!(op_return_data(&[OP_RETURN]).is_none());
        // PUSHDATA1 form is accepted
        let mut s = vec![OP_RETURN, OP_PUSHDATA1, 4];
        s.extend_from_slice(&[0x59, 0x42, 0x03, 0x07]);
        assert_eq!(op_return_data(&s), Some(&[0x59, 0x42, 0x03, 0x07][..]));
        // 81 bytes is too long
        let mut s = vec![OP_RETURN, OP_PUSHDATA1, 81];
        s.extend_from_slice(&[0; 81]);
        assert!(op_return_data(&s).is_none());
    }
}

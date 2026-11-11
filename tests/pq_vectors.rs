// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! The node lines' shared post-quantum address vectors (decision D-9): `tests/fixtures/pq_address_vectors.json` is
//! a byte-identical copy of ycash-dd / ycash6 `src/test/data/pq_address_vectors.json` (`up/b2-pq-prefix`); its
//! sha256 is pinned here so a drift fails. Every valid vector decodes to its own chain, coin, scheme and key hash and
//! encodes back to itself; every invalid vector (the pre-D-9 form, truncated, extended, bad checksum, unknown
//! version) does not decode.

use chain_viz::pq::{self, PqCoin};
use serde_json::Value;
use sha2::{Digest, Sha256};

const FILE: &[u8] = include_bytes!("fixtures/pq_address_vectors.json");
const FILE_SHA256: &str = "dbbd1da05d995d659e1e9f8a671197e141c272998722d6cca85cdbdb52b61797";

fn vectors() -> Value {
    assert_eq!(hex::encode(Sha256::digest(FILE)), FILE_SHA256, "pq_address_vectors.json drifted from the node lines' file");
    serde_json::from_slice(FILE).expect("parses")
}

#[test]
fn shared_pq_address_vectors() {
    let v = vectors();
    assert_eq!(v["format"], "ycash-pq-address-vectors-1");
    let valid = v["valid"].as_array().unwrap();
    let invalid = v["invalid"].as_array().unwrap();
    assert_eq!((valid.len(), invalid.len()), (72, 60));
    for c in valid {
        let addr = c["address"].as_str().unwrap();
        let chain = c["network"].as_str().unwrap();
        let coin = match c["coin"].as_str().unwrap() {
            "yec" => PqCoin::Yec,
            "yed" => PqCoin::Yed,
            other => panic!("coin {other}"),
        };
        let scheme = c["scheme"].as_u64().unwrap() as u8;
        let hash: [u8; 32] = hex::decode(c["keyhash"].as_str().unwrap()).unwrap().try_into().unwrap();
        assert_eq!(hex::encode(pq::version(chain, coin, scheme).unwrap()), c["version"].as_str().unwrap(), "{addr}");
        assert_eq!(pq::decode(addr), Some((chain, coin, scheme, hash)), "{addr}");
        assert_eq!(pq::encode(chain, coin, scheme, &hash).as_deref(), Some(addr));
    }
    for c in invalid {
        let addr = c["address"].as_str().unwrap();
        assert_eq!(pq::decode(addr), None, "{addr} ({})", c["reason"]);
    }
}

// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! The transparent value pool by address type (beta3 item 2, rpcversion 10): `getblock`'s and
//! `getblockchaininfo`'s `transparentPools` object (ycash6 / ycash-dd `doc/yellowback-rpc.md`,
//! "The `transparentPools` object"). Every entry is found by its `id` (pools), `pqscheme`
//! (`pq_by_scheme`) or `module` (`vault_by_module`), never by position, and so is `valuePools`
//! wherever chain-viz would read it. A node before rpcversion 10 has no such object (`None`); a
//! block the node's index does not cover yet says `"monitored": false` and carries no values.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The pool ids of `transparentPools.pools`, in the node's documented order (display only: an
/// entry is looked up by id, and an id not listed here is kept as well).
pub const POOL_IDS: [&str; 5] = ["transparent_ecc", "transparent_pq", "pq_channel", "vault", "vault_intent"];

/// One part of a split (`pq_by_scheme` or `vault_by_module`): its name and `chainValueZat`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub zat: i64,
}

/// A block's `transparentPools`, reduced to what the UI shows: the chain values in zatoshi.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TransparentPools {
    /// `transparentPools.monitored`: false until the node's index covers the block.
    pub monitored: bool,
    /// `transparent.chainValueZat` (equal to `valuePools`' `transparent` entry).
    #[serde(rename = "transparentZat", default, skip_serializing_if = "Option::is_none")]
    pub transparent_zat: Option<i64>,
    /// `chainValueZat` by pool `id`, the monitored entries only.
    #[serde(default)]
    pub pools: BTreeMap<String, i64>,
    /// `pq_by_scheme`: `pqschemename` (or `scheme <n>`) and its value.
    #[serde(rename = "pqByScheme", default, skip_serializing_if = "Vec::is_empty")]
    pub pq_by_scheme: Vec<Part>,
    /// `vault_by_module`: `module` (or the `tag`) and its value.
    #[serde(rename = "vaultByModule", default, skip_serializing_if = "Vec::is_empty")]
    pub vault_by_module: Vec<Part>,
}

/// An entry's `chainValueZat`, unless the entry says `"monitored": false`.
fn zat(e: &Value) -> Option<i64> {
    if e.get("monitored").and_then(Value::as_bool) == Some(false) {
        return None;
    }
    e.get("chainValueZat").and_then(Value::as_i64)
}

/// `transparentPools` from a `getblock` / `getblockchaininfo` result's fields (its `extra`);
/// `None` when the node does not report it (before rpcversion 10) or reports something else.
pub fn transparent_pools(fields: &Map<String, Value>) -> Option<TransparentPools> {
    let tp = fields.get("transparentPools")?.as_object()?;
    let monitored = tp.get("monitored").and_then(Value::as_bool).unwrap_or(false);
    if !monitored {
        return Some(TransparentPools::default());
    }
    let pools = tp.get("pools").and_then(Value::as_array).map(|a| a.iter().filter_map(|e| Some((e.get("id")?.as_str()?.to_string(), zat(e)?))).collect()).unwrap_or_default();
    let parts = |key: &str, name: &dyn Fn(&Value) -> Option<String>| -> Vec<Part> {
        tp.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|e| Some(Part { name: name(e)?, zat: zat(e)? })).collect()).unwrap_or_default()
    };
    let pq_by_scheme =
        parts("pq_by_scheme", &|e| e.get("pqschemename").and_then(Value::as_str).map(str::to_string).or_else(|| e.get("pqscheme").and_then(Value::as_i64).map(|n| format!("scheme {n}"))));
    let vault_by_module = parts("vault_by_module", &|e| e.get("module").and_then(Value::as_str).or_else(|| e.get("tag").and_then(Value::as_str)).map(str::to_string));
    Some(TransparentPools { monitored, transparent_zat: tp.get("transparent").and_then(zat), pools, pq_by_scheme, vault_by_module })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn reads_by_id_whatever_the_order() {
        // the pools out of the documented order, an unknown id kept, an unmonitored entry dropped
        let f = fields(json!({ "transparentPools": {
            "monitored": true,
            "transparent": { "monitored": true, "chainValue": 1.5, "chainValueZat": 150_000_000i64 },
            "pools": [
                { "id": "vault", "monitored": true, "chainValueZat": 40_000_000 },
                { "id": "transparent_ecc", "monitored": true, "chainValueZat": 100_000_000 },
                { "id": "vault_intent", "monitored": true, "chainValueZat": 0 },
                { "id": "pq_channel", "monitored": false },
                { "id": "transparent_pq", "monitored": true, "chainValueZat": 10_000_000 },
                { "id": "future_pool", "monitored": true, "chainValueZat": 7 }
            ],
            "pq_by_scheme": [
                { "pqscheme": 2, "pqschemename": "FN-DSA-512", "monitored": true, "chainValueZat": 6_000_000 },
                { "pqscheme": 1, "pqschemename": "SLH-DSA-SHA2-128s", "monitored": true, "chainValueZat": 4_000_000 }
            ],
            "vault_by_module": [ { "module": "YED", "tag": "59454400", "monitored": true, "chainValueZat": 40_000_000 } ]
        }}));
        let tp = transparent_pools(&f).unwrap();
        assert!(tp.monitored);
        assert_eq!(tp.transparent_zat, Some(150_000_000));
        assert_eq!(tp.pools.get("transparent_ecc"), Some(&100_000_000));
        assert_eq!(tp.pools.get("transparent_pq"), Some(&10_000_000));
        assert_eq!(tp.pools.get("vault"), Some(&40_000_000));
        assert_eq!(tp.pools.get("vault_intent"), Some(&0));
        assert_eq!(tp.pools.get("pq_channel"), None);
        assert_eq!(tp.pools.get("future_pool"), Some(&7));
        assert_eq!(tp.pq_by_scheme, vec![Part { name: "FN-DSA-512".into(), zat: 6_000_000 }, Part { name: "SLH-DSA-SHA2-128s".into(), zat: 4_000_000 }]);
        assert_eq!(tp.vault_by_module, vec![Part { name: "YED".into(), zat: 40_000_000 }]);
        let ui = serde_json::to_value(&tp).unwrap();
        assert_eq!(ui["transparentZat"], json!(150_000_000));
        assert_eq!(ui["pqByScheme"][0]["name"], json!("FN-DSA-512"));
    }

    #[test]
    fn older_node_and_unmonitored_block() {
        // rpcversion 9: no transparentPools at all, only valuePools
        let old = fields(json!({ "valuePools": [ { "id": "transparent", "monitored": true, "chainValueZat": 1 } ] }));
        assert_eq!(transparent_pools(&old), None);
        // not an object: not read
        assert_eq!(transparent_pools(&fields(json!({ "transparentPools": [] }))), None);
        // a block the node's index does not cover yet
        let unmonitored = fields(json!({ "transparentPools": { "monitored": false, "transparent": { "monitored": false }, "pools": [] } }));
        let tp = transparent_pools(&unmonitored).unwrap();
        assert!(!tp.monitored && tp.pools.is_empty() && tp.transparent_zat.is_none());
        assert_eq!(serde_json::to_value(&tp).unwrap(), json!({ "monitored": false, "pools": {} }));
    }
}

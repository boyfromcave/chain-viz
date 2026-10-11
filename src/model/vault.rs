// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! The vault primitive's view (upgrade plan §3, §12 P6, §15.3–§15.8): signer sets, vaults (V)
//! and intents (I) by application tag, the spends of both, and the `YV` acts. Read-only like
//! everything else here: the collector feeds it `vault_getinfo`, `set_list` / `set_getinfo` and
//! `vault_list` answers (ycash-dd `doc/vault-rpc.md`) and the decoded transactions of every
//! block it fetched, and publishes what it returns. Field names are the node's (lowercase).
//!
//! A spend of a tracked V or I outpoint is classified from the spending input's selector (the
//! last scriptSig byte, `OP_1`..`OP_4`, §15.3): V 1 unlock, 2 owner, 3 owner-released, 4 app;
//! I 1 release, 2 cancel, 3 owner-released. A cancel's value goes back to the vault it was
//! unlocked from: the output whose script is the intent's `origin`. An outpoint that leaves
//! `vault_list` without a spend seen in a fetched block (a block walked before the outpoint was
//! listed, or a backfill gap) is inferred from the listing alone: an intent replaced by a vault
//! with its `origin` script before `matureheight` is a cancel, one gone at or after maturity a
//! release.
//!
//! Owners (the quantum line, `doc/vault-rpc.md` "Owners"): every V and I owner is a post-quantum
//! key, listed as `owner` (a `pqkeyid`, scheme ‖ keyHash) and `ownerscheme`; the snapshot adds
//! `owneraddress` (the 53-character PQ Yellowback address of the chain) and `ownerschemename`. A
//! spend whose scriptSig carries the chunked PQ signature stack (an owner spend) records its
//! `pqscheme`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;
use serde_json::{json, Value};

use crate::events::EventKind;
use crate::model::chain::Emitted;
use crate::rpc::{BlockFull, RawTransaction, VaultInfo};

/// Spends and acts kept for the snapshot (newest first).
pub const KEEP_SPENDS: usize = 200;
/// `set_getinfo` is asked for at most this many sets per block (`set_list` covers the rest).
pub const MAX_SET_DETAIL: usize = 16;
/// Template outputs shown in the snapshot at most.
pub const SNAPSHOT_ENTRIES: usize = 500;

/// `YV` act types (§15.5).
pub fn act_name(t: u8) -> &'static str {
    match t {
        0x01 => "SET_CREATE",
        0x02 => "SET_JOIN",
        0x03 => "SET_HEARTBEAT",
        0x04 => "SET_REMOVE",
        0x05 => "SET_EQUIVOCATION",
        0x06 => "SET_WINDDOWN",
        _ => "unknown",
    }
}

/// The action a selector names on a V (`vault`) or I (`intent`) input.
pub fn action_of(kind: &str, selector: u8) -> &'static str {
    match (kind, selector) {
        ("vault", 1) => "unlock",
        ("vault", 2) => "owner",
        ("vault", 3) => "owner-released",
        ("vault", 4) => "app",
        ("intent", 1) => "release",
        ("intent", 2) => "cancel",
        ("intent", 3) => "owner-released",
        _ => "spent",
    }
}

/// The selector of a template input: the scriptSig's last byte when it is `OP_1`..`OP_4`.
pub fn selector_of(script_sig_hex: &str) -> Option<u8> {
    let b = u8::from_str_radix(script_sig_hex.get(script_sig_hex.len().checked_sub(2)?..)?, 16).ok()?;
    (0x51..=0x54).contains(&b).then(|| b - 0x50)
}

/// The act of a `YV` OP_RETURN (`6a <push> 59 56 01 <type> …`): its type byte and, when the body
/// starts with one (every type but SET_CREATE), the 32-byte set id as a txid (byte-reversed).
pub fn decode_act(spk_hex: &str) -> Option<(u8, Option<String>)> {
    let b = hex::decode(spk_hex).ok()?;
    if b.first() != Some(&0x6a) {
        return None;
    }
    let (start, len) = match *b.get(1)? {
        n @ 1..=0x4b => (2usize, n as usize),
        0x4c => (3, *b.get(2)? as usize),
        0x4d => (4, u16::from_le_bytes([*b.get(2)?, *b.get(3)?]) as usize),
        _ => return None,
    };
    let p = b.get(start..start + len)?;
    if p.len() < 4 || p[0] != 0x59 || p[1] != 0x56 || p[2] != 0x01 {
        return None;
    }
    let set = (p[3] != 0x01 && p.len() >= 36).then(|| p[4..36].iter().rev().map(|x| format!("{:02x}", x)).collect());
    Some((p[3], set))
}

fn outpoint(v: &Value) -> String {
    v.get("outpoint")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}:{}", v.get("txid").and_then(Value::as_str).unwrap_or(""), v.get("vout").and_then(Value::as_u64).unwrap_or(0)))
}
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn u(v: &Value, k: &str) -> Option<u64> {
    v.get(k).and_then(Value::as_u64)
}
fn zat(v: &Value) -> i64 {
    v.get("valuezat").and_then(Value::as_i64).unwrap_or_else(|| (v.get("value").and_then(Value::as_f64).unwrap_or(0.0) * 1e8).round() as i64)
}

#[derive(Debug, Default)]
pub struct VaultModel {
    /// Some node answered `vault_getinfo` (`false`: every node said method-not-found).
    pub available: Option<bool>,
    pub info: Option<VaultInfo>,
    /// `set_getinfo` per set (with `memberlist`), in `set_list` order; a set beyond
    /// `MAX_SET_DETAIL` is its `set_list` row.
    pub sets: Vec<Value>,
    /// `vault_list` rows by outpoint.
    pub entries: BTreeMap<String, Value>,
    /// The `statehash` the entries were listed at.
    listed_at: Option<String>,
    /// Spends of V and I outputs, newest first.
    pub spends: VecDeque<Value>,
    spent: BTreeSet<String>,
    /// `YV` acts seen in fetched blocks, newest first.
    pub acts: VecDeque<Value>,
    acts_seen: BTreeSet<String>,
    /// `yed_getinfo.upgrade.attestorSetId` of the leader: the YED attestor set.
    pub attestor_set: Option<String>,
    /// The PQ Yellowback address version of the chain (quantum spec §4), from its name.
    owner_version: Option<[u8; 2]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VaultSnapshot {
    pub available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<VaultInfo>,
    #[serde(rename = "attestorSetId", skip_serializing_if = "Option::is_none")]
    pub attestor_set_id: Option<String>,
    /// The height predicates and blocks-to-release are taken at (the next block).
    #[serde(rename = "nextHeight", skip_serializing_if = "Option::is_none")]
    pub next_height: Option<u64>,
    pub sets: Vec<Value>,
    pub vaults: Vec<Value>,
    pub intents: Vec<Value>,
    /// Per tag: `{tag, tagtext, vaults, intents, valuezat}`.
    #[serde(rename = "byTag")]
    pub by_tag: Vec<Value>,
    pub spends: Vec<Value>,
    pub acts: Vec<Value>,
}

impl VaultModel {
    /// A fresh `vault_getinfo`. Returns whether `vault_list` must be asked again (the state hash
    /// moved since the last listing).
    pub fn on_info(&mut self, info: VaultInfo) -> bool {
        self.available = Some(true);
        let stale = self.listed_at.as_deref() != Some(info.statehash.as_str());
        self.info = Some(info);
        stale
    }

    /// The set rows (`set_getinfo` where asked, else `set_list`'s) at `height`. Changes of a
    /// set's `dormant` / `released` / `winddownheight` and of a member's `status` / `live` /
    /// `bondfrozen` are `set_state` events.
    pub fn on_sets(&mut self, height: u64, sets: Vec<Value>) -> Vec<Emitted> {
        let mut out = Vec::new();
        let before: BTreeMap<String, &Value> = self.sets.iter().map(|v| (s(v, "setid").to_string(), v)).collect();
        for set in &sets {
            let id = s(set, "setid");
            let Some(prev) = before.get(id) else {
                if !self.sets.is_empty() {
                    out.push(state(height, id, None, "created", Value::Null, json!(true)));
                }
                continue;
            };
            for f in ["dormant", "released", "winddownheight"] {
                if prev.get(f) != set.get(f) && set.get(f).is_some() {
                    out.push(state(height, id, None, f, prev.get(f).cloned().unwrap_or(Value::Null), set[f].clone()));
                }
            }
            let (Some(pm), Some(nm)) = (prev.get("memberlist").and_then(Value::as_array), set.get("memberlist").and_then(Value::as_array)) else { continue };
            let pk: BTreeMap<&str, &Value> = pm.iter().map(|m| (s(m, "key"), m)).collect();
            for m in nm {
                let key = s(m, "key");
                match pk.get(key) {
                    None => out.push(state(height, id, Some(key), "status", Value::Null, m.get("status").cloned().unwrap_or(Value::Null))),
                    Some(p) => {
                        for f in ["status", "live", "current", "bondfrozen"] {
                            if p.get(f) != m.get(f) && m.get(f).is_some() {
                                out.push(state(height, id, Some(key), f, p.get(f).cloned().unwrap_or(Value::Null), m[f].clone()));
                            }
                        }
                    }
                }
            }
        }
        self.sets = sets;
        out
    }

    /// A fresh `vault_list` at `statehash`, the tip `height`. Outpoints that left it without a
    /// spend seen in a block are inferred spends (`inferred: true`).
    pub fn on_list(&mut self, height: u64, statehash: &str, rows: Vec<Value>) -> Vec<Emitted> {
        let now: BTreeMap<String, Value> = rows.into_iter().map(|v| (outpoint(&v), v)).collect();
        let mut recs = Vec::new();
        for (op, old) in &self.entries {
            if now.contains_key(op) || self.spent.contains(op) {
                continue;
            }
            let kind = s(old, "kind");
            let action = if kind == "intent" {
                let origin = s(old, "origin");
                let back = now.iter().find(|(k, v)| !self.entries.contains_key(*k) && s(v, "kind") == "vault" && !origin.is_empty() && s(v, "script") == origin).map(|(k, _)| k.clone());
                match (back, u(old, "matureheight")) {
                    (Some(b), Some(m)) if height < m => Some(("cancel", Some(b))),
                    (_, Some(m)) if height >= m => Some(("release", None)),
                    _ => None,
                }
            } else {
                None
            };
            let (action, to) = action.unwrap_or(("spent", None));
            recs.push((op.clone(), spend_record(old, kind, action, None, None, Some(height), to, true)));
        }
        let out = recs.into_iter().map(|(op, rec)| self.push_spend(&op, rec)).collect();
        self.entries = now;
        self.listed_at = Some(statehash.to_string());
        out
    }

    /// Every transaction of a fetched block: spends of tracked V/I outpoints and `YV` acts.
    pub fn on_block(&mut self, b: &BlockFull) -> Vec<Emitted> {
        let mut out = Vec::new();
        for tx in &b.tx {
            out.extend(self.on_tx(tx, b.height));
        }
        out
    }

    fn on_tx(&mut self, tx: &RawTransaction, height: u64) -> Vec<Emitted> {
        let mut out = Vec::new();
        for vin in &tx.vin {
            let (Some(t), Some(n)) = (vin.get("txid").and_then(Value::as_str), vin.get("vout").and_then(Value::as_u64)) else { continue };
            let op = format!("{}:{}", t, n);
            let Some(entry) = self.entries.get(&op).cloned() else { continue };
            if self.spent.contains(&op) {
                continue;
            }
            let sig_hex = vin.get("scriptSig").and_then(|x| x.get("hex")).and_then(Value::as_str).unwrap_or("");
            let sel = selector_of(sig_hex);
            let kind = s(&entry, "kind").to_string();
            let action = sel.map(|x| action_of(&kind, x)).unwrap_or("spent");
            // A cancel returns the value to the originating vault script; an unlock re-locks the rest in the same V.
            let back_script = if kind == "intent" { s(&entry, "origin").to_string() } else { s(&entry, "script").to_string() };
            let to = tx.vout.iter().find(|o| !back_script.is_empty() && o.script_pub_key.get("hex").and_then(Value::as_str) == Some(back_script.as_str())).map(|o| format!("{}:{}", tx.txid, o.n));
            let mut rec = spend_record(&entry, &kind, action, sel, Some(&tx.txid), Some(height), to, false);
            if let Some(scheme) = hex::decode(sig_hex).ok().and_then(|b| crate::pq::scriptsig_scheme(&b)) {
                rec["pqscheme"] = scheme.into();
            }
            out.push(self.push_spend(&op, rec));
        }
        for o in &tx.vout {
            let Some((t, set)) = o.script_pub_key.get("hex").and_then(Value::as_str).and_then(decode_act) else { continue };
            if !self.acts_seen.insert(tx.txid.clone()) {
                continue;
            }
            let setid = if t == 0x01 { Some(tx.txid.clone()) } else { set };
            let rec = json!({"txid": tx.txid, "height": height, "type": act_name(t), "setid": setid});
            self.acts.push_front(rec.clone());
            self.acts.truncate(KEEP_SPENDS);
            if self.acts_seen.len() > 20_000 {
                self.acts_seen.clear();
            }
            out.push(Emitted { height: Some(height), node: None, kind: EventKind::SetAct(rec) });
        }
        out
    }

    fn push_spend(&mut self, op: &str, rec: Value) -> Emitted {
        self.spent.insert(op.to_string());
        if self.spent.len() > 20_000 {
            self.spent.clear();
        }
        self.spends.push_front(rec.clone());
        self.spends.truncate(KEEP_SPENDS);
        Emitted { height: rec.get("height").and_then(Value::as_u64), node: None, kind: EventKind::VaultSpend(rec) }
    }

    /// The chain's name (`getblockchaininfo.chain`): fixes the owner address version.
    pub fn set_chain(&mut self, chain: &str) {
        self.owner_version = crate::pq::yed_version(chain);
    }

    /// `owneraddress` and `ownerschemename` beside a row's `owner` (a pqkeyid).
    fn with_owner(&self, row: &mut Value) {
        let Some((scheme, hash)) = row.get("owner").and_then(Value::as_str).and_then(crate::pq::parse_pqkeyid) else { return };
        let Some(o) = row.as_object_mut() else { return };
        if let Some(v) = self.owner_version {
            o.insert("owneraddress".into(), crate::pq::encode(v, scheme, &hash).into());
        }
        o.insert("ownerschemename".into(), crate::pq::scheme_name(scheme).into());
    }

    pub fn snapshot(&self) -> VaultSnapshot {
        let next = self.info.as_ref().map(|i| i.height + 1);
        let mut vaults = Vec::new();
        let mut intents = Vec::new();
        let mut tags: BTreeMap<String, (String, u64, u64, i64)> = BTreeMap::new();
        for v in self.entries.values() {
            let t = tags.entry(s(v, "tag").to_string()).or_insert_with(|| (s(v, "tagtext").to_string(), 0, 0, 0));
            t.3 += zat(v);
            let mut row = v.clone();
            self.with_owner(&mut row);
            if s(v, "kind") == "intent" {
                t.2 += 1;
                if let (Some(o), Some(m), Some(n)) = (row.as_object_mut(), u(v, "matureheight"), next) {
                    o.insert("blocksToRelease".into(), (m as i64 - n as i64).into());
                }
                intents.push(row);
            } else {
                t.1 += 1;
                vaults.push(row);
            }
        }
        let sort = |a: &Value, b: &Value| u(b, "height").cmp(&u(a, "height"));
        vaults.sort_by(sort);
        intents.sort_by_key(|a| u(a, "matureheight"));
        vaults.truncate(SNAPSHOT_ENTRIES);
        intents.truncate(SNAPSHOT_ENTRIES);
        let attestor = self.attestor_set.clone();
        let sets = self.sets.iter().map(|set| with_rate(set, attestor.as_deref())).collect();
        VaultSnapshot {
            available: self.available,
            info: self.info.clone(),
            attestor_set_id: attestor,
            next_height: next,
            sets,
            vaults,
            intents,
            by_tag: tags.into_iter().map(|(tag, (text, nv, ni, z))| json!({"tag": tag, "tagtext": text, "vaults": nv, "intents": ni, "valuezat": z})).collect(),
            spends: self.spends.iter().cloned().collect(),
            acts: self.acts.iter().cloned().collect(),
        }
    }
}

fn state(height: u64, setid: &str, member: Option<&str>, field: &str, from: Value, to: Value) -> Emitted {
    Emitted { height: Some(height), node: None, kind: EventKind::SetState(json!({"setid": setid, "member": member, "field": field, "from": from, "to": to})) }
}

#[allow(clippy::too_many_arguments)]
fn spend_record(entry: &Value, kind: &str, action: &str, selector: Option<u8>, txid: Option<&str>, height: Option<u64>, to: Option<String>, inferred: bool) -> Value {
    json!({
        "outpoint": outpoint(entry), "kind": kind, "action": action, "selector": selector,
        "txid": txid, "height": height, "returnedTo": to, "inferred": inferred,
        "tag": entry.get("tag"), "tagtext": entry.get("tagtext"), "setid": entry.get("setid"), "cancelsetid": entry.get("cancelsetid"),
        "valuezat": zat(entry), "matureheight": entry.get("matureheight"),
    })
}

/// A set row with `rate`: the epoch's limit `epochbasis × ratelimitbps / 10000` and the share of
/// it used (`usedBps`), and `yed` when it is the YED attestor set.
pub fn with_rate(set: &Value, attestor: Option<&str>) -> Value {
    let mut row = set.clone();
    let bps = set.get("ratelimitbps").and_then(Value::as_i64).unwrap_or(0);
    let num = |k: &str| set.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let (basis, used) = (num("epochbasis"), num("epochused"));
    let rate = if bps == 0 {
        json!({"limited": false, "used": used})
    } else {
        let limit = basis * bps as f64 / 10_000.0;
        json!({"limited": true, "limit": limit, "used": used, "basis": basis, "usedBps": if limit > 0.0 { Some((used * 10_000.0 / limit).round() as i64) } else { None }})
    };
    if let Some(o) = row.as_object_mut() {
        o.insert("rate".into(), rate);
        o.insert("yed".into(), json!(attestor.is_some() && attestor == set.get("setid").and_then(Value::as_str)));
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_and_acts() {
        assert_eq!(selector_of("0151"), Some(1));
        assert_eq!(selector_of("47304402aa52"), Some(2));
        assert_eq!(selector_of("54"), Some(4));
        assert_eq!(selector_of("00"), None);
        assert_eq!(selector_of(""), None);
        assert_eq!(action_of("intent", 2), "cancel");
        assert_eq!(action_of("vault", 1), "unlock");
        // SET_HEARTBEAT: "YV" 01 03 setId(32) memberKey(33)
        let mut p = vec![0x59, 0x56, 0x01, 0x03];
        let mut id = [0u8; 32];
        id[0] = 0xab;
        p.extend_from_slice(&id);
        p.extend_from_slice(&[2u8; 33]);
        let spk = format!("6a{:02x}{}", p.len(), hex::encode(&p));
        let (t, set) = decode_act(&spk).unwrap();
        assert_eq!(act_name(t), "SET_HEARTBEAT");
        assert!(set.unwrap().ends_with("ab"), "set id printed as a txid (byte-reversed)");
        // a longer act uses OP_PUSHDATA1
        let mut big = vec![0x59, 0x56, 0x01, 0x05];
        big.extend_from_slice(&[0u8; 264]);
        assert_eq!(decode_act(&format!("6a4d{}{}", hex::encode((big.len() as u16).to_le_bytes()), hex::encode(&big))).map(|x| x.0), Some(5));
        assert_eq!(decode_act("6a0459420301"), None, "a YB payload is not an act");
    }

    fn row(op: &str, kind: &str, extra: Value) -> Value {
        let mut v = json!({"outpoint": op, "txid": op.split(':').next().unwrap(), "vout": 0, "kind": kind, "tag": "54455354", "tagtext": "TEST", "setid": "s1", "valuezat": 100, "height": 10, "script": format!("{}-spk", op)});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    #[test]
    fn cancel_from_the_block_and_inferred() {
        let mut m = VaultModel::default();
        m.on_info(VaultInfo { height: 20, statehash: "a".into(), ..Default::default() });
        let intent = row("i1:0", "intent", json!({"origin": "v-spk", "matureheight": 25}));
        let intent2 = row("i2:0", "intent", json!({"origin": "v2-spk", "matureheight": 22}));
        assert!(m.on_list(20, "a", vec![intent.clone(), intent2.clone()]).is_empty());
        let snap = m.snapshot();
        assert_eq!(snap.intents[0]["blocksToRelease"], 1, "22 - next height 21");
        assert_eq!(snap.by_tag[0]["intents"], 2);
        // block 21 spends i1 with selector 2 back to its origin script
        let tx: RawTransaction = serde_json::from_value(json!({"txid": "c1", "vin": [{"txid": "i1", "vout": 0, "scriptSig": {"hex": "4730aa52"}}],
            "vout": [{"value": 1.0, "n": 0, "scriptPubKey": {"hex": "v-spk"}}]}))
        .unwrap();
        let ev = m.on_tx(&tx, 21);
        assert!(matches!(&ev[0].kind, EventKind::VaultSpend(r) if r["action"] == "cancel" && r["returnedTo"] == "c1:0" && r["inferred"] == false));
        // the listing drops i1 (already recorded) and i2 without a block spend, at height 22 = maturity
        let ev = m.on_list(22, "b", vec![row("c1:0", "vault", json!({"script": "v-spk"}))]);
        assert_eq!(ev.len(), 1);
        assert!(matches!(&ev[0].kind, EventKind::VaultSpend(r) if r["outpoint"] == "i2:0" && r["action"] == "release" && r["inferred"] == true));
        // an inferred cancel: an intent replaced by its origin vault before maturity
        let i3 = row("i3:0", "intent", json!({"origin": "v3-spk", "matureheight": 40}));
        let c1 = row("c1:0", "vault", json!({"script": "v-spk"}));
        m.on_list(23, "c", vec![c1.clone(), i3]);
        let ev = m.on_list(24, "d", vec![c1, row("v3:0", "vault", json!({"script": "v3-spk"}))]);
        assert!(matches!(&ev[0].kind, EventKind::VaultSpend(r) if r["action"] == "cancel" && r["returnedTo"] == "v3:0" && r["inferred"] == true));
        assert_eq!(m.snapshot().spends.len(), 3);
    }

    /// The quantum line: PQ owners get their address and scheme name; a chunked owner spend
    /// (a 7,939-byte SLH-DSA scriptSig ending in selector 2) is an owner spend with `pqscheme` 1.
    #[test]
    fn pq_owner_and_chunked_selector() {
        use crate::pq::tests::pq_scriptsig;
        let mut m = VaultModel::default();
        m.set_chain("regtest");
        m.on_info(VaultInfo { height: 20, statehash: "a".into(), ..Default::default() });
        let owner = format!("01{}", "42".repeat(32));
        let v = row("v1:0", "vault", json!({"owner": owner, "ownerscheme": 1, "ownerheight": 30}));
        let bad = row("v2:0", "vault", json!({"owner": format!("07{}", "42".repeat(32)), "ownerscheme": 7}));
        m.on_list(20, "a", vec![v, bad]);
        let snap = m.snapshot();
        let r = snap.vaults.iter().find(|x| x["outpoint"] == "v1:0").unwrap();
        let addr = r["owneraddress"].as_str().unwrap();
        assert!(addr.starts_with("yr") && addr.len() == 53, "{}", addr);
        assert_eq!(crate::pq::decode(addr), Some(([0x57, 0x10], 1, [0x42; 32])));
        assert_eq!(r["ownerschemename"], "SLH-DSA-SHA2-128s");
        let r2 = snap.vaults.iter().find(|x| x["outpoint"] == "v2:0").unwrap();
        assert!(r2.get("owneraddress").is_none(), "an unregistered scheme has no address");
        for (scheme, sel, action) in [(1u8, 0x52u8, "owner"), (2, 0x53, "owner-released")] {
            let sig = pq_scriptsig(scheme, &[], &[sel]);
            if scheme == 1 {
                assert_eq!(sig.len(), 7939);
            }
            assert_eq!(selector_of(&hex::encode(&sig)), Some(sel - 0x50));
            let tx: RawTransaction = serde_json::from_value(json!({"txid": format!("o{}", scheme), "vin": [{"txid": "v1", "vout": 0, "scriptSig": {"hex": hex::encode(&sig)}}], "vout": []})).unwrap();
            let mut mm = VaultModel::default();
            mm.on_list(20, "a", vec![row("v1:0", "vault", json!({"owner": owner}))]);
            let ev = mm.on_tx(&tx, 21);
            assert!(matches!(&ev[0].kind, EventKind::VaultSpend(r) if r["action"] == action && r["pqscheme"] == scheme), "{:?}", ev);
        }
    }

    #[test]
    fn set_diffs_and_rate() {
        let mut m = VaultModel { attestor_set: Some("s1".into()), ..Default::default() };
        let set = |dormant: bool, live: bool| {
            json!({"setid": "s1", "dormant": dormant, "released": dormant, "winddownheight": 0, "ratelimitbps": 5000, "epochbasis": 10.0, "epochused": 2.5,
            "memberlist": [{"key": "k1", "status": "active", "live": live, "current": true, "bondfrozen": false}]})
        };
        assert!(m.on_sets(10, vec![set(false, true)]).is_empty(), "first sight is no change");
        let ev = m.on_sets(11, vec![set(true, false)]);
        let fields: Vec<String> = ev.iter().filter_map(|e| if let EventKind::SetState(v) = &e.kind { Some(v["field"].as_str().unwrap().to_string()) } else { None }).collect();
        assert_eq!(fields, ["dormant", "released", "live"]);
        let snap = m.snapshot();
        assert_eq!(snap.sets[0]["yed"], true);
        assert_eq!(snap.sets[0]["rate"]["limit"], 5.0);
        assert_eq!(snap.sets[0]["rate"]["usedBps"], 5000);
        assert_eq!(with_rate(&json!({"setid": "x", "ratelimitbps": 0}), None)["rate"]["limited"], false);
    }
}

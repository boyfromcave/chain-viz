//! The revenue ledger (plan §3.2.5, C4): one attributed output per row,
//! `(height, txid, vout, kind, zat, payee)`, built from each confirmed block's coinbase
//! (`subsidy`, `subsidy_other`, `netfee`) and from every Yellowback transaction's `yed_gettxinfo`
//! (`enforcefee` → `payee`, `attestfee` → `attestPayee`, `residual` → the owner,
//! `collateral_release` → whoever took a vault's collateral while enforcement was off). Pure
//! state: the collector feeds it blocks, tags, enriched transactions and `yed_getfeepayee`
//! answers and publishes what it returns; `/api/revenue` rolls it up.
//!
//! Money is kept in zatoshi; USD is computed at read time with the yellowback model's `pMint`
//! for the row's height (C-9), so a late `yed_gethistory` backfill fills earlier rows in. Every
//! USD figure is labelled `PRICE_LABEL`.
//!
//! Payee keys are addresses as the node spells them (`yed_gettxinfo.payee`,
//! `yed_gettag.payoutAddress`, `yed_listminers.payoutAddress`); a coinbase output's address is
//! `scriptPubKey.addresses[0]`, or is encoded here from a P2PKH script with the prefix learned
//! from any node-supplied address. A block whose coinbase pays one address while its tag names
//! another is an alias: the coinbase address rolls up under the tag's `payoutAddress`.
//!
//! Eviction (`keep`) drops per-height rows below the window; the cumulative rollups
//! (`totals`, per payee) are never evicted, so a long run's headline numbers stay whole.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::events::EventKind;
use crate::model::chain::Emitted;
use crate::model::yellowback::YbTx;
use crate::rpc::{BlockFull, BlockSubsidy};

/// The label every USD figure carries (C-9).
pub const PRICE_LABEL: &str = "at pMint";
/// Rows returned by `/api/revenue` at most (the totals and groups cover the whole range).
pub const MAX_ROWS: usize = 5000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The miner's share of `getblocksubsidy` at that height.
    Subsidy,
    /// Coinbase outputs that are not the miner's: the founders / dev-fund share.
    SubsidyOther,
    /// Network fees: the miner's coinbase outputs beyond the subsidy.
    Netfee,
    /// FEE-1/2: `feeZat` → `payee` (a quoting pool, not necessarily the block's miner).
    Enforcefee,
    /// AFEE-1: `attestFeeZat` → `attestPayee`.
    Attestfee,
    /// A vault's collateral taken through the `OP_TRUE` path while enforcement was off.
    CollateralRelease,
    /// RED-5: `residualZat` back to the vault owner on a claim.
    Residual,
}

impl Kind {
    pub const ALL: [Kind; 7] = [Kind::Subsidy, Kind::SubsidyOther, Kind::Netfee, Kind::Enforcefee, Kind::Attestfee, Kind::CollateralRelease, Kind::Residual];
    pub fn name(self) -> &'static str {
        match self {
            Kind::Subsidy => "subsidy",
            Kind::SubsidyOther => "subsidy_other",
            Kind::Netfee => "netfee",
            Kind::Enforcefee => "enforcefee",
            Kind::Attestfee => "attestfee",
            Kind::CollateralRelease => "collateral_release",
            Kind::Residual => "residual",
        }
    }
    /// The camelCase key the rollups use.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Subsidy => "subsidy",
            Kind::SubsidyOther => "subsidyOther",
            Kind::Netfee => "netfee",
            Kind::Enforcefee => "enforcefee",
            Kind::Attestfee => "attestfee",
            Kind::CollateralRelease => "collateralRelease",
            Kind::Residual => "residual",
        }
    }
}

/// One ledger row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Row {
    pub height: u64,
    pub txid: String,
    pub vout: u32,
    pub kind: Kind,
    pub zat: i64,
    pub payee: String,
    /// `enforcefee` rows: the payload's `refHeight` (R), whose E(R) the counterfactual needs.
    #[serde(rename = "refHeight", default, skip_serializing_if = "Option::is_none")]
    pub ref_height: Option<u64>,
}

/// One block's ledger.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BlockLedger {
    pub hash: String,
    /// The coinbase's payee (the miner), when a P2PKH/P2SH address could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub miner: Option<String>,
    /// `yed_gettag.payoutAddress` when the tag was found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub rows: Vec<Row>,
    /// The Yellowback transactions attributed so far (`txid` → the summary shown per block).
    #[serde(rename = "ybTxs")]
    pub yb_txs: Vec<Value>,
}

/// Per-payee cumulative counters (never evicted).
#[derive(Debug, Clone, Default, Serialize)]
pub struct PayeeTotals {
    #[serde(rename = "blocksMined")]
    pub blocks_mined: u64,
    pub tags: u64,
    #[serde(rename = "timesSelected")]
    pub times_selected: u64,
    #[serde(rename = "timesAttestor")]
    pub times_attestor: u64,
    pub zat: BTreeMap<&'static str, i64>,
}

/// An output of a Yellowback transaction as the block carried it (for the release / residual rows).
#[derive(Debug, Clone)]
struct Out {
    n: u32,
    zat: i64,
    address: Option<String>,
}

#[derive(Debug, Default)]
pub struct RevenueModel {
    blocks: BTreeMap<u64, BlockLedger>,
    /// Outputs of the Yellowback transactions seen in blocks, by txid, until attributed.
    outs: HashMap<String, Vec<Out>>,
    /// `getblocksubsidy` per height (miner zat, other zat, founders address).
    subsidy: BTreeMap<u64, (i64, i64, Option<String>)>,
    /// |E(R)| per refHeight (`None`: the node said FEE-0 / out of range).
    eligible: BTreeMap<u64, Option<usize>>,
    /// refHeights to ask, with the collateral to pass.
    pending_ref: BTreeMap<u64, i64>,
    /// coinbase address → the tag's payoutAddress seen on the same block.
    aliases: BTreeMap<String, String>,
    totals: BTreeMap<&'static str, i64>,
    by_payee: BTreeMap<String, PayeeTotals>,
    /// Heights ever ledgered (cumulative counters cover them), first and last.
    first_height: Option<u64>,
    last_height: Option<u64>,
    /// Heights whose rows were evicted below the window.
    evicted: u64,
    prefix: Option<[u8; 2]>,
    keep: u64,
    /// The fund's address as last attributed. 6.20.0's `getblocksubsidy` no longer names it
    /// (only `miner founders totalblocksubsidy`), so it is learned from a coinbase where exactly
    /// one output equals the fund's share, and breaks a tie when several do.
    fund_addr: Option<String>,
}

fn zat_of(v: f64) -> i64 {
    (v * 1e8).round() as i64
}

/// `zat` as USD at `p_mint` micro-USD per YEC.
pub fn usd(zat: i64, p_mint: Option<i64>) -> Option<f64> {
    let p = p_mint.filter(|p| *p > 0)?;
    Some(zat as f64 / 1e8 * p as f64 / 1e6)
}

fn money(zat: i64, p_mint: Option<i64>) -> Value {
    json!({"zat": zat, "usd": usd(zat, p_mint), "priceUsed": p_mint.filter(|p| *p > 0)})
}

/// Base58Check-decode an address to its 2-byte prefix and 20-byte hash (no checksum check).
pub fn decode_address(addr: &str) -> Option<([u8; 2], [u8; 20])> {
    let bytes = bs58::decode(addr).into_vec().ok()?;
    if bytes.len() != 26 {
        return None;
    }
    Some(([bytes[0], bytes[1]], bytes[2..22].try_into().ok()?))
}

/// Base58Check-encode `prefix ++ hash160`.
pub fn encode_address(prefix: [u8; 2], hash: &[u8; 20]) -> String {
    let mut payload = Vec::with_capacity(26);
    payload.extend_from_slice(&prefix);
    payload.extend_from_slice(hash);
    let check = Sha256::digest(Sha256::digest(&payload));
    payload.extend_from_slice(&check[..4]);
    bs58::encode(payload).into_string()
}

/// The hash160 of a P2PKH script (`76 a9 14 <20> 88 ac`).
pub fn p2pkh_hash(script: &[u8]) -> Option<[u8; 20]> {
    if script.len() == 25 && script[0] == 0x76 && script[1] == 0xa9 && script[2] == 0x14 && script[23] == 0x88 && script[24] == 0xac {
        script[3..23].try_into().ok()
    } else {
        None
    }
}

impl RevenueModel {
    pub fn new(keep: u64) -> RevenueModel {
        RevenueModel { keep, ..Default::default() }
    }

    /// Remember the address prefix from any address the node spelled (once).
    pub fn learn_prefix(&mut self, addr: &str) {
        if self.prefix.is_none() {
            if let Some((p, _)) = decode_address(addr) {
                self.prefix = Some(p);
            }
        }
    }

    /// The address of an output: `scriptPubKey.addresses[0]`, else encoded from a P2PKH hex.
    pub fn address_of(&self, script_pub_key: &Value) -> Option<String> {
        if let Some(a) = script_pub_key.get("addresses").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str) {
            return Some(a.to_string());
        }
        let hex = script_pub_key.get("hex").and_then(Value::as_str)?;
        let script = hex::decode(hex).ok()?;
        let hash = p2pkh_hash(&script)?;
        Some(match self.prefix {
            Some(p) => encode_address(p, &hash),
            None => format!("hash160:{}", hex::encode(hash)),
        })
    }

    /// Does the ledger still need this block (unknown height, or another hash there)?
    pub fn wants_block(&self, hash: &str, height: u64) -> bool {
        self.blocks.get(&height).map(|b| b.hash != hash).unwrap_or(true)
    }
    pub fn subsidy_known(&self, height: u64) -> bool {
        self.subsidy.contains_key(&height)
    }
    pub fn set_subsidy(&mut self, height: u64, s: &BlockSubsidy) {
        let other = s.extra.get("founders").and_then(Value::as_f64).map(zat_of).unwrap_or(0)
            + s.extra.get("fundingstreams").and_then(Value::as_array).map(|a| a.iter().filter_map(|f| f.get("valueZat").and_then(Value::as_i64)).sum()).unwrap_or(0);
        let addr = s.extra.get("foundersaddress").and_then(Value::as_str).filter(|a| !a.is_empty()).map(str::to_string);
        self.subsidy.insert(height, (zat_of(s.miner), other, addr));
    }
    /// Cached per height; the miner's share only changes at halvings and fund boundaries.
    pub fn subsidy(&self, height: u64) -> Option<(i64, i64)> {
        self.subsidy.get(&height).map(|(m, o, _)| (*m, *o))
    }

    /// A fetched block (`getblock … 2`): coinbase rows, and the outputs of its Yellowback
    /// transactions kept for attribution. Replaces whatever the ledger held at that height
    /// (a reorg). Returns the `revenue` events to publish.
    pub fn on_block(&mut self, b: &BlockFull, yb_txids: &[String]) -> Vec<Emitted> {
        let height = b.height;
        let (miner_subsidy, other_subsidy, founders_addr) = self.subsidy.get(&height).cloned().unwrap_or((0, 0, None));
        let mut rows = Vec::new();
        let mut miner = None;
        if let Some(cb) = b.tx.first() {
            // The miner's outputs are every coinbase output not paying the founders address;
            // the largest of them names the miner.
            let mut mine: Vec<(u32, i64, Option<String>)> = Vec::new();
            for o in &cb.vout {
                let zat = zat_of(o.value);
                let addr = self.address_of(&o.script_pub_key);
                if founders_addr.is_some() && addr == founders_addr {
                    rows.push(Row { height, txid: cb.txid.clone(), vout: o.n, kind: Kind::SubsidyOther, zat, payee: addr.unwrap_or_default(), ref_height: None });
                } else {
                    mine.push((o.n, zat, addr));
                }
            }
            if founders_addr.is_some() {
                self.fund_addr = founders_addr.clone();
            } else if other_subsidy > 0 {
                // No address to match (ycashd 6.20.0 dropped `foundersaddress`): the output
                // equal to the other share is the fund's; among several, the learned address.
                let hits: Vec<usize> = mine.iter().enumerate().filter(|(_, m)| m.1 == other_subsidy).map(|(i, _)| i).collect();
                let pick = match hits.len() {
                    0 => None,
                    1 => Some(hits[0]),
                    _ => hits.iter().copied().find(|&i| self.fund_addr.is_some() && mine[i].2 == self.fund_addr).or(Some(hits[0])),
                };
                if let Some(i) = pick {
                    let (n, z, a) = mine.remove(i);
                    if hits.len() == 1 && a.is_some() {
                        self.fund_addr = a.clone();
                    }
                    rows.push(Row { height, txid: cb.txid.clone(), vout: n, kind: Kind::SubsidyOther, zat: z, payee: a.unwrap_or_default(), ref_height: None });
                }
            }
            let total: i64 = mine.iter().map(|m| m.1).sum();
            miner = mine.iter().max_by_key(|m| m.1).and_then(|m| m.2.clone());
            let payee = miner.clone().unwrap_or_else(|| "unknown".into());
            let vout = mine.iter().max_by_key(|m| m.1).map(|m| m.0).unwrap_or(0);
            // Without `getblocksubsidy` for the height (replay of an older file) the whole
            // coinbase counts as subsidy and no netfee row is written.
            let known = self.subsidy.contains_key(&height);
            let subsidy = if known { total.min(miner_subsidy) } else { total };
            let netfee = (total - miner_subsidy).max(0);
            rows.push(Row { height, txid: cb.txid.clone(), vout, kind: Kind::Subsidy, zat: subsidy, payee: payee.clone(), ref_height: None });
            if known {
                rows.push(Row { height, txid: cb.txid.clone(), vout, kind: Kind::Netfee, zat: netfee, payee, ref_height: None });
            }
        }
        for t in &b.tx {
            if yb_txids.contains(&t.txid) {
                self.outs.insert(t.txid.clone(), t.vout.iter().map(|o| Out { n: o.n, zat: zat_of(o.value), address: self.address_of(&o.script_pub_key) }).collect());
            }
        }
        if let Some(old) = self.blocks.remove(&height) {
            self.uncount(&old);
        }
        let ledger = BlockLedger { hash: b.hash.clone(), miner, tag: None, rows, yb_txs: Vec::new() };
        self.count(&ledger);
        let out = ledger.rows.iter().map(emitted).collect();
        self.blocks.insert(height, ledger);
        self.first_height = Some(self.first_height.map_or(height, |f| f.min(height)));
        self.last_height = Some(self.last_height.map_or(height, |l| l.max(height)));
        self.evict();
        out
    }

    /// `yed_gettag` for a block: the tag's `payoutAddress` (when found). A coinbase paying a
    /// different address becomes an alias of it.
    pub fn on_tag(&mut self, height: u64, hash: &str, tag: &Value) {
        let found = tag.get("found").and_then(Value::as_bool).unwrap_or(false);
        let Some(payout) = tag.get("payoutAddress").and_then(Value::as_str).filter(|_| found) else { return };
        self.learn_prefix(payout);
        let Some(b) = self.blocks.get_mut(&height).filter(|b| b.hash == hash) else { return };
        if b.tag.as_deref() == Some(payout) {
            return;
        }
        b.tag = Some(payout.to_string());
        let miner = b.miner.clone();
        self.by_payee.entry(payout.to_string()).or_default().tags += 1;
        if let Some(m) = miner.filter(|m| m != payout) {
            self.aliases.insert(m, payout.to_string());
        }
    }

    /// An enriched (confirmed) Yellowback transaction: its fee, attestor-fee, residual and
    /// (with enforcement off) collateral-release rows. `enforcing` is the node's
    /// `yed_getinfo.enforcing` at the time. Idempotent per `(txid, kind)`.
    pub fn on_yb_tx(&mut self, height: u64, t: &YbTx, enforcing: bool) -> Vec<Emitted> {
        if t.info.is_none() {
            return Vec::new();
        }
        for a in [&t.payee, &t.attest_payee].into_iter().flatten() {
            self.learn_prefix(a);
        }
        let outs = self.outs.get(&t.txid).cloned().unwrap_or_default();
        let out_at = |n: Option<u8>| n.and_then(|n| outs.iter().find(|o| o.n == n as u32));
        let mut rows = Vec::new();
        let collat = if t.tx_type == "mint" { outs.first().map(|o| o.zat).unwrap_or(0) } else { 0 };
        if t.fee_zat > 0 {
            if let Some(p) = &t.payee {
                let vout = t.payload.fee_vout.map(|v| v as u32).unwrap_or(u32::MAX);
                let r = t.payload.ref_height.map(|r| r as u64);
                rows.push(Row { height, txid: t.txid.clone(), vout, kind: Kind::Enforcefee, zat: t.fee_zat, payee: p.clone(), ref_height: r });
                if let Some(r) = r {
                    if !self.eligible.contains_key(&r) {
                        self.pending_ref.entry(r).and_modify(|c| *c = (*c).max(collat)).or_insert(collat);
                    }
                }
            }
        }
        if t.attest_fee_zat > 0 {
            if let Some(p) = &t.attest_payee {
                let vout = t.payload.attest_fee_vout.map(|v| v as u32).unwrap_or(u32::MAX);
                rows.push(Row { height, txid: t.txid.clone(), vout, kind: Kind::Attestfee, zat: t.attest_fee_zat, payee: p.clone(), ref_height: None });
            }
        }
        if t.residual_zat > 0 {
            let o = outs.iter().find(|o| o.zat == t.residual_zat);
            rows.push(Row {
                height,
                txid: t.txid.clone(),
                vout: o.map(|o| o.n).unwrap_or(u32::MAX),
                kind: Kind::Residual,
                zat: t.residual_zat,
                payee: o.and_then(|o| o.address.clone()).unwrap_or_else(|| "owner".into()),
                ref_height: None,
            });
        }
        if !enforcing && t.tx_type == "redeem" {
            // The vault spend's collateral: the largest output that is not the fee, the
            // attestor fee, the payload or a carrier (10 000 zat).
            let skip: BTreeSet<u32> = [out_at(t.payload.fee_vout), out_at(t.payload.attest_fee_vout)].into_iter().flatten().map(|o| o.n).chain(std::iter::once(t.payload.payload_vout)).collect();
            if let Some(o) = outs.iter().filter(|o| !skip.contains(&o.n) && o.zat > 10_000).max_by_key(|o| o.zat) {
                rows.push(Row { height, txid: t.txid.clone(), vout: o.n, kind: Kind::CollateralRelease, zat: o.zat, payee: o.address.clone().unwrap_or_else(|| "unknown".into()), ref_height: None });
            }
        }
        let summary = json!({
            "txid": t.txid, "type": t.tx_type, "path": t.path, "verdict": t.verdict,
            "feeZat": t.fee_zat, "payee": t.payee, "attestFeeZat": t.attest_fee_zat, "attestPayee": t.attest_payee,
            "residualZat": t.residual_zat, "burned": t.burned, "refHeight": t.payload.ref_height,
        });
        let Some(b) = self.blocks.get_mut(&height) else { return Vec::new() };
        let mut out = Vec::new();
        let mut added = Vec::new();
        for r in rows {
            if b.rows.iter().any(|x| x.txid == r.txid && x.kind == r.kind) {
                continue;
            }
            out.push(emitted(&r));
            added.push(r);
        }
        if let Some(slot) = b.yb_txs.iter_mut().find(|x| x["txid"] == summary["txid"]) {
            *slot = summary;
        } else {
            b.yb_txs.push(summary);
        }
        let single = BlockLedger { rows: added.clone(), ..Default::default() };
        b.rows.extend(added);
        self.count(&single);
        self.outs.remove(&t.txid);
        out
    }

    /// Replay: a recorded `revenue` event.
    pub fn apply_event(&mut self, height: u64, row: Row) {
        let b = self.blocks.entry(height).or_default();
        if b.rows.iter().any(|x| x.txid == row.txid && x.kind == row.kind && x.vout == row.vout) {
            return;
        }
        let miner = matches!(row.kind, Kind::Subsidy).then(|| row.payee.clone());
        if miner.is_some() {
            b.miner = miner.clone();
        }
        let single = BlockLedger { rows: vec![row.clone()], miner, ..Default::default() };
        b.rows.push(row);
        self.count(&single);
        self.first_height = Some(self.first_height.map_or(height, |f| f.min(height)));
        self.last_height = Some(self.last_height.map_or(height, |l| l.max(height)));
    }

    /// The next `yed_getfeepayee refHeight collateralZat` to ask, if any.
    pub fn next_ref(&self) -> Option<(u64, i64)> {
        self.pending_ref.iter().next().map(|(r, c)| (*r, *c))
    }
    /// `|E(R)|` from `yed_getfeepayee` (`None` when the node refused: FEE-0 or out of range).
    pub fn set_eligible(&mut self, r: u64, n: Option<usize>) {
        self.pending_ref.remove(&r);
        self.eligible.insert(r, n);
    }
    pub fn eligible(&self, r: u64) -> Option<Option<usize>> {
        self.eligible.get(&r).copied()
    }

    fn count(&mut self, b: &BlockLedger) {
        if b.miner.is_some() {
            self.by_payee.entry(b.miner.clone().unwrap()).or_default().blocks_mined += 1;
        }
        for r in &b.rows {
            *self.totals.entry(r.kind.key()).or_insert(0) += r.zat;
            let p = self.by_payee.entry(r.payee.clone()).or_default();
            *p.zat.entry(r.kind.key()).or_insert(0) += r.zat;
            match r.kind {
                Kind::Enforcefee => p.times_selected += 1,
                Kind::Attestfee => p.times_attestor += 1,
                _ => {}
            }
        }
    }
    fn uncount(&mut self, b: &BlockLedger) {
        if let Some(m) = &b.miner {
            if let Some(p) = self.by_payee.get_mut(m) {
                p.blocks_mined = p.blocks_mined.saturating_sub(1);
            }
        }
        if let Some(t) = &b.tag {
            if let Some(p) = self.by_payee.get_mut(t) {
                p.tags = p.tags.saturating_sub(1);
            }
        }
        for r in &b.rows {
            *self.totals.entry(r.kind.key()).or_insert(0) -= r.zat;
            if let Some(p) = self.by_payee.get_mut(&r.payee) {
                *p.zat.entry(r.kind.key()).or_insert(0) -= r.zat;
                match r.kind {
                    Kind::Enforcefee => p.times_selected = p.times_selected.saturating_sub(1),
                    Kind::Attestfee => p.times_attestor = p.times_attestor.saturating_sub(1),
                    _ => {}
                }
            }
        }
    }

    fn evict(&mut self) {
        let Some(top) = self.blocks.keys().next_back().copied() else { return };
        if top > self.keep {
            let floor = top - self.keep;
            let kept = self.blocks.split_off(&floor);
            self.evicted += self.blocks.len() as u64;
            self.blocks = kept;
            self.subsidy = self.subsidy.split_off(&floor);
        }
    }

    /// The window of heights the per-block rows cover.
    pub fn window(&self) -> Option<(u64, u64)> {
        Some((*self.blocks.keys().next()?, *self.blocks.keys().next_back()?))
    }
    pub fn block(&self, height: u64) -> Option<&BlockLedger> {
        self.blocks.get(&height)
    }
    /// Resolve an address to the key it rolls up under.
    pub fn canonical<'a>(&'a self, payee: &'a str) -> &'a str {
        self.aliases.get(payee).map(String::as_str).unwrap_or(payee)
    }

    /// The snapshot section: cumulative totals and per-payee rollups (never evicted).
    pub fn snapshot(&self, price: &dyn Fn(u64) -> Option<i64>) -> Value {
        let last = self.last_height.and_then(price);
        let mut by_payee: BTreeMap<String, PayeeTotals> = BTreeMap::new();
        for (k, v) in &self.by_payee {
            let key = self.canonical(k).to_string();
            let e = by_payee.entry(key).or_default();
            e.blocks_mined += v.blocks_mined;
            e.tags += v.tags;
            e.times_selected += v.times_selected;
            e.times_attestor += v.times_attestor;
            for (kk, z) in &v.zat {
                *e.zat.entry(kk).or_insert(0) += z;
            }
        }
        let totals: BTreeMap<&str, Value> = Kind::ALL.iter().map(|k| (k.key(), money(self.totals.get(k.key()).copied().unwrap_or(0), last))).collect();
        json!({
            "priceLabel": PRICE_LABEL,
            "priceNote": "cumulative USD uses the latest pMint; per-row USD in /api/revenue uses each row's height",
            "firstHeight": self.first_height, "lastHeight": self.last_height,
            "window": self.window().map(|(a, b)| json!({"from": a, "to": b})),
            "evictedBlocks": self.evicted,
            "totals": totals,
            "byPayee": by_payee,
            "aliases": self.aliases,
        })
    }

    /// `GET /api/revenue?from&to&by`: the rows in `[from, to]` rolled up by `payoutKey`,
    /// `attestor` or `block`, with the counterfactual and the no-enforcement releases.
    /// `miners` and `attestors` are the leader's `yed_listminers` / `yed_listattestors`.
    pub fn query(&self, from: u64, to: u64, by: &str, price: &dyn Fn(u64) -> Option<i64>, miners: &[Value], attestors: &[Value]) -> Value {
        let blocks: Vec<(&u64, &BlockLedger)> = self.blocks.range(from..=to).collect();
        let sum = |rows: &[&Row]| -> (i64, f64, bool) {
            let mut zat = 0;
            let mut usd_sum = 0.0;
            let mut complete = true;
            for r in rows {
                zat += r.zat;
                match usd(r.zat, price(r.height)) {
                    Some(u) => usd_sum += u,
                    None => complete = false,
                }
            }
            (zat, usd_sum, complete)
        };
        let money_of = |rows: &[&Row]| {
            let (z, u, c) = sum(rows);
            json!({"zat": z, "usd": if rows.is_empty() { Some(0.0) } else { Some(u) }, "usdComplete": c})
        };
        let all: Vec<&Row> = blocks.iter().flat_map(|(_, b)| b.rows.iter()).collect();
        let mut totals = serde_json::Map::new();
        for k in Kind::ALL {
            let rows: Vec<&Row> = all.iter().copied().filter(|r| r.kind == k).collect();
            totals.insert(k.key().into(), money_of(&rows));
        }
        totals.insert("blocks".into(), blocks.len().into());
        totals.insert("ybTxs".into(), blocks.iter().map(|(_, b)| b.yb_txs.len()).sum::<usize>().into());
        // Counterfactual (C-7): a key that had quoted would have been one more member of E(R).
        let fees: Vec<&Row> = all.iter().copied().filter(|r| r.kind == Kind::Enforcefee).collect();
        let (mut cf_zat, mut cf_usd, mut resolved, mut unresolved, mut empty) = (0.0f64, 0.0f64, 0usize, 0usize, 0usize);
        for r in &fees {
            match r.ref_height.and_then(|h| self.eligible.get(&h)) {
                Some(Some(n)) => {
                    resolved += 1;
                    let share = r.zat as f64 / (*n as f64 + 1.0);
                    cf_zat += share;
                    if let Some(u) = usd(share.round() as i64, price(r.height)) {
                        cf_usd += u;
                    }
                }
                Some(None) => empty += 1,
                None => unresolved += 1,
            }
        }
        let counterfactual = json!({
            "zat": cf_zat.round() as i64, "usd": cf_usd, "feeOutputs": fees.len(), "resolved": resolved, "unresolved": unresolved, "noEligible": empty,
            "label": format!("a quoting pool of any size would have expected ≈ {:.4} YEC in this range", cf_zat / 1e8),
            "method": "Σ fee / (|E(R)| + 1) over the enforcement-fee outputs, E(R) from yed_getfeepayee at each fee's refHeight; uniform selection. Accuracy weighting (FEE-W) makes the realised figure for an honest quoter higher.",
        });
        let releases: Vec<&Row> = all.iter().copied().filter(|r| r.kind == Kind::CollateralRelease).collect();
        let no_enforcement = json!({
            "rows": releases.iter().map(|r| row_json(r, price)).collect::<Vec<_>>(),
            "total": money_of(&releases),
            "label": "collateral taken through the OP_TRUE path while enforcement was off",
        });
        let groups = match by {
            "attestor" => self.by_attestor(&all, &money_of, attestors),
            "block" => blocks.iter().map(|(h, b)| self.block_json(**h, b, &money_of, price)).collect(),
            _ => self.by_payout_key(&blocks, &all, &money_of, miners),
        };
        let rows: Vec<Value> = all.iter().take(MAX_ROWS).map(|r| row_json(r, price)).collect();
        json!({
            "from": from, "to": to, "by": if by == "attestor" || by == "block" { by } else { "payoutKey" },
            "window": self.window().map(|(a, b)| json!({"from": a, "to": b})),
            "priceLabel": PRICE_LABEL,
            "totals": totals,
            "groups": groups,
            "counterfactual": counterfactual,
            "noEnforcement": no_enforcement,
            "rows": rows,
            "rowsTruncated": all.len() > MAX_ROWS,
        })
    }

    fn by_payout_key(&self, blocks: &[(&u64, &BlockLedger)], all: &[&Row], money_of: &dyn Fn(&[&Row]) -> Value, miners: &[Value]) -> Vec<Value> {
        let mut keys: BTreeSet<String> = BTreeSet::new();
        for (_, b) in blocks {
            if let Some(m) = &b.miner {
                keys.insert(self.canonical(m).to_string());
            }
            if let Some(t) = &b.tag {
                keys.insert(t.clone());
            }
        }
        for r in all {
            if r.kind == Kind::Enforcefee {
                keys.insert(self.canonical(&r.payee).to_string());
            }
        }
        for m in miners {
            if let Some(a) = m.get("payoutAddress").and_then(Value::as_str) {
                keys.insert(a.to_string());
            }
        }
        let mut out = Vec::new();
        for k in keys {
            let mine = |r: &&Row| self.canonical(&r.payee) == k;
            let mined = blocks.iter().filter(|(_, b)| b.miner.as_deref().map(|m| self.canonical(m) == k).unwrap_or(false)).count();
            let tags = blocks.iter().filter(|(_, b)| b.tag.as_deref() == Some(&k)).count();
            let fees: Vec<&Row> = all.iter().copied().filter(|r| r.kind == Kind::Enforcefee).filter(mine).collect();
            let subsidy: Vec<&Row> = all.iter().copied().filter(|r| r.kind == Kind::Subsidy).filter(mine).collect();
            let netfee: Vec<&Row> = all.iter().copied().filter(|r| r.kind == Kind::Netfee).filter(mine).collect();
            let stock: Vec<&Row> = subsidy.iter().chain(netfee.iter()).copied().collect();
            let fee_total: i64 = fees.iter().map(|r| r.zat).sum();
            let aliases: Vec<&String> = self.aliases.iter().filter(|(_, v)| **v == k).map(|(a, _)| a).collect();
            let row = miners.iter().find(|m| m.get("payoutAddress").and_then(Value::as_str) == Some(&k));
            out.push(json!({
                "payee": k, "aliases": aliases,
                "blocksMined": mined, "tags": tags, "timesSelected": fees.len(),
                "enforcefee": money_of(&fees), "subsidy": money_of(&subsidy), "netfee": money_of(&netfee), "stockCoinbase": money_of(&stock),
                "perTagZat": if tags > 0 { Some(fee_total / tags as i64) } else { None },
                "perBlockZat": if mined > 0 { Some(fee_total / mined as i64) } else { None },
                "miner": row,
            }));
        }
        out.sort_by(|a, b| b["enforcefee"]["zat"].as_i64().cmp(&a["enforcefee"]["zat"].as_i64()).then_with(|| b["blocksMined"].as_u64().cmp(&a["blocksMined"].as_u64())));
        out
    }

    fn by_attestor(&self, all: &[&Row], money_of: &dyn Fn(&[&Row]) -> Value, attestors: &[Value]) -> Vec<Value> {
        let mut keys: BTreeSet<String> = all.iter().filter(|r| r.kind == Kind::Attestfee).map(|r| r.payee.clone()).collect();
        for a in attestors {
            let known = ["bondKeyAddress", "bondAddress"].iter().any(|f| a.get(*f).and_then(Value::as_str).map(|s| keys.contains(s)).unwrap_or(false));
            if let (false, Some(s)) = (known, a.get("bondKeyAddress").and_then(Value::as_str)) {
                keys.insert(s.to_string());
            }
        }
        let mut out = Vec::new();
        for k in keys {
            let fees: Vec<&Row> = all.iter().copied().filter(|r| r.kind == Kind::Attestfee && r.payee == k).collect();
            let row = attestors.iter().find(|a| ["bondKeyAddress", "bondAddress", "attestorPubKey"].iter().any(|f| a.get(*f).and_then(Value::as_str) == Some(&k)));
            let bond = row.and_then(|a| a.get("bondZat").and_then(Value::as_i64)).unwrap_or(0);
            let total: i64 = fees.iter().map(|r| r.zat).sum();
            out.push(json!({
                "payee": k, "timesSelected": fees.len(), "attestfee": money_of(&fees), "bondZat": bond,
                "realisedYieldBps": if bond > 0 { Some(total * 10_000 / bond) } else { None },
                "yieldLabel": "realised (fees received ÷ bond posted, this range), not promised",
                "attestor": row,
            }));
        }
        out.sort_by(|a, b| b["attestfee"]["zat"].as_i64().cmp(&a["attestfee"]["zat"].as_i64()));
        out
    }

    fn block_json(&self, height: u64, b: &BlockLedger, money_of: &dyn Fn(&[&Row]) -> Value, price: &dyn Fn(u64) -> Option<i64>) -> Value {
        let of = |k: Kind| money_of(&b.rows.iter().filter(|r| r.kind == k).collect::<Vec<_>>());
        let payees: BTreeSet<&str> = b.rows.iter().filter(|r| matches!(r.kind, Kind::Enforcefee | Kind::Attestfee)).map(|r| r.payee.as_str()).collect();
        json!({
            "height": height, "hash": b.hash, "miner": b.miner, "tag": b.tag, "priceUsed": price(height),
            "subsidy": of(Kind::Subsidy), "subsidyOther": of(Kind::SubsidyOther), "netfee": of(Kind::Netfee),
            "enforcefee": of(Kind::Enforcefee), "attestfee": of(Kind::Attestfee), "residual": of(Kind::Residual), "collateralRelease": of(Kind::CollateralRelease),
            "feePayees": payees, "ybTxs": b.yb_txs,
        })
    }
}

fn row_json(r: &Row, price: &dyn Fn(u64) -> Option<i64>) -> Value {
    let p = price(r.height);
    json!({"height": r.height, "txid": r.txid, "vout": r.vout, "kind": r.kind, "zat": r.zat, "payee": r.payee, "usd": usd(r.zat, p), "priceUsed": p.filter(|p| *p > 0), "refHeight": r.ref_height})
}

/// The `revenue` event of a row (USD is added by the collector from the price it knows).
fn emitted(r: &Row) -> Emitted {
    Emitted {
        height: Some(r.height),
        node: None,
        kind: EventKind::Revenue { txid: r.txid.clone(), vout: r.vout, entry: r.kind, zat: r.zat, payee: r.payee.clone(), usd: None, ref_height: r.ref_height },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{RawTransaction, TxOut};

    fn p2pkh(hash: u8) -> Value {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(&[hash; 20]);
        s.extend_from_slice(&[0x88, 0xac]);
        json!({"hex": hex::encode(s), "type": "pubkeyhash"})
    }
    fn tx(txid: &str, outs: &[(f64, Value)]) -> RawTransaction {
        RawTransaction {
            txid: txid.into(),
            hex: String::new(),
            vin: vec![],
            vout: outs.iter().enumerate().map(|(n, (v, s))| TxOut { value: *v, n: n as u32, script_pub_key: s.clone() }).collect(),
            blockhash: None,
            height: None,
            extra: Default::default(),
        }
    }
    fn block(height: u64, hash: &str, txs: Vec<RawTransaction>) -> BlockFull {
        BlockFull { hash: hash.into(), height, confirmations: 1, size: 0, time: 0, chainwork: String::new(), tx: txs, previousblockhash: None, extra: Default::default() }
    }
    fn subsidy(miner: f64, founders: f64, addr: &str) -> BlockSubsidy {
        serde_json::from_value(json!({"miner": miner, "founders": founders, "foundersaddress": addr})).unwrap()
    }

    #[test]
    fn address_round_trip() {
        // a regtest address from the fixtures: prefix 0x1C 0x95, hash160 3adc5831…
        let (p, h) = decode_address("smJQihTy3t6QqGdNimrrWhN1aF8SbwrWmT4").unwrap();
        assert_eq!(p, [0x1c, 0x95]);
        assert_eq!(hex::encode(h), "3adc5831726e1b524b0549a1cea8922e01610c73");
        assert_eq!(encode_address(p, &h), "smJQihTy3t6QqGdNimrrWhN1aF8SbwrWmT4");
        let mut m = RevenueModel::new(100);
        assert_eq!(m.address_of(&json!({"hex": "76a9143adc5831726e1b524b0549a1cea8922e01610c7388ac"})).unwrap(), "hash160:3adc5831726e1b524b0549a1cea8922e01610c73");
        m.learn_prefix("smJQihTy3t6QqGdNimrrWhN1aF8SbwrWmT4");
        assert_eq!(m.address_of(&json!({"hex": "76a9143adc5831726e1b524b0549a1cea8922e01610c7388ac"})).unwrap(), "smJQihTy3t6QqGdNimrrWhN1aF8SbwrWmT4");
        assert_eq!(m.address_of(&json!({"addresses": ["x"], "hex": "00"})).unwrap(), "x");
    }

    #[test]
    fn coinbase_rows_and_reorg() {
        let mut m = RevenueModel::new(100);
        let founders = json!({"addresses": ["fund"], "hex": ""});
        m.set_subsidy(10, &subsidy(2.96875, 0.15625, "fund"));
        // miner takes subsidy + 0.001 fees
        let b = block(10, "aa", vec![tx("cb", &[(2.96975, p2pkh(1)), (0.15625, founders.clone())])]);
        let ev = m.on_block(&b, &[]);
        assert_eq!(ev.len(), 3);
        let l = m.block(10).unwrap();
        let of = |k: Kind| l.rows.iter().find(|r| r.kind == k).map(|r| r.zat);
        assert_eq!(of(Kind::Subsidy), Some(296_875_000));
        assert_eq!(of(Kind::Netfee), Some(100_000));
        assert_eq!(of(Kind::SubsidyOther), Some(15_625_000));
        assert_eq!(l.miner.as_deref(), Some("hash160:0101010101010101010101010101010101010101"));
        assert_eq!(m.totals["netfee"], 100_000);
        // the same height again with another hash replaces the rows and the cumulative counters
        let b2 = block(10, "bb", vec![tx("cb2", &[(2.96875, p2pkh(2)), (0.15625, founders)])]);
        m.on_block(&b2, &[]);
        assert_eq!(m.totals["netfee"], 0);
        assert_eq!(m.by_payee["hash160:0101010101010101010101010101010101010101"].blocks_mined, 0);
        assert_eq!(m.by_payee["hash160:0202020202020202020202020202020202020202"].blocks_mined, 1);
        assert!(!m.wants_block("bb", 10));
        assert!(m.wants_block("cc", 10));
    }

    #[test]
    fn subsidy_other_without_founders_address() {
        // ycashd 6.20.0: `getblocksubsidy` is only {miner, founders, totalblocksubsidy}.
        let s620 = |m: f64, f: f64| -> BlockSubsidy { serde_json::from_value(json!({"miner": m, "founders": f, "totalblocksubsidy": m + f})).unwrap() };
        let mut m = RevenueModel::new(100);
        let fund = json!({"addresses": ["fund"], "hex": ""});
        let pool2 = json!({"addresses": ["pool2"], "hex": ""});
        // (1) one output equals the fund's share: attributed by value, its address learned
        m.set_subsidy(10, &s620(2.96875, 0.15625));
        m.on_block(&block(10, "a", vec![tx("cb", &[(2.96975, p2pkh(1)), (0.15625, fund.clone())])]), &[]);
        let of = |m: &RevenueModel, h: u64, k: Kind| m.block(h).unwrap().rows.iter().filter(|r| r.kind == k).map(|r| (r.zat, r.payee.clone())).collect::<Vec<_>>();
        assert_eq!(of(&m, 10, Kind::SubsidyOther), vec![(15_625_000, "fund".to_string())]);
        assert_eq!(of(&m, 10, Kind::Netfee), vec![(100_000, "hash160:0101010101010101010101010101010101010101".to_string())]);
        // (2) a miner output that also equals the fund's share, listed first: the learned
        //     address wins the tie, and the coinbase still adds up
        m.set_subsidy(11, &s620(2.96875, 0.15625));
        m.on_block(&block(11, "b", vec![tx("cb", &[(0.15625, pool2.clone()), (2.8125, p2pkh(1)), (0.15625, fund.clone())])]), &[]);
        assert_eq!(of(&m, 11, Kind::SubsidyOther), vec![(15_625_000, "fund".to_string())]);
        assert_eq!(of(&m, 11, Kind::Subsidy)[0].0, 296_875_000);
        assert_eq!(of(&m, 11, Kind::Netfee)[0].0, 0);
        // (3) no fund share (founders 0): nothing is taken out of the miner's outputs
        m.set_subsidy(12, &s620(3.125, 0.0));
        m.on_block(&block(12, "c", vec![tx("cb", &[(3.125, p2pkh(1))])]), &[]);
        assert!(of(&m, 12, Kind::SubsidyOther).is_empty());
        assert_eq!(of(&m, 12, Kind::Subsidy)[0].0, 312_500_000);
    }

    #[test]
    fn yellowback_rows_counterfactual_and_query() {
        let mut m = RevenueModel::new(100);
        m.set_subsidy(20, &subsidy(3.0, 0.0, ""));
        let p = crate::classify::decode(
            &{
                let mut d = vec![0x59, 0x42, 0x03, 0x01, 0x00];
                d.extend_from_slice(&10_000u32.to_le_bytes());
                d.extend_from_slice(&380u32.to_le_bytes());
                d.extend_from_slice(&18u32.to_le_bytes());
                d.extend_from_slice(&[0x02; 33]);
                d.extend_from_slice(&[3, 4]);
                d
            },
            2,
        )
        .unwrap();
        let mint = tx("m1", &[(10.0, json!({"hex": "a9", "type": "scripthash"})), (0.0001, p2pkh(9)), (0.0, json!({"hex": "6a"})), (0.5, p2pkh(3)), (0.125, p2pkh(4)), (1.0, p2pkh(9))]);
        let b = block(20, "h20", vec![tx("cb", &[(3.0, p2pkh(1))]), mint]);
        m.on_block(&b, &["m1".into()]);
        m.on_tag(20, "h20", &json!({"found": true, "payoutAddress": "pool1"}));
        assert_eq!(m.canonical("hash160:0101010101010101010101010101010101010101"), "pool1");
        let mut t = YbTx::from_payload("m1", p);
        t.apply(&json!({"type": "mint", "verdict": "ok", "feeZat": 50_000_000, "payee": "pool1", "attestFeeZat": 12_500_000, "attestPayee": "att1", "residualZat": 0}), Some(20));
        let ev = m.on_yb_tx(20, &t, true);
        assert_eq!(ev.len(), 2);
        assert!(m.on_yb_tx(20, &t, true).is_empty(), "idempotent");
        assert_eq!(m.next_ref(), Some((18, 1_000_000_000)), "collateral is the vault output");
        m.set_eligible(18, Some(2));
        assert_eq!(m.next_ref(), None);
        let price = |h: u64| if h == 20 { Some(2_000_000) } else { None };
        let q = m.query(1, 30, "payoutKey", &price, &[json!({"payoutAddress": "pool2", "eligible": true})], &[]);
        assert_eq!(q["totals"]["enforcefee"]["zat"], 50_000_000);
        assert_eq!(q["totals"]["enforcefee"]["usd"], 1.0);
        assert_eq!(q["totals"]["attestfee"]["zat"], 12_500_000);
        assert_eq!(q["totals"]["subsidy"]["zat"], 300_000_000);
        assert_eq!(q["counterfactual"]["zat"], 16_666_667, "fee / (|E(R)| + 1)");
        assert_eq!(q["counterfactual"]["resolved"], 1);
        let g = q["groups"].as_array().unwrap();
        let pool1 = g.iter().find(|g| g["payee"] == "pool1").unwrap();
        assert_eq!(pool1["blocksMined"], 1);
        assert_eq!(pool1["tags"], 1);
        assert_eq!(pool1["timesSelected"], 1);
        assert_eq!(pool1["stockCoinbase"]["zat"], 300_000_000);
        assert!(g.iter().any(|g| g["payee"] == "pool2" && g["blocksMined"] == 0), "a quoting pool with no fee still lists");
        let a = m.query(1, 30, "attestor", &price, &[], &[json!({"bondKeyAddress": "att1", "bondZat": 1_000_000_000, "seq": 1})]);
        assert_eq!(a["groups"][0]["realisedYieldBps"], 125);
        let bl = m.query(20, 20, "block", &price, &[], &[]);
        assert_eq!(bl["groups"][0]["ybTxs"].as_array().unwrap().len(), 1);
        assert_eq!(bl["groups"][0]["enforcefee"]["usd"], 1.0);
        let snap = m.snapshot(&price);
        assert_eq!(snap["byPayee"]["pool1"]["blocksMined"], 1, "aliases roll the coinbase address under the tag's key");
        assert_eq!(snap["totals"]["enforcefee"]["zat"], 50_000_000);
    }

    #[test]
    fn release_when_not_enforcing_and_eviction() {
        let mut m = RevenueModel::new(2);
        let mut r = vec![0x59, 0x42, 0x03, 0x03];
        r.extend_from_slice(&5u32.to_le_bytes());
        r.extend_from_slice(&[1, 0xff, 1, 0, 0x10, 0x27, 0, 0]);
        let p = crate::classify::decode(&r, 2).unwrap();
        let redeem = tx("r1", &[(9.5, p2pkh(7)), (0.5, p2pkh(3)), (0.0, json!({"hex": "6a"}))]);
        for h in 5..=8u64 {
            m.on_block(&block(h, &format!("h{}", h), vec![tx(&format!("cb{}", h), &[(3.0, p2pkh(1))]), redeem.clone()]), &["r1".into()]);
        }
        let mut t = YbTx::from_payload("r1", p);
        t.apply(&json!({"type": "redeem", "path": "owner", "verdict": "ok", "feeZat": 50_000_000, "payee": "pool1"}), Some(8));
        let ev = m.on_yb_tx(8, &t, false);
        assert_eq!(ev.len(), 2);
        let l = m.block(8).unwrap();
        let rel = l.rows.iter().find(|r| r.kind == Kind::CollateralRelease).unwrap();
        assert_eq!((rel.zat, rel.vout), (950_000_000, 0));
        // keep = 2: heights 5 evicted, cumulative counters whole
        assert_eq!(m.window(), Some((6, 8)));
        assert_eq!(m.snapshot(&|_| None)["totals"]["subsidy"]["zat"], 4 * 300_000_000);
        let q = m.query(1, 100, "block", &|_| None, &[], &[]);
        assert_eq!(q["noEnforcement"]["total"]["zat"], 950_000_000);
        assert_eq!(q["totals"]["blocks"], 3);
        assert_eq!(q["totals"]["enforcefee"]["usdComplete"], false);
    }
}

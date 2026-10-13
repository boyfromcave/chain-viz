// Copyright (c) 2026 The Ycash developers
// Distributed under the MIT software license, see the accompanying
// file LICENSE or https://www.opensource.org/licenses/mit-license.php .

//! JSON-RPC 1.0 over HTTP to `ycashd`, one client per node, read-only by construction (the
//! methods here are the plan's §4.1 list; `tests/readonly_gate.rs` greps `src/` for anything
//! from §4.2). Field names of the `yed_*` responses follow
//! `ycash-dd/doc/yellowback-rpc-contract.json` verbatim; parts chain-viz does not model yet
//! stay `serde_json::Value`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;

/// One node as configured (from `devnet.json` or `--nodes`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeConfig {
    /// `devnet.json`'s key (`"0"`..`"7"`) or the `--nodes` index.
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub password: String,
    /// `tcp://host:port` of the node's `-zmqpubhashblock`/`-zmqpubhashtx` socket, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zmq: Option<String>,
    /// The devnet's role name for the seat, when known (`user`, `stock`, `pool`, `attestor`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Parse `<dir>/devnet.json` as `yellowback-devnet up` writes it (`yellowback-devnet:723-731`):
/// `rpc: {"<n>": {url, port, user, password}}`, plus the per-node map C4's devnet adds,
/// `nodes: {"<n>": {zmq: {hashblock, hashtx}}}` (both URLs equal: ycashd publishes every topic
/// on one PUB socket per address, C-F29). An older flat `zmq: {"<n>": "tcp://…"}` map is the
/// fallback. Node ids come back in numeric order.
pub fn nodes_from_devnet(devnet: &Value) -> Result<Vec<NodeConfig>, String> {
    let rpc = devnet.get("rpc").and_then(Value::as_object).ok_or("devnet.json: no \"rpc\" map")?;
    let zmq = devnet.get("zmq").and_then(Value::as_object);
    let per_node = devnet.get("nodes").and_then(Value::as_object);
    let pools: Vec<u64> = devnet.get("pools").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    let attestors: Vec<u64> = devnet.get("attestors").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    let mut nodes = Vec::new();
    for (id, entry) in rpc {
        let raw = entry.get("url").and_then(Value::as_str).ok_or_else(|| format!("devnet.json: rpc.{}: no url", id))?;
        // The devnet's url embeds the credentials as userinfo (`http://rpcuser💻0:rpcpass🔑0@…`,
        // non-ASCII: `qa/rpc-tests/test_framework/util.py`); the separate fields are authoritative
        // and the Authorization header carries them, so the URL is kept host:port only.
        let parsed = node_from_url(id, raw, "", "")?;
        let n: u64 = id.parse().unwrap_or(u64::MAX);
        let role = match n {
            0 => Some("user"),
            1 => Some("stock"),
            _ if pools.contains(&n) => Some("pool"),
            _ if attestors.contains(&n) => Some("attestor"),
            _ => None,
        };
        nodes.push(NodeConfig {
            id: id.clone(),
            url: parsed.url,
            user: entry.get("user").and_then(Value::as_str).map(str::to_string).unwrap_or(parsed.user),
            password: entry.get("password").and_then(Value::as_str).map(str::to_string).unwrap_or(parsed.password),
            zmq: zmq_endpoint(per_node.and_then(|n| n.get(id))).or_else(|| zmq.and_then(|z| z.get(id)).and_then(Value::as_str).map(str::to_string)),
            role: role.map(str::to_string),
        });
    }
    nodes.sort_by_key(|n| n.id.parse::<u64>().unwrap_or(u64::MAX));
    Ok(nodes)
}

/// `nodes[n].zmq`: `{hashblock, hashtx}` (a string is accepted too). One endpoint per node:
/// `hashblock` wins, `hashtx` counts only when it differs (then it is not subscribed; the
/// collector wakes on blocks and refreshes the mempool on the same wake).
fn zmq_endpoint(node: Option<&Value>) -> Option<String> {
    let z = node?.get("zmq")?;
    if let Some(s) = z.as_str() {
        return Some(s.to_string());
    }
    let block = z.get("hashblock").and_then(Value::as_str);
    let tx = z.get("hashtx").and_then(Value::as_str);
    block.or(tx).map(str::to_string)
}

/// Does an RPC URL point at this machine (`http://127.0.0.1:8832`, `[::1]`, `localhost`)?
pub fn is_loopback_url(url: &str) -> bool {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let hostport = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
    let hostport = hostport.split('/').next().unwrap_or("");
    let host = match hostport.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => hostport.rsplit_once(':').map(|(h, _)| h).unwrap_or(hostport),
    };
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Parse one `--nodes` entry: `http://user:pass@host:port` or `http://host:port` (then the
/// `--rpcuser/--rpcpassword` defaults apply). Error text never quotes the URL's userinfo (H-9).
pub fn node_from_url(id: &str, url: &str, default_user: &str, default_password: &str) -> Result<NodeConfig, String> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("--nodes entry {}: not a URL (want http://[user:pass@]host:port)", id))?;
    let (creds, hostport) = match rest.rsplit_once('@') {
        Some((c, h)) => (Some(c), h),
        None => (None, rest),
    };
    let (user, password) = match creds {
        Some(c) => {
            let (u, p) = c.split_once(':').unwrap_or((c, ""));
            (u.to_string(), p.to_string())
        }
        None => (default_user.to_string(), default_password.to_string()),
    };
    if hostport.is_empty() {
        return Err(format!("--nodes entry {}: no host", id));
    }
    Ok(NodeConfig { id: id.to_string(), url: format!("{}://{}", scheme, hostport.trim_end_matches('/')), user, password, zmq: None, role: None })
}

#[derive(Debug, Clone)]
pub enum RpcError {
    /// Transport or HTTP failure: the node is down or unreachable.
    Transport(String),
    /// The node answered with a JSON-RPC error object.
    Node { code: i64, message: String },
    /// The body was not what a JSON-RPC response looks like, or did not fit the typed struct.
    Protocol(String),
}

impl RpcError {
    /// `-32601`: the node does not have the method (a stock node asked a `yed_*` question).
    pub fn is_method_not_found(&self) -> bool {
        matches!(self, RpcError::Node { code: -32601, .. })
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Transport(s) => write!(f, "rpc transport: {}", s),
            RpcError::Node { code, message } => write!(f, "rpc error {}: {}", code, message),
            RpcError::Protocol(s) => write!(f, "rpc protocol: {}", s),
        }
    }
}

impl std::error::Error for RpcError {}

/// Per-method call counts (the budget test in §7 asserts on these).
#[derive(Default, Debug)]
pub struct CallCounter {
    total: AtomicU64,
    by_method: Mutex<BTreeMap<String, u64>>,
}

impl CallCounter {
    pub fn bump(&self, method: &str) {
        self.total.fetch_add(1, Ordering::Relaxed);
        let mut m = self.by_method.lock().unwrap_or_else(|e| e.into_inner());
        *m.entry(method.to_string()).or_insert(0) += 1;
    }
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }
    pub fn snapshot(&self) -> BTreeMap<String, u64> {
        self.by_method.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[derive(Clone)]
pub struct RpcClient {
    node: NodeConfig,
    authorization: String,
    http: reqwest::Client,
    limit: Arc<Semaphore>,
    pub counter: Arc<CallCounter>,
}

impl std::fmt::Debug for RpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the URL or credentials: a Debug print may end up in a log.
        f.debug_struct("RpcClient").field("node", &self.node.id).finish()
    }
}

impl RpcClient {
    /// `concurrency`: in-flight calls allowed per node (the node's own limit is `-rpcthreads`, 4).
    pub fn new(node: NodeConfig, concurrency: usize) -> RpcClient {
        let authorization = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", node.user, node.password)));
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().expect("reqwest client");
        RpcClient { node, authorization, http, limit: Arc::new(Semaphore::new(concurrency.max(1))), counter: Arc::new(CallCounter::default()) }
    }

    pub fn id(&self) -> &str {
        &self.node.id
    }
    pub fn node(&self) -> &NodeConfig {
        &self.node
    }

    /// reqwest's transport errors quote the request URL (`error sending request for url
    /// (http://host:port/)`), and those strings reach `nodes[].error`, `note` events and the log:
    /// the node's address is replaced by its id before the error leaves this client.
    fn scrub(&self, msg: String) -> String {
        let hostport = self.node.url.split_once("://").map(|(_, h)| h).unwrap_or(&self.node.url);
        msg.replace(&self.node.url, &format!("node {}", self.node.id)).replace(hostport, &format!("node {}", self.node.id))
    }

    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let _permit = self.limit.acquire().await.map_err(|e| RpcError::Transport(e.to_string()))?;
        self.counter.bump(method);
        let body = json!({"jsonrpc": "1.0", "id": "chain-viz", "method": method, "params": params});
        let resp = self
            .http
            .post(&self.node.url)
            .header("Authorization", &self.authorization)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| RpcError::Transport(self.scrub(e.to_string())))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| RpcError::Transport(self.scrub(e.to_string())))?;
        let v: Value = serde_json::from_str(&text).map_err(|e| RpcError::Protocol(format!("HTTP {}: {} ({})", status, e, text.chars().take(120).collect::<String>())))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            return Err(RpcError::Node { code: err.get("code").and_then(Value::as_i64).unwrap_or(0), message: err.get("message").and_then(Value::as_str).unwrap_or("").to_string() });
        }
        v.get("result").cloned().ok_or_else(|| RpcError::Protocol("no result".into()))
    }

    async fn typed<T: serde::de::DeserializeOwned>(&self, method: &str, params: Value) -> Result<T, RpcError> {
        let v = self.call(method, params).await?;
        serde_json::from_value(v).map_err(|e| RpcError::Protocol(format!("{}: {}", method, e)))
    }

    pub async fn get_blockchain_info(&self) -> Result<BlockchainInfo, RpcError> {
        self.typed("getblockchaininfo", json!([])).await
    }
    pub async fn get_best_block_hash(&self) -> Result<String, RpcError> {
        self.typed("getbestblockhash", json!([])).await
    }
    pub async fn get_block_hash(&self, height: u64) -> Result<String, RpcError> {
        self.typed("getblockhash", json!([height])).await
    }
    /// `getblock <hash> 1`: header fields and txids.
    pub async fn get_block(&self, hash: &str) -> Result<Block, RpcError> {
        self.typed("getblock", json!([hash, 1])).await
    }
    /// `getblock <hash> 2`: with decoded transactions.
    pub async fn get_block_full(&self, hash: &str) -> Result<BlockFull, RpcError> {
        self.typed("getblock", json!([hash, 2])).await
    }
    pub async fn get_chain_tips(&self) -> Result<Vec<ChainTip>, RpcError> {
        self.typed("getchaintips", json!([])).await
    }
    /// `getrawmempool true`: txid → entry.
    pub async fn get_raw_mempool(&self) -> Result<BTreeMap<String, MempoolEntry>, RpcError> {
        self.typed("getrawmempool", json!([true])).await
    }
    pub async fn get_raw_transaction(&self, txid: &str) -> Result<RawTransaction, RpcError> {
        self.typed("getrawtransaction", json!([txid, 1])).await
    }
    pub async fn get_mempool_info(&self) -> Result<MempoolInfo, RpcError> {
        self.typed("getmempoolinfo", json!([])).await
    }
    pub async fn get_mining_info(&self) -> Result<MiningInfo, RpcError> {
        self.typed("getmininginfo", json!([])).await
    }
    pub async fn get_network_hash_ps(&self) -> Result<f64, RpcError> {
        self.typed("getnetworkhashps", json!([])).await
    }
    pub async fn get_block_subsidy(&self, height: Option<u64>) -> Result<BlockSubsidy, RpcError> {
        self.typed("getblocksubsidy", height.map(|h| json!([h])).unwrap_or(json!([]))).await
    }
    pub async fn yed_getinfo(&self) -> Result<YedInfo, RpcError> {
        self.typed("yed_getinfo", json!([])).await
    }
    pub async fn yed_getstats(&self) -> Result<YedStats, RpcError> {
        self.typed("yed_getstats", json!([])).await
    }
    pub async fn yed_getblockverdict(&self, hash: &str) -> Result<YedBlockVerdict, RpcError> {
        self.typed("yed_getblockverdict", json!([hash])).await
    }
    // ---- C3: the Yellowback health model's reads (plan §4.1); shapes stay `Value` where the
    // UI only relays them. Field names: doc/yellowback-rpc-contract.json rpcversion 6 (in-term).
    // `yed_listclaimable` rows: `claimable` (rpcversion 6) false = listed, not claimable
    // (`model::yellowback::is_claimable_row`).
    pub async fn yed_getprice(&self, height: Option<u64>) -> Result<Value, RpcError> {
        self.call("yed_getprice", height.map(|h| json!([h])).unwrap_or(json!([]))).await
    }
    pub async fn yed_getactivation(&self) -> Result<YedUpgrade, RpcError> {
        self.typed("yed_getactivation", json!([])).await
    }
    pub async fn yed_listminers(&self) -> Result<Vec<Value>, RpcError> {
        self.typed("yed_listminers", json!([])).await
    }
    /// `yed_listvaults [status] [count] [skip]`; `status` empty = every status.
    pub async fn yed_listvaults(&self, status: &str, count: u64, skip: u64) -> Result<Vec<Value>, RpcError> {
        self.typed("yed_listvaults", json!([status, count, skip])).await
    }
    pub async fn yed_listclaimable(&self) -> Result<Vec<Value>, RpcError> {
        self.typed("yed_listclaimable", json!([])).await
    }
    pub async fn yed_listattestors(&self) -> Result<Vec<Value>, RpcError> {
        self.typed("yed_listattestors", json!([])).await
    }
    pub async fn yed_getstatehash(&self) -> Result<YedStateHash, RpcError> {
        self.typed("yed_getstatehash", json!([])).await
    }
    /// `yed_gethistory from to`: at most 2016 rows per call.
    pub async fn yed_gethistory(&self, from: u64, to: u64) -> Result<Vec<Value>, RpcError> {
        self.typed("yed_gethistory", json!([from, to])).await
    }
    /// `yed_gettag <height|blockhash>`.
    pub async fn yed_gettag(&self, block: &str) -> Result<Value, RpcError> {
        self.call("yed_gettag", json!([block])).await
    }
    pub async fn yed_gettxinfo(&self, txid: &str) -> Result<Value, RpcError> {
        self.call("yed_gettxinfo", json!([txid])).await
    }
    pub async fn yed_decodepayload(&self, hex: &str) -> Result<Value, RpcError> {
        self.call("yed_decodepayload", json!([hex])).await
    }
    pub async fn yed_validaterawtransaction(&self, hex: &str) -> Result<Value, RpcError> {
        self.call("yed_validaterawtransaction", json!([hex])).await
    }
    /// `yed_getfeepayee refHeight`: E(R) and the default payee (C4). rpcversion 8 dropped the
    /// `collateralZat` argument (and the `feeZat` field: the fee is a dollar amount of the debt at
    /// the vault's `pMint`); an older node still takes it, so `collateral_zat` is passed only when
    /// given ([`feepayee_args`]).
    pub async fn yed_getfeepayee(&self, ref_height: u64, collateral_zat: Option<i64>) -> Result<Value, RpcError> {
        self.call("yed_getfeepayee", feepayee_args(ref_height, collateral_zat)).await
    }
    // ---- the vault primitive's read RPCs (upgrade plan §15.8, ycash-dd doc/vault-rpc.md). No
    // wallet needed; the act / vault-spend RPCs of the same doc are never called (readonly_gate).
    pub async fn vault_getinfo(&self) -> Result<VaultInfo, RpcError> {
        self.typed("vault_getinfo", json!([])).await
    }
    /// Every set: `set_getinfo`'s fields without `memberlist`.
    pub async fn set_list(&self) -> Result<Vec<Value>, RpcError> {
        self.typed("set_list", json!([])).await
    }
    /// One set with its `memberlist`, predicates at the next block.
    pub async fn set_getinfo(&self, setid: &str) -> Result<Value, RpcError> {
        self.call("set_getinfo", json!([setid])).await
    }
    /// The unspent V and I outputs (no filter: every tag, every set).
    pub async fn vault_list(&self) -> Result<Vec<Value>, RpcError> {
        self.typed("vault_list", json!([{}])).await
    }
    pub async fn vault_decodescript(&self, hex: &str) -> Result<Value, RpcError> {
        self.call("vault_decodescript", json!([hex])).await
    }
}

// ---- stock RPC shapes (Ycash 4.5: src/rpc/blockchain.cpp, mining.cpp) ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockchainInfo {
    pub chain: String,
    pub blocks: u64,
    #[serde(default)]
    pub headers: u64,
    pub bestblockhash: String,
    #[serde(default)]
    pub difficulty: f64,
    #[serde(default)]
    pub chainwork: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `getblock … 1`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Block {
    pub hash: String,
    pub height: u64,
    #[serde(default)]
    pub confirmations: i64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub time: u64,
    #[serde(default)]
    pub chainwork: String,
    #[serde(default)]
    pub tx: Vec<String>,
    #[serde(default)]
    pub previousblockhash: Option<String>,
    #[serde(default)]
    pub nextblockhash: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `getblock … 2`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockFull {
    pub hash: String,
    pub height: u64,
    #[serde(default)]
    pub confirmations: i64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub time: u64,
    #[serde(default)]
    pub chainwork: String,
    #[serde(default)]
    pub tx: Vec<RawTransaction>,
    #[serde(default)]
    pub previousblockhash: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChainTip {
    pub height: u64,
    pub hash: String,
    pub branchlen: u64,
    /// `active`, `valid-fork`, `valid-headers`, `headers-only`, `invalid`.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MempoolEntry {
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub fee: f64,
    #[serde(default)]
    pub time: u64,
    #[serde(default)]
    pub height: u64,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MempoolInfo {
    pub size: u64,
    pub bytes: u64,
    #[serde(default)]
    pub usage: u64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MiningInfo {
    pub blocks: u64,
    #[serde(default)]
    pub difficulty: f64,
    #[serde(default)]
    pub networksolps: f64,
    #[serde(default)]
    pub pooledtx: u64,
    #[serde(default)]
    pub chain: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockSubsidy {
    pub miner: f64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `getrawtransaction <txid> 1` and the elements of `getblock … 2`'s `tx`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawTransaction {
    pub txid: String,
    #[serde(default)]
    pub hex: String,
    #[serde(default)]
    pub vin: Vec<Value>,
    #[serde(default)]
    pub vout: Vec<TxOut>,
    #[serde(default)]
    pub blockhash: Option<String>,
    #[serde(default)]
    pub height: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TxOut {
    pub value: f64,
    #[serde(default)]
    pub n: u32,
    #[serde(rename = "scriptPubKey")]
    pub script_pub_key: Value,
}

// ---- yed_* shapes (doc/yellowback-rpc-contract.json, rpcversion 7 on upgrade/quantum-vaults) ----

/// The `rpcversion` whose shapes the typed structs below follow. chain-viz is an observer and
/// keeps parsing an older node (its typed fields default and it has no `upgrade`); the UI says
/// which nodes report another version rather than refusing them (README, "rpcversion").
/// 6 is the in-term claims line (`upgrade/vault-in-term`, the workspace's
/// `docs/plans/yellowback-in-term-claims-plan.md` §4.1): `yed_listclaimable` lists every ACTIVE
/// vault whose claim branch is open, in term too, with `claimable` true or false, and
/// `yed_getinfo.params` carries `inTermClaims`, `claimThresholdBps` and `earlyRedeemFeeBps`.
/// A 5 node still parses (its rows lack `claimable` and are all claimable).
/// 7 is the post-quantum line (`upgrade/quantum-vaults`, the workspace's
/// `docs/plans/yellowback-quantum-rpc-delta.md`): owners are post-quantum (`ownerScheme`,
/// `ownerHash`, a 53-character `ownerAddress`; `ownerPubKey` / `ownerKeyId` are gone), a MINT's
/// payload is version 4, and `yed_getinfo.params.pq` carries the schemes, the Falcon height and the
/// size-priced fee rate. No typed field here changed, so a 6 node parses the same.
/// 8 is the fee structure (the workspace's `docs/plans/yellowback-quantum-fees-spec.md`, ycash-dd
/// `doc/yellowback-rpc.md` "The fee structure"): the protocol fee is dollars of the debt by band at
/// the vault's stored `pMint` (`params.fees.protocol`), network fees are dollars per KB at the
/// attested tip price (`params.fees.network`), `params.maxMintCents` and `params.supplyCap` are new,
/// every vault row carries `pMint`; `params.feeMinZat` / `params.feeBps` are gone and
/// `yed_getfeepayee` lost its `collateralZat` argument ([`feepayee_args`]). No typed field here
/// changed (the new params ride in the untyped `params`); the UI models 8, 7 and 6
/// ([`MODELLED_RPC_VERSIONS`]) and warns on any other or on a mix.
pub const KNOWN_RPC_VERSION: u32 = 8;
/// The versions whose shapes chain-viz models without a warning.
pub const MODELLED_RPC_VERSIONS: [u32; 3] = [8, 7, 6];

/// The first `rpcversion` whose `yed_getfeepayee` takes no `collateralZat`.
pub const FEEPAYEE_NO_COLLATERAL_FROM: u32 = 8;

/// `yed_getfeepayee`'s positional arguments: `[refHeight]` on rpcversion 8 and later (`None`),
/// `[refHeight, collateralZat]` before it.
pub fn feepayee_args(ref_height: u64, collateral_zat: Option<i64>) -> Value {
    match collateral_zat {
        Some(c) => json!([ref_height, c]),
        None => json!([ref_height]),
    }
}

/// One outpoint of a `yed_*` answer (`{txid, vout}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YedOutpoint {
    pub txid: String,
    pub vout: u32,
}

/// The per-vault cancel limit of a `yed_getvault` / `yed_listvaults` row (the workspace's
/// `docs/plans/yellowback-cancel-limit-design.md`, upgrade plan §15.10 CAN-2, CAN-3; additive,
/// `rpcversion` unchanged). chain-viz relays vault rows untyped; this is the read-only view the UI's
/// vault table shows. `cancelsRequired` is for a cancel in the block after the node's tip:
/// `min(q + 1, CANCEL_SIGNERS_MAX)`, `q` = `cancelCount` inside the quiet period, else 0.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct YedVaultCancel {
    /// The position's cancels counted by the escalation schedule (0 at the mint).
    pub cancel_count: u32,
    /// Distinct current attestor-set members whose co-signatures a cancel of its claim needs.
    pub cancels_required: u32,
    /// The first height the schedule starts from one again; `None` when never cancelled.
    #[serde(default)]
    pub cancel_quiet_until: Option<u64>,
    /// The residual intents of cancelled claims the position holds (CAN-3); absent = none.
    #[serde(default)]
    pub held_residuals: Vec<YedOutpoint>,
}

impl YedVaultCancel {
    /// The cancel limit of one vault row; `None` for a node that predates it (no `cancelsRequired`).
    pub fn of(row: &Value) -> Option<YedVaultCancel> {
        row.get("cancelsRequired")?;
        serde_json::from_value(row.clone()).ok()
    }
}

/// `yed_getinfo` at rpcversion 5 (the vault upgrade, upgrade plan §6, §15.10): `enforcing`,
/// `valveTripped`, `sunset`, `abandoned`, `rejectedBlocks`, `suppressedBlocks`, `activation`
/// and `templatePolicy` are gone; `upgrade` (the `yed_getactivation` object) is new. Anything a
/// node still sends beyond these fields lands in `extra` and is never read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct YedInfo {
    pub enabled: bool,
    /// 5 on the vault upgrade; 0 when absent.
    #[serde(default)]
    pub rpcversion: u32,
    pub height: u64,
    pub blockhash: String,
    #[serde(default)]
    pub chain_height: u64,
    pub healthy: bool,
    /// rpcversion 4 (hardening H-1): a mint at an unarmed reference height is invalid
    /// (`mint-halted-unarmed`).
    #[serde(default)]
    pub mint_requires_armed: bool,
    #[serde(default)]
    pub unhealthy_reason: String,
    #[serde(default)]
    pub network: String,
    /// The upgrade's activation height (U-22).
    #[serde(default, deserialize_with = "null_default")]
    pub start_height: u64,
    #[serde(default)]
    pub supply_cap_reached: bool,
    /// rpcversion 5: the vault upgrade. `None` on an older node.
    #[serde(default)]
    pub upgrade: Option<YedUpgrade>,
    #[serde(default)]
    pub attest: Value,
    #[serde(default)]
    pub miner: Value,
    #[serde(default)]
    pub params: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `yed_getactivation` and `yed_getinfo.upgrade` (rpcversion 5): the vault network upgrade YED
/// is the rule module of. `status` is `active` from `activationHeight`, else `pending`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct YedUpgrade {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub branch_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, deserialize_with = "null_default")]
    pub activation_height: i64,
    /// The network's YED attestor set (a primitive signer set, P4-b).
    #[serde(default, deserialize_with = "null_default")]
    pub attestor_set_id: String,
    #[serde(default)]
    pub claim_delay: u64,
    #[serde(default)]
    pub height: u64,
}

/// `PriceOrNull` fields (`src/rpc/yellowback.cpp`): the node renders an undefined price or
/// ratio as an explicit `null` (`globalRatioBps` before the first vault, `pFast`… while a window
/// is unfilled), which `#[serde(default)]` alone rejects (it covers a missing key only).
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct YedStats {
    pub height: u64,
    #[serde(default)]
    pub supply_cents: i64,
    #[serde(default)]
    pub collateral_zat: i64,
    #[serde(default)]
    pub issued_zat: i64,
    #[serde(default)]
    pub unbacked_cents: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub global_ratio_bps: i64,
    #[serde(default)]
    pub active_vaults: u64,
    #[serde(default)]
    pub void_vaults: u64,
    #[serde(default)]
    pub closed_vaults: u64,
    #[serde(default)]
    pub claimed_vaults: u64,
    #[serde(default, deserialize_with = "null_default")]
    pub p_fast: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub p_mid: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub p_slow: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub p_mint: i64,
    #[serde(default, deserialize_with = "null_default")]
    pub p_claim: i64,
    #[serde(default)]
    pub halt_mask: Vec<String>,
    #[serde(default)]
    pub minting_allowed: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct YedBlockVerdict {
    #[serde(default)]
    pub block_invalid: bool,
    /// rpcversion 5: the first invalid transaction's verdict, `"ok"` when none.
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub transactions: Vec<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct YedStateHash {
    pub height: u64,
    pub blockhash: String,
    pub statehash: String,
}

/// `vault_getinfo` (doc/vault-rpc.md): the upgrade's activation and branch ID, the counts and the
/// set-state hash (`statehash`, equal on two nodes on the same chain).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VaultInfo {
    #[serde(default)]
    pub branchid: String,
    /// -1 when the upgrade is not scheduled.
    #[serde(default)]
    pub activationheight: i64,
    /// Active at the next block.
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub height: u64,
    /// `{hash, height}` of the block the set state is at; null before the first active block.
    #[serde(default)]
    pub dbtip: Value,
    #[serde(default)]
    pub sets: u64,
    #[serde(default)]
    pub vaults: u64,
    #[serde(default)]
    pub intents: u64,
    #[serde(default)]
    pub lockedvalue: f64,
    #[serde(default)]
    pub statehash: String,
    /// The set the registered module (YED) governs on this node, null when it governs none (audit
    /// 01/V-7): two nodes reporting different values compute different set state for the same blocks.
    #[serde(default)]
    pub attestorsetid: Option<String>,
    /// Whether the module governs a set on this node (`attestorsetid` is not null).
    #[serde(default)]
    pub moduleactive: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Per-node counters as `/api/health` reports them.
pub fn counts_by_node(clients: &[RpcClient]) -> HashMap<String, BTreeMap<String, u64>> {
    clients.iter().map(|c| (c.id().to_string(), c.counter.snapshot())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh regtest node reports `globalRatioBps` and the prices as explicit `null` before the
    /// first vault; `#[serde(default)]` alone rejects an explicit null (C-F25).
    #[test]
    fn yed_stats_tolerates_explicit_nulls() {
        let v = json!({"height": 1, "globalRatioBps": null, "pFast": null, "pMid": null, "pSlow": null, "pMint": null, "pClaim": null});
        let s: YedStats = serde_json::from_value(v).expect("nulls decode as defaults");
        assert_eq!(s.global_ratio_bps, 0);
        assert_eq!(s.p_fast, 0);
    }

    #[test]
    fn devnet_json_nodes() {
        let d = json!({"pools": [2,3,4], "attestors": [5,6,7], "rpc": {
            "1": {"url": "http://rpcuser💻1:rpcpass🔑1@127.0.0.1:16001", "port": 16001, "user": "rpcuser💻1", "password": "rpcpass🔑1"},
            "0": {"url": "http://127.0.0.1:16000", "port": 16000, "user": "rt", "password": "rt0"},
            "2": {"url": "http://127.0.0.1:16002", "port": 16002, "user": "rt", "password": "rt2"}},
            "zmq": {"2": "tcp://127.0.0.1:28002"}});
        let n = nodes_from_devnet(&d).unwrap();
        assert_eq!(n.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), ["0", "1", "2"]);
        assert_eq!(n[2].zmq.as_deref(), Some("tcp://127.0.0.1:28002"));
        assert_eq!(n[2].role.as_deref(), Some("pool"));
        assert_eq!(n[1].role.as_deref(), Some("stock"));
        assert_eq!(n[0].password, "rt0");
        assert_eq!(n[1].url, "http://127.0.0.1:16001");
        assert_eq!(n[1].password, "rpcpass🔑1");
    }

    #[test]
    fn devnet_json_nodes_zmq_map() {
        let d = json!({"rpc": {
            "0": {"url": "http://127.0.0.1:16000", "user": "u", "password": "p"},
            "1": {"url": "http://127.0.0.1:16001", "user": "u", "password": "p"},
            "2": {"url": "http://127.0.0.1:16002", "user": "u", "password": "p"}},
            "nodes": {
                "0": {"zmq": {"hashblock": "tcp://127.0.0.1:31516", "hashtx": "tcp://127.0.0.1:31516"}},
                "1": {"zmq": {"hashtx": "tcp://127.0.0.1:31517"}}},
            "zmq": {"0": "tcp://127.0.0.1:1", "2": "tcp://127.0.0.1:31518"}});
        let n = nodes_from_devnet(&d).unwrap();
        assert_eq!(n[0].zmq.as_deref(), Some("tcp://127.0.0.1:31516"), "nodes map wins over the flat map");
        assert_eq!(n[1].zmq.as_deref(), Some("tcp://127.0.0.1:31517"));
        assert_eq!(n[2].zmq.as_deref(), Some("tcp://127.0.0.1:31518"), "flat map is the fallback");
    }

    #[test]
    fn url_credentials() {
        let n = node_from_url("0", "http://u:p@127.0.0.1:18232/", "", "").unwrap();
        assert_eq!((n.url.as_str(), n.user.as_str(), n.password.as_str()), ("http://127.0.0.1:18232", "u", "p"));
        let n = node_from_url("1", "http://127.0.0.1:18232", "du", "dp").unwrap();
        assert_eq!((n.user.as_str(), n.password.as_str()), ("du", "dp"));
        assert!(node_from_url("2", "127.0.0.1", "", "").is_err());
        // H-9: a bad entry's error names the entry, never the password it carried.
        let e = node_from_url("3", "u:hunter2@nohost", "", "").unwrap_err();
        assert!(!e.contains("hunter2") && e.contains("entry 3"), "{}", e);
        let e = node_from_url("4", "http://u:hunter2@", "", "").unwrap_err();
        assert!(!e.contains("hunter2"), "{}", e);
    }

    #[test]
    fn loopback_urls() {
        for u in ["http://127.0.0.1:8832", "http://u:p@localhost:8832/", "http://[::1]:8832", "https://LOCALHOST", "http://127.5.5.5:1"] {
            assert!(is_loopback_url(u), "{}", u);
        }
        for u in ["http://10.0.0.7:8832", "http://u:p@seed.example.org:8832", "http://[fd00::1]:8832", "http://0.0.0.0:8832"] {
            assert!(!is_loopback_url(u), "{}", u);
        }
    }

    #[test]
    fn contract_shapes_parse() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rpc-contract-samples.json")).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        let info: YedInfo = serde_json::from_value(v["yed_getinfo"].clone()).unwrap();
        assert_eq!((info.height, info.rpcversion, info.start_height), (331, KNOWN_RPC_VERSION, 103));
        let up = info.upgrade.as_ref().expect("rpcversion 5 carries the upgrade object");
        // rpcversion 8 (the fee structure): params.fees / maxMintCents / supplyCap, feeMinZat and feeBps gone
        for gone in ["feeMinZat", "feeBps"] {
            assert!(info.params.get(gone).is_none(), "rpcversion 8 has no params.{}", gone);
        }
        assert_eq!(info.params["maxMintCents"], json!(10_000_000));
        assert_eq!(info.params["fees"]["protocol"]["bands"].as_array().map(Vec::len), Some(3));
        assert_eq!(info.params["fees"]["protocol"]["priceSource"], json!("pMint"));
        // the doc says params.pq.feeRateZatPerKB = fees.network.plain.zatPerKB on a live node; the contract's
        // static sample does not keep them equal (100 vs the priced rate), so only the shapes are checked here
        assert!(info.params["fees"]["network"]["plain"]["zatPerKB"].is_u64() && info.params["pq"]["feeRateZatPerKB"].is_u64());
        assert!(info.params["supplyCap"]["circulatingZat"].is_i64());
        assert!(info.params["supplyCap"]["maxVaultShareBps"].is_i64());
        // rpcversion 7 (the quantum line): params.pq and the MINT payload version
        assert_eq!(info.params["pq"]["schemes"], json!([1]));
        assert_eq!(info.params["pq"]["falconActive"], json!(false));
        assert!(info.params["pq"]["feeRateZatPerKB"].is_u64());
        assert_eq!(info.params["attest"]["mintPayloadVersion"], json!(4));
        assert_eq!(info.params["attest"]["payloadVersion"], json!(3));
        // rpcversion 6 (in-term claims): the new params ride in the untyped `params`
        assert_eq!(info.params["inTermClaims"], json!(true));
        assert_eq!(info.params["claimThresholdBps"], json!(12500));
        assert_eq!(info.params["earlyRedeemFeeBps"], json!([500, 250, 100]));
        let rows: Vec<Value> = serde_json::from_value(v["yed_listclaimable"].clone()).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(crate::model::yellowback::is_claimable_row(&rows[0]));
        assert_eq!(rows[0]["underwaterAt"], json!(436700));
        assert!(rows[0]["pMint"].is_i64(), "rpcversion 8: every vault row carries pMint");
        // rpcversion 8, the attestor rules (owner 2026-10-11, D-Q-30, no bump): the bond minimum is computed
        // from vault collateral between a floor and a cap, the set has seats ranked by seat weight, the
        // bond exit delay is BOND_MATURITY after statusHeight (BOND_MIN_LOCK 0); never bondMinUsdCents
        let att = &info.params["attest"];
        for k in ["bondMinZat", "bondMinFloorZat", "bondMinCapZat", "bondShareBps", "bondMaturity", "seats", "seatAgeUnit", "seatAgeCap", "seatMarginBps", "evictMarginBps"] {
            assert!(att[k].is_i64(), "params.attest.{}", k);
        }
        assert!(att["bondMinFloorZat"].as_i64() <= att["bondMinZat"].as_i64() && att["bondMinZat"].as_i64() <= att["bondMinCapZat"].as_i64());
        assert_eq!(att["bondMinLock"], json!(0));
        for never in ["bondMinUsdCents", "bondMinNoPriceZat"] {
            assert!(att.get(never).is_none(), "{} never shipped", never);
        }
        let attestors: Vec<Value> = serde_json::from_value(v["yed_listattestors"].clone()).unwrap();
        assert_eq!(attestors.len(), 1);
        assert_eq!(attestors[0]["evicted"], json!(false));
        // seatWeight is a decimal string in Q16 (zat × 65,536)
        let sw: u128 = attestors[0]["seatWeight"].as_str().unwrap().parse().unwrap();
        assert!(sw >> 16 >= attestors[0]["bondZat"].as_u64().unwrap() as u128, "the tenure factor is at least 1.0");
        assert_eq!((up.name.as_str(), up.branch_id.as_str(), up.status.as_str(), up.activation_height, up.claim_delay), ("Vault", "6d5b7a31", "active", 103, 10));
        assert_eq!(up.attestor_set_id, info.params["attestorSetId"].as_str().unwrap());
        for gone in ["enforcing", "valveTripped", "sunset", "abandoned", "rejectedBlocks", "suppressedBlocks", "activation", "templatePolicy"] {
            assert!(!info.extra.contains_key(gone), "rpcversion 5 has no {}", gone);
        }
        // yed_getactivation is the same object
        let act: YedUpgrade = serde_json::from_value(v["yed_getactivation"].clone()).unwrap();
        assert_eq!(&act, up);
        // an rpcversion-4 node still parses (chain-viz observes, never refuses): no upgrade, its
        // retired fields only in `extra`
        let mut v4 = v["yed_getinfo"].clone();
        let o = v4.as_object_mut().unwrap();
        o.remove("upgrade");
        o.insert("rpcversion".into(), 4.into());
        o.insert("enforcing".into(), true.into());
        o.insert("activation".into(), json!({"status": "active", "signalCount": 64}));
        let old: YedInfo = serde_json::from_value(v4).unwrap();
        assert!(old.upgrade.is_none() && old.rpcversion == 4 && old.extra.contains_key("enforcing"));
        let stats: YedStats = serde_json::from_value(v["yed_getstats"].clone()).unwrap();
        assert_eq!(stats.p_mint, 1990000);
        assert_eq!(stats.supply_cents, 250000);
        let verdict: YedBlockVerdict = serde_json::from_value(v["yed_getblockverdict"].clone()).unwrap();
        assert!(verdict.block_invalid);
        assert_eq!(verdict.verdict, "vault-spend-short-burn");
        assert!(!verdict.extra.contains_key("enforcementOn"));
        assert_eq!(verdict.transactions.len(), 1);
    }

    #[test]
    fn feepayee_args_by_version() {
        assert_eq!(feepayee_args(18, None), json!([18]));
        assert_eq!(feepayee_args(18, Some(1_000_000_000)), json!([18, 1_000_000_000]));
        assert!(MODELLED_RPC_VERSIONS.contains(&KNOWN_RPC_VERSION) && KNOWN_RPC_VERSION >= FEEPAYEE_NO_COLLATERAL_FROM);
    }

    /// The per-vault cancel limit (CAN-2, CAN-3) on the contract's `yed_listvaults` row (a vault never
    /// cancelled: 0, 1, null, no held residual), on a cancelled row, and absent on an older node's row.
    #[test]
    fn vault_cancel_limit_parses() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rpc-contract-samples.json")).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        let row = v["yed_listvaults"][0].clone();
        let never = YedVaultCancel::of(&row).expect("the contract row carries the cancel limit");
        assert_eq!(never, YedVaultCancel { cancel_count: 0, cancels_required: 1, cancel_quiet_until: None, held_residuals: vec![] });
        let mut cancelled = row.clone();
        cancelled["cancelCount"] = json!(2);
        cancelled["cancelsRequired"] = json!(3);
        cancelled["cancelQuietUntil"] = json!(8512);
        cancelled["heldResiduals"] = json!([{ "txid": "c1".repeat(32), "vout": 1 }]);
        let c = YedVaultCancel::of(&cancelled).unwrap();
        assert_eq!((c.cancel_count, c.cancels_required, c.cancel_quiet_until), (2, 3, Some(8512)));
        assert_eq!(c.held_residuals, vec![YedOutpoint { txid: "c1".repeat(32), vout: 1 }]);
        let mut old = row;
        for k in ["cancelCount", "cancelsRequired", "cancelQuietUntil"] {
            old.as_object_mut().unwrap().remove(k);
        }
        assert!(YedVaultCancel::of(&old).is_none());
        // yed_listclaimable rows carry the same three fields (node fix/cancel-fields-2): what a claim faces.
        let claim = YedVaultCancel::of(&v["yed_listclaimable"][0]).expect("a claimable row carries the cancel limit");
        assert_eq!((claim.cancel_count, claim.cancels_required, claim.cancel_quiet_until), (0, 1, None));
    }

    /// The vault primitive's read RPCs as a live devnet on the upgrade answered them.
    #[test]
    fn vault_shapes_parse() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rpc-contract-samples.json")).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        let info: VaultInfo = serde_json::from_value(v["vault_getinfo"].clone()).unwrap();
        assert_eq!((info.branchid.as_str(), info.activationheight, info.active), ("6d5b7a31", 103, true));
        assert_eq!(info.statehash.len(), 64);
        // audit 01/V-7: the module's attestor set and whether it governs one
        assert!(info.moduleactive);
        assert_eq!(info.attestorsetid.as_deref(), v["set_getinfo"]["setid"].as_str());
        let mut none = v["vault_getinfo"].clone();
        none["attestorsetid"] = Value::Null;
        none["moduleactive"] = false.into();
        let none: VaultInfo = serde_json::from_value(none).unwrap();
        assert!(none.attestorsetid.is_none() && !none.moduleactive);
        assert!(info.extra.is_empty(), "every vault_getinfo field is typed: {:?}", info.extra.keys().collect::<Vec<_>>());
        let sets: Vec<Value> = serde_json::from_value(v["set_list"].clone()).unwrap();
        assert!(sets.iter().all(|s| s.get("memberlist").is_none() && s["setid"].is_string() && s["ratelimitbps"].is_u64()));
        assert!(v["set_getinfo"]["memberlist"].as_array().is_some_and(|m| !m.is_empty() && m[0]["key"].is_string() && m[0]["live"].is_boolean()));
        let list: Vec<Value> = serde_json::from_value(v["vault_list"].clone()).unwrap();
        assert!(list.iter().any(|e| e["kind"] == "intent" && e["matureheight"].is_u64() && e["origin"].is_string() && e["cancellable"].is_boolean()));
        assert!(list.iter().any(|e| e["kind"] == "vault" && e["ownerheight"].is_u64() && e["outpoint"].is_string()));
        // the quantum line: every owner is a pqkeyid of a registered scheme; no EC owner key is left
        for e in &list {
            assert!(e.get("ownerkey").is_none(), "{}", e);
            let (scheme, _) = crate::pq::parse_pqkeyid(e["owner"].as_str().unwrap()).unwrap();
            assert_eq!(e["ownerscheme"], scheme);
        }
        assert_eq!(v["vault_decodescript_vault"]["ownerscheme"], 1);
        assert_eq!(v["vault_decodescript_intent"]["type"], "intent");
        assert_eq!(v["vault_decodescript_vault"]["type"], "vault");
    }
}

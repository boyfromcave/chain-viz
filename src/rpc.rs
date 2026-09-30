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
/// on one PUB socket per address, C-F-1). An older flat `zmq: {"<n>": "tcp://…"}` map is the
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

/// Parse one `--nodes` entry: `http://user:pass@host:port` or `http://host:port` (then the
/// `--rpcuser/--rpcpassword` defaults apply).
pub fn node_from_url(id: &str, url: &str, default_user: &str, default_password: &str) -> Result<NodeConfig, String> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("{}: not a URL", url))?;
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
        return Err(format!("{}: no host", url));
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
        f.debug_struct("RpcClient").field("node", &self.node.id).field("url", &self.node.url).finish()
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
            .map_err(|e| RpcError::Transport(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| RpcError::Transport(e.to_string()))?;
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
    // UI only relays them. Field names: doc/yellowback-rpc-contract.json rpcversion 3.
    pub async fn yed_getprice(&self, height: Option<u64>) -> Result<Value, RpcError> {
        self.call("yed_getprice", height.map(|h| json!([h])).unwrap_or(json!([]))).await
    }
    pub async fn yed_getactivation(&self) -> Result<Value, RpcError> {
        self.call("yed_getactivation", json!([])).await
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

// ---- yed_* shapes (doc/yellowback-rpc-contract.json, rpcversion 3) ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct YedInfo {
    pub enabled: bool,
    pub height: u64,
    pub blockhash: String,
    #[serde(default)]
    pub chain_height: u64,
    pub healthy: bool,
    pub enforcing: bool,
    #[serde(default)]
    pub valve_tripped: bool,
    #[serde(default)]
    pub sunset: bool,
    #[serde(default)]
    pub abandoned: bool,
    #[serde(default)]
    pub rejected_blocks: u64,
    #[serde(default)]
    pub suppressed_blocks: u64,
    #[serde(default)]
    pub unhealthy_reason: String,
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub activation: Value,
    #[serde(default)]
    pub attest: Value,
    #[serde(default)]
    pub miner: Value,
    #[serde(default)]
    pub params: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
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
    #[serde(default)]
    pub global_ratio_bps: i64,
    #[serde(default)]
    pub active_vaults: u64,
    #[serde(default)]
    pub void_vaults: u64,
    #[serde(default)]
    pub closed_vaults: u64,
    #[serde(default)]
    pub claimed_vaults: u64,
    #[serde(default)]
    pub p_fast: i64,
    #[serde(default)]
    pub p_mid: i64,
    #[serde(default)]
    pub p_slow: i64,
    #[serde(default)]
    pub p_mint: i64,
    #[serde(default)]
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
    #[serde(default)]
    pub enforcement_on: bool,
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

/// Per-node counters as `/api/health` reports them.
pub fn counts_by_node(clients: &[RpcClient]) -> HashMap<String, BTreeMap<String, u64>> {
    clients.iter().map(|c| (c.id().to_string(), c.counter.snapshot())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn contract_shapes_parse() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rpc-contract-samples.json")).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        let info: YedInfo = serde_json::from_value(v["yed_getinfo"].clone()).unwrap();
        assert_eq!(info.height, 331);
        assert_eq!(info.rejected_blocks, 0);
        let stats: YedStats = serde_json::from_value(v["yed_getstats"].clone()).unwrap();
        assert_eq!(stats.p_mint, 1990000);
        assert_eq!(stats.supply_cents, 250000);
        let verdict: YedBlockVerdict = serde_json::from_value(v["yed_getblockverdict"].clone()).unwrap();
        assert!(verdict.block_invalid);
        assert_eq!(verdict.transactions.len(), 1);
    }
}

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tracing::{info, warn};

use chain_viz::auth::{read_cookie, read_cookie_in};
use chain_viz::bus::Bus;
use chain_viz::collector::{clients, Collector, Model};
use chain_viz::public::{self, PublicState};
use chain_viz::replay::ReplayStatus;
use chain_viz::rpc::{node_from_url, nodes_from_devnet, NodeConfig};
use chain_viz::server::{router, AppState};
use chain_viz::session::Recorder;

/// Real-time x-ray of a Ycash chain and its Yellowback overlay. Read-only: it never holds a
/// key, never broadcasts, never calls a `yed_*` writer.
#[derive(Parser, Debug)]
#[command(name = "chain-viz", version, about, long_about = None)]
struct Cli {
    /// A `yellowback-devnet` directory: reads <dir>/devnet.json for the nodes and credentials,
    /// and heartbeat.json / sim-stats.json beside it.
    #[arg(long, value_name = "DIR")]
    devnet: Option<PathBuf>,
    /// Node RPC URLs, http://user:pass@host:port, comma separated (alternative to --devnet).
    #[arg(long, value_name = "URL[,URL…]", value_delimiter = ',')]
    nodes: Vec<String>,
    /// RPC username applied to every --nodes URL without credentials.
    #[arg(long, value_name = "USER", default_value = "")]
    rpcuser: String,
    /// RPC password applied to every --nodes URL without credentials.
    #[arg(long, value_name = "PASSWORD", default_value = "")]
    rpcpassword: String,
    /// A node's RPC cookie file (`<datadir>/.cookie`, written by ycashd without -rpcuser); its
    /// user:password applies to every --nodes URL without credentials, ahead of --rpcuser.
    #[arg(long, value_name = "PATH", conflicts_with = "datadir")]
    cookie: Option<PathBuf>,
    /// A node's datadir: reads its .cookie (also regtest/.cookie, testnet3/.cookie).
    #[arg(long, value_name = "DIR")]
    datadir: Option<PathBuf>,
    /// ZMQ endpoint of a node: <node id>=<tcp url> (repeatable). Ycash has no getzmqnotifications,
    /// so this or devnet.json's `zmq` map is how chain-viz learns of one.
    #[arg(long, value_name = "ID=URL")]
    zmq: Vec<String>,
    /// HTTP/WS listen address.
    #[arg(long, default_value = "127.0.0.1:8480")]
    listen: SocketAddr,
    /// Poll interval in seconds (default 1 on regtest, 5 otherwise).
    #[arg(long, value_name = "SECS")]
    poll: Option<f64>,
    /// Append every event to <dir>/session.jsonl.
    #[arg(long, value_name = "DIR")]
    record: Option<PathBuf>,
    /// Serve from a recorded session file, no node.
    #[arg(long, value_name = "FILE")]
    replay: Option<PathBuf>,
    /// Replay speed multiplier (with --replay; 0 = as fast as possible).
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
    /// yolo /status URLs (C4; not implemented yet).
    #[arg(long, value_name = "URL[,URL…]", value_delimiter = ',')]
    yolo: Vec<String>,
    /// lightwalletd Prometheus /metrics URL (not implemented yet).
    #[arg(long, value_name = "URL")]
    lightwalletd: Option<String>,
    /// Blocks kept in the model.
    #[arg(long, default_value_t = 5000)]
    keep: u64,
    /// Public mode: per-IP rate limit, WebSocket and /api/events caps, and no node address,
    /// URL, credential or path in any response (node ids stay).
    #[arg(long)]
    public: bool,
    /// Write a static snapshot (index.html + ui/ + snapshot/events/health.json) the UI opens
    /// with no server, every 30 s and at shutdown.
    #[arg(long, value_name = "DIR")]
    export: Option<PathBuf>,
    /// Write our pid here (removed on exit).
    #[arg(long, value_name = "PATH")]
    pid_file: Option<PathBuf>,
    /// In-flight RPC calls allowed per node.
    #[arg(long, default_value_t = 2)]
    rpc_concurrency: usize,
    /// Log level: error, warn, info, debug, trace (or a tracing filter).
    #[arg(long, default_value = "info")]
    log: String,
}

/// SIGINT or SIGTERM (the devnet's `down` sends the latter).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("chain-viz: {}", msg);
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let filter = tracing_subscriber::EnvFilter::try_new(&cli.log).unwrap_or_else(|_| fail("bad --log"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).with_writer(std::io::stderr).init();

    for (flag, set) in [("--yolo", !cli.yolo.is_empty()), ("--lightwalletd", cli.lightwalletd.is_some())] {
        if set {
            warn!("{} is not implemented yet (ignored)", flag);
        }
    }
    let public = cli.public.then(|| Arc::new(PublicState::default()));
    if public.is_some() {
        info!("public mode: {} req/s per client (burst {}), {} websockets, {} events per /api/events", public::RATE, public::BURST, public::MAX_WS, public::EVENTS_CAP);
    }
    if let Some(file) = &cli.replay {
        // No node: the model is rebuilt from the file's events and the same server serves it.
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| fail(&format!("--replay {}: {}", file.display(), e)));
        let events = chain_viz::session::read_session(&text).unwrap_or_else(|e| fail(&format!("--replay {}: {}", file.display(), e)));
        if cli.speed < 0.0 {
            fail("--speed must be >= 0 (0 = as fast as possible)");
        }
        info!("replaying {} ({} events) at speed {}", file.display(), events.len(), cli.speed);
        let status = Arc::new(ReplayStatus::new(file.display().to_string(), events.len() as u64, cli.speed));
        let bus = Arc::new(Bus::new(100_000, None));
        let model = Arc::new(tokio::sync::RwLock::new(Model {
            chain: chain_viz::model::chain::ChainModel::new(cli.keep),
            revenue: chain_viz::model::revenue::RevenueModel::new(cli.keep),
            ..Default::default()
        }));
        tokio::spawn(chain_viz::replay::run(model.clone(), bus.clone(), events, cli.speed, status.clone()));
        serve(Arc::new(AppState { model, bus, clients: Vec::new(), replay: Some(status), public }), cli.listen, cli.pid_file.clone(), cli.export.clone()).await;
        return;
    }

    // Credentials for --nodes URLs without userinfo: the cookie file first, else --rpcuser/--rpcpassword.
    let (rpcuser, rpcpassword) = match (&cli.cookie, &cli.datadir) {
        (Some(path), _) => read_cookie(path).unwrap_or_else(|e| fail(&e)),
        (None, Some(dir)) => read_cookie_in(dir).unwrap_or_else(|e| fail(&e)),
        (None, None) => (cli.rpcuser.clone(), cli.rpcpassword.clone()),
    };

    // Nodes.
    let mut nodes: Vec<NodeConfig> = Vec::new();
    let mut chain_hint: Option<String> = None;
    if let Some(dir) = &cli.devnet {
        let path = dir.join("devnet.json");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| fail(&format!("{}: {}", path.display(), e)));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| fail(&format!("{}: {}", path.display(), e)));
        nodes = nodes_from_devnet(&v).unwrap_or_else(|e| fail(&e));
        chain_hint = Some("regtest".into());
        info!("devnet {}: {} nodes, portseed {}", dir.display(), nodes.len(), v.get("portseed").map(|p| p.to_string()).unwrap_or_default());
    }
    for (i, url) in cli.nodes.iter().enumerate() {
        let id = (nodes.len() + i).to_string();
        nodes.push(node_from_url(&id, url, &rpcuser, &rpcpassword).unwrap_or_else(|e| fail(&e)));
    }
    if nodes.is_empty() {
        fail("no nodes: pass --devnet <dir> or --nodes <url>[,<url>…]");
    }
    let zmq: HashMap<String, String> =
        cli.zmq.iter().map(|s| s.split_once('=').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_else(|| fail(&format!("--zmq {}: want <node id>=<tcp url>", s)))).collect();
    for n in &mut nodes {
        if let Some(u) = zmq.get(&n.id) {
            n.zmq = Some(u.clone());
        }
    }
    let clients = clients(&nodes, cli.rpc_concurrency);

    // Poll interval: --poll, else 1 s on regtest, 5 s otherwise (asked of the first node that answers).
    let chain = match chain_hint {
        Some(c) => c,
        None => {
            let mut found = String::new();
            for c in &clients {
                if let Ok(info) = c.get_blockchain_info().await {
                    found = info.chain;
                    break;
                }
            }
            found
        }
    };
    let poll = Duration::from_secs_f64(cli.poll.unwrap_or(if chain == "regtest" { 1.0 } else { 5.0 }));

    let recorder = cli.record.as_deref().map(|d| Recorder::open(d).unwrap_or_else(|e| fail(&format!("--record {}: {}", d.display(), e))));
    if let Some(r) = &recorder {
        info!("recording to {}", r.path().display());
    }
    let bus = Arc::new(Bus::new(100_000, recorder));
    bus.publish(None, None, chain_viz::session::header(nodes.iter().map(|n| n.id.clone()).collect(), chain.clone()));

    let mut model = Model { chain: chain_viz::model::chain::ChainModel::new(cli.keep), revenue: chain_viz::model::revenue::RevenueModel::new(cli.keep), ..Default::default() };
    model.chain_name = chain;
    let model = Arc::new(tokio::sync::RwLock::new(model));
    let collector = Arc::new(Collector { model: model.clone(), bus: bus.clone(), clients: clients.clone(), poll, devnet_dir: cli.devnet.clone(), backfill: Default::default() });
    collector.start();

    serve(Arc::new(AppState { model, bus, clients, replay: None, public }), cli.listen, cli.pid_file.clone(), cli.export.clone()).await;
}

/// Bind, print the URL, serve until SIGINT/SIGTERM; the pid file lives for the duration.
/// `--export` writes its directory periodically and once more on the way out.
async fn serve(state: Arc<AppState>, listen: SocketAddr, pid_file: Option<PathBuf>, export: Option<PathBuf>) {
    if let Some(p) = &pid_file {
        std::fs::write(p, format!("{}\n", std::process::id())).unwrap_or_else(|e| fail(&format!("--pid-file {}: {}", p.display(), e)));
    }
    if let Some(dir) = &export {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| fail(&format!("--export {}: {}", dir.display(), e)));
        chain_viz::export::spawn(state.clone(), dir.clone());
    }
    if !listen.ip().is_loopback() && state.public.is_none() {
        warn!("listening on a non-loopback address without --public: put a reverse proxy in front or pass --public (README, Hosting)");
    }
    let listener = tokio::net::TcpListener::bind(listen).await.unwrap_or_else(|e| fail(&format!("bind {}: {}", listen, e)));
    let addr = listener.local_addr().unwrap_or(listen);
    println!("listening on http://{}", addr);
    // ConnectInfo gives --public its per-client address.
    let server = axum::serve(listener, router(state.clone()).into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(async {
        shutdown_signal().await;
        info!("shutting down");
    });
    if let Err(e) = server.await {
        warn!("server: {}", e);
    }
    if let Some(dir) = &export {
        chain_viz::export::final_export(&state, dir).await;
    }
    if let Some(p) = pid_file {
        let _ = std::fs::remove_file(p);
    }
}

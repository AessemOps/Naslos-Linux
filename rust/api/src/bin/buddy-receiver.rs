//! Standalone Buddy Backup receiver (port of `cmd/buddy-receiver`): a container
//! with two plain volumes (the backup store and the peer registry) and no ZFS,
//! no Kubernetes and no database. It speaks exactly the protocol a Naslos
//! instance speaks, so a sender does not know - or care - which kind of receiver
//! it has. It stores only ciphertext: the keys never leave the sender.

use std::sync::Arc;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use naslos_api::buddy::{auth::Authenticator, peers::PeerStore, store::Store, PATH_PREFIX};
use naslos_api::server::buddy::{self, BuddyState, Receiver};

/// Read an environment variable, treating an empty value as unset.
fn env_or(key: &str, fallback: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => fallback.to_string(),
    }
}

async fn health() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"status":"ok"}"#,
    )
        .into_response()
}

fn router(receiver: Arc<Receiver>) -> Router {
    Router::new()
        .route(&format!("{PATH_PREFIX}/health"), get(health))
        .route(&format!("{PATH_PREFIX}/status"), get(buddy::status))
        .route(&format!("{PATH_PREFIX}/enroll"), post(buddy::enroll))
        .route(&format!("{PATH_PREFIX}/backups"), get(buddy::backups))
        .route(
            &format!("{PATH_PREFIX}/chunks/{{*source}}"),
            get(buddy::chunks).put(buddy::chunks),
        )
        .route(
            &format!("{PATH_PREFIX}/chains/{{*source}}"),
            get(buddy::chains),
        )
        .route(
            &format!("{PATH_PREFIX}/sequence/{{*source}}"),
            get(buddy::sequence),
        )
        .route(
            &format!("{PATH_PREFIX}/manifest/{{*source}}"),
            get(buddy::manifest).put(buddy::manifest),
        )
        .route(
            &format!("{PATH_PREFIX}/prune/{{*source}}"),
            post(buddy::prune),
        )
        .with_state(BuddyState(receiver))
}

fn human_bytes(n: i64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// A `--health` probe of the running receiver.
async fn probe(url: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if resp.status() != reqwest::StatusCode::OK {
        return Err(format!("{url} returned {}", resp.status()));
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let listen = env_or("BUDDY_LISTEN", ":8484");

    if args.iter().any(|a| a == "--health") {
        let url = std::env::var("BUDDY_HEALTH_URL").unwrap_or_else(|_| {
            let bind = if listen.starts_with(':') {
                format!("127.0.0.1{listen}")
            } else {
                listen.clone()
            };
            format!("http://{bind}{PATH_PREFIX}/health")
        });
        if let Err(e) = probe(&url).await {
            eprintln!("unhealthy: {e}");
            std::process::exit(1);
        }
        return;
    }

    let store_path = env_or("BUDDY_RECEIVE_PATH", "/data");
    let peers_path = env_or("BUDDY_PEERS", "/config/peers.json");

    let peers = Arc::new(PeerStore::new(&peers_path));
    if let Err(e) = peers.load() {
        eprintln!("cannot read the peer registry at {peers_path}: {e}");
        std::process::exit(1);
    }

    let store = Arc::new(Store::new(&store_path));
    let free = match store.free_space() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cannot use the store at {store_path}: {e}");
            std::process::exit(1);
        }
    };

    let auth = Arc::new(Authenticator::new(peers.clone()));
    // Keep the replay cache across restarts: a request captured just before a
    // restart could otherwise be replayed inside the clock-skew window.
    auth.persist_nonces(&store_path);

    let name = env_or("BUDDY_NAME", "buddy-receiver");
    let mut receiver = Receiver::new(store, peers.clone(), auth, &name);
    receiver.version = env_or("BUDDY_VERSION", env!("CARGO_PKG_VERSION"));
    receiver.enroll_token = env_or("BUDDY_ENROLL_TOKEN", "");

    let bind = if listen.starts_with(':') {
        format!("0.0.0.0{listen}")
    } else {
        listen.clone()
    };
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {listen}: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "buddy receiver {} ({}) listening on {}",
        name, receiver.version, listen
    );
    println!("  store:      {} ({} free)", store_path, human_bytes(free));
    println!(
        "  peers:      {} ({} authorized)",
        peers_path,
        peers.count()
    );
    if receiver.enroll_token.is_empty() {
        println!("  enrollment: closed - authorize keys in {peers_path}");
    } else {
        println!("  enrollment: open - the first key that presents the token is authorized");
    }

    let app = router(Arc::new(receiver));
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("receiver stopped: {e}");
        std::process::exit(1);
    }
}

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use clap::{Parser, Subcommand};
use esf_mcp::{Game, Server, data};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::signal::unix::{SignalKind, signal};

const LISTEN: &str = "0.0.0.0:8080";

/// MCP server, over HTTP, that calculates EVE Online ship fits.
#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// A directory with `sde.dat` and `names.dat`. Default: download and cache the newest SDE.
    #[arg(long, env = "SDE_DIR")]
    sde_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Exit 0 when the running server answers its health check.
    Healthcheck,
}

fn load(sde_dir: Option<&Path>) -> Result<Game, String> {
    match sde_dir {
        Some(dir) => Game::new(
            data::read(&dir.join("sde.dat"))?,
            Some(data::read(&dir.join("names.dat"))?),
        ),
        None => {
            let cache_dir = std::env::temp_dir().join("esf-mcp-server");
            data::load(&cache_dir)
        }
    }
}

fn healthcheck() -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", 8080)) else {
        return false;
    };
    let mut response = String::new();
    stream
        .write_all(b"GET /healthz HTTP/1.0\r\n\r\n")
        .and_then(|()| stream.read_to_string(&mut response))
        .is_ok_and(|_| response.starts_with("HTTP/1.0 200") || response.starts_with("HTTP/1.1 200"))
}

async fn shutdown() {
    let mut terminate = signal(SignalKind::terminate()).expect("a SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if let Some(Command::Healthcheck) = args.command {
        std::process::exit(if healthcheck() { 0 } else { 1 });
    }

    let server = match load(args.sde_dir.as_deref()) {
        Ok(game) => Server::new(game),
        Err(error) => {
            eprintln!("mcp-server: {error}");
            std::process::exit(1);
        }
    };

    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .disable_allowed_hosts();
    let mcp = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest_service("/mcp", mcp);

    let listener = match tokio::net::TcpListener::bind(LISTEN).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("mcp-server: {LISTEN}: {error}");
            std::process::exit(1);
        }
    };
    eprintln!("mcp-server: listening on {LISTEN}");
    if let Err(error) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
    {
        eprintln!("mcp-server: {error}");
        std::process::exit(1);
    }
}

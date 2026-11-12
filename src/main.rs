use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use clap::{Parser, Subcommand};
use esf_mcp::{Server, data};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::spawn_blocking;

mod client;
mod eve;
mod limit;
mod login;
mod token;

const LISTEN: &str = "0.0.0.0:8080";
const UPDATE_EVERY: Duration = Duration::from_secs(60 * 60);

/// MCP server, over HTTP, that calculates EVE Online ship fits.
#[derive(Parser)]
#[command(version, about, long_about = None, subcommand_negates_reqs = true)]
struct Args {
    /// Where the downloaded SDE is kept. Default: a directory in the system's temporary directory.
    #[arg(long, env = "CACHE_DIR")]
    cache_dir: Option<PathBuf>,

    /// The address clients reach this server at, like `https://mcp.example.com`.
    #[arg(long, env = "PUBLIC_URL", required_unless_present = "no_login")]
    public_url: Option<String>,

    /// The client id of the EVE Online application to log in with.
    #[arg(long, env = "EVE_CLIENT_ID", required_unless_present = "no_login")]
    eve_client_id: Option<String>,

    /// The secret key of that EVE Online application.
    #[arg(
        long,
        env = "EVE_CLIENT_SECRET",
        required_unless_present = "no_login",
        hide_env_values = true
    )]
    eve_client_secret: Option<String>,

    /// The key tokens are signed with; at least 32 characters, the same on every instance.
    #[arg(
        long,
        env = "SIGNING_KEY",
        required_unless_present = "no_login",
        hide_env_values = true
    )]
    signing_key: Option<String>,

    /// The EVE Online SSO to log in with.
    #[arg(long, env = "EVE_SSO_URL", default_value = eve::SSO_URL)]
    eve_sso_url: String,

    /// How many requests a minute each character may make.
    #[arg(long, env = "RATE_LIMIT", default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..))]
    rate_limit: u32,

    /// Serve without asking anyone to log in.
    #[arg(long, env = "NO_LOGIN")]
    no_login: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Exit 0 when the running server answers its health check.
    Healthcheck,
}

async fn update(cache_dir: PathBuf, mut dir: PathBuf, current: Arc<RwLock<Server>>) {
    let mut interval = tokio::time::interval(UPDATE_EVERY);
    interval.tick().await;
    loop {
        interval.tick().await;
        let (cache, known) = (cache_dir.clone(), dir.clone());
        match blocking(move || data::update(&cache, &known)).await {
            Ok(Some((newest, game))) => {
                *current.write().unwrap_or_else(|error| error.into_inner()) = Server::new(game);
                eprintln!("mcp-server: now on SDE {}", version(&newest));
                dir = newest;
            }
            Ok(None) => {}
            Err(error) => eprintln!("mcp-server: could not update the SDE: {error}"),
        }
    }
}

/// How characters log in, unless `--no-login`.
fn login(args: &Args) -> Result<Option<Arc<login::Login>>, String> {
    if args.no_login {
        return Ok(None);
    }
    let setting = |value: &Option<String>| value.clone().unwrap_or_default();
    login::Login::new(
        &setting(&args.public_url),
        &setting(&args.signing_key),
        setting(&args.eve_client_id),
        setting(&args.eve_client_secret),
        args.eve_sso_url.clone(),
    )
    .map(|login| Some(Arc::new(login)))
}

/// Run `work` on a thread that may block.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    spawn_blocking(work)
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result)
}

fn version(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
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

    let login = match login(&args) {
        Ok(login) => login,
        Err(error) => {
            eprintln!("mcp-server: {error}");
            std::process::exit(1);
        }
    };

    let cache_dir = args
        .cache_dir
        .unwrap_or_else(|| std::env::temp_dir().join("esf-mcp-server"));
    let cache = cache_dir.clone();
    let (dir, game) = match blocking(move || data::load(&cache)).await {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("mcp-server: {error}");
            std::process::exit(1);
        }
    };
    eprintln!("mcp-server: on SDE {}", version(&dir));
    let current = Arc::new(RwLock::new(Server::new(game)));
    tokio::spawn(update(cache_dir, dir, current.clone()));

    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .disable_allowed_hosts();
    let mcp = StreamableHttpService::new(
        move || {
            Ok(current
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone())
        },
        Arc::new(NeverSessionManager::default()),
        config,
    );
    let mcp = Router::new().nest_service("/mcp", mcp);
    let app = match login {
        Some(login) => mcp
            .layer(from_fn_with_state(
                Arc::new(limit::Limits::new(args.rate_limit)),
                limit::limit,
            ))
            .layer(from_fn_with_state(Arc::clone(&login), login::require))
            .layer(login::cors())
            .merge(login::routes(login)),
        None => mcp.layer(login::cors()),
    }
    .route("/healthz", get(|| async { "ok" }));

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

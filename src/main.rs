//! Process entry for the X wire service.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use xfeed::cli::{self, Command};
use xfeed::config::Config;
use xfeed::http::{router, AppState};
use xfeed::images::{prune_media, ImageClient};
use xfeed::store::{Store, UpdateTarget};
use xfeed::xapi::XClient;

#[tokio::main]
async fn main() {
    let command = match cli::parse_args(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("xfeed=info")),
        )
        .with_target(false)
        .compact()
        .init();

    if let Err(err) = run(command).await {
        tracing::error!("{err}");
        std::process::exit(1);
    }
}

async fn run(command: Command) -> Result<(), String> {
    match command {
        Command::Serve { config } => serve(config).await,
        Command::Update { config, target } => update(config, target).await,
        Command::Prune { config } => prune(config).await,
    }
}

async fn serve(path: PathBuf) -> Result<(), String> {
    let store = open_store(&path).await?;
    let listen = store.config().listen.clone();
    let shown = if store.config().base_path.is_empty() {
        "/".to_string()
    } else {
        format!("{}/", store.config().base_path)
    };
    let listener = TcpListener::bind(&listen)
        .await
        .map_err(|err| format!("bind {listen}: {err}"))?;
    tracing::info!(listen = %listen, path = %shown, "listening");
    axum::serve(listener, router(AppState { store }))
        .tcp_nodelay(true)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|err| format!("server stopped: {err}"))?;
    Ok(())
}

async fn update(path: PathBuf, target: UpdateTarget) -> Result<(), String> {
    let store = open_store(&path).await?;
    let reports = store.update(target).await?;
    let mut failed = false;
    for report in &reports {
        match &report.error {
            Some(err) => {
                eprintln!("{}: error: {err}", report.handle);
                failed = true;
            }
            None => eprintln!("{}: stored {}", report.handle, report.stored),
        }
    }
    if failed {
        Err("one or more accounts failed".to_string())
    } else {
        Ok(())
    }
}

async fn prune(path: PathBuf) -> Result<(), String> {
    let config = Config::load(&path).map_err(|err| err.to_string())?;
    let report = prune_media(std::path::Path::new(&config.data_dir))?;
    eprintln!(
        "removed {} unreferenced file(s), kept {}",
        report.removed, report.kept
    );
    Ok(())
}

async fn open_store(path: &std::path::Path) -> Result<Store<XClient, ImageClient>, String> {
    let mut config = Config::load(path).map_err(|err| err.to_string())?;
    let env_token = std::env::var("X_BEARER_TOKEN").ok();
    let from_env = env_token
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    let token = config
        .resolve_bearer_token(env_token.as_deref())
        .map_err(|err| err.to_string())?;
    // Drop the file copy so the long-lived config is not holding the secret.
    config.x_bearer_token.clear();
    tracing::info!(
        source = if from_env {
            "X_BEARER_TOKEN"
        } else {
            "config.yml"
        },
        "bearer token loaded"
    );
    let client = XClient::new(token, config.api_base.clone())?;
    Store::open(config, client).await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutting down");
}

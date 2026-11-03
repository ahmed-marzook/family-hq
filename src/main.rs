use family_hq::AppState;
use family_hq::config::Config;

// `#[tokio::main]` turns `async fn main` into a normal `main` that starts
// the Tokio runtime (the thing that actually runs async code) and then
// runs this function on it. Without a runtime, `.await` can't do anything.
#[tokio::main]
async fn main() {
    // First, so everything after it can log (see `family_hq::logging`).
    family_hq::logging::init();
    let config = Config::from_env();

    // Opens the database (creating it on first run) and applies any new
    // migrations before the server accepts a single request.
    // On failure, explain the usual cause (a folder the program can't write
    // to) instead of panicking with a backtrace hint. `exit(1)` stops the
    // program with an error code, so Docker shows it as failed.
    let db = match family_hq::db::connect(&config.database_url).await {
        Ok(db) => db,
        Err(err) => {
            tracing::error!(
                "could not open the database at {}: {err}\n\
                 Usually the folder isn't writable by the user family-hq runs as \
                 (PUID:PGID, default 1000:1000). On the host, for example:\n  \
                 sudo chown -R 1000:1000 /opt/stacks/family-hq/data",
                config.database_url
            );
            std::process::exit(1);
        }
    };
    tracing::info!("database ready at {}", config.database_url);

    match &config.backup_dir {
        Some(dir) => tracing::info!(
            "backups: daily into {}, keeping {}",
            dir.display(),
            config.backup_keep
        ),
        None => tracing::info!("backups: off (set BACKUP_DIR to turn them on)"),
    }

    let port = config.port;
    let defaults = config.defaults.clone();
    let state = AppState::new(config, db);

    // `tokio::spawn` starts the reminder loop as a separate task that runs
    // alongside the web server. It gets its own clone of the `Arc`. It
    // stops when `main` returns.
    tokio::spawn(family_hq::reminders::run(state.clone()));
    tracing::info!(
        "defaults: {}, {} ({}), prayer method {}",
        defaults.city,
        defaults.country,
        defaults.timezone,
        defaults.prayer_method
    );

    // 0.0.0.0 = listen on every network interface, which is needed inside
    // Docker. `.await` waits for the bind without blocking the thread.
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("failed to bind port");

    tracing::info!("listening on http://localhost:{port}");

    axum::serve(listener, family_hq::app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    tracing::info!("shut down cleanly");
}

/// Finishes when Ctrl+C is pressed. Axum then stops accepting new
/// connections, lets in-flight requests complete, and `serve` returns.
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for Ctrl+C");
}

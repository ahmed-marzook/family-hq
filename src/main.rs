use family_hq::AppState;
use family_hq::config::Config;

// `#[tokio::main]` turns `async fn main` into a normal `main` that starts
// the Tokio runtime (the thing that actually runs async code) and then
// runs this function on it. Without a runtime, `.await` can't do anything.
#[tokio::main]
async fn main() {
    let config = Config::from_env();

    // Opens the database (creating it on first run) and applies any new
    // migrations before the server accepts a single request.
    let db = family_hq::db::connect(&config.database_url)
        .await
        .expect("failed to open database");
    println!("database ready at {}", config.database_url);

    let port = config.port;
    let defaults = config.defaults.clone();
    let state = AppState::new(config, db);

    // `tokio::spawn` starts the reminder loop as a separate task that runs
    // alongside the web server. It gets its own clone of the `Arc`. It
    // stops when `main` returns.
    tokio::spawn(family_hq::reminders::run(state.clone()));
    println!(
        "defaults: {}, {} ({}), prayer method {}",
        defaults.city, defaults.country, defaults.timezone, defaults.prayer_method
    );

    // 0.0.0.0 = listen on every network interface, which is needed inside
    // Docker. `.await` waits for the bind without blocking the thread.
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("failed to bind port");

    println!("listening on http://localhost:{port}");

    axum::serve(listener, family_hq::app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    println!("shut down cleanly");
}

/// Finishes when Ctrl+C is pressed. Axum then stops accepting new
/// connections, lets in-flight requests complete, and `serve` returns.
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for Ctrl+C");
}

use family_hq::config::Config;

// `#[tokio::main]` turns `async fn main` into a normal `main` that starts
// the Tokio runtime (the thing that actually runs async code) and then
// runs this function on it. Without a runtime, `.await` can't do anything.
#[tokio::main]
async fn main() {
    let config = Config::from_env();

    // 0.0.0.0 = listen on every network interface, which is needed inside
    // Docker. `.await` waits for the bind without blocking the thread.
    let addr = ("0.0.0.0", config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind port");

    println!("listening on http://localhost:{}", config.port);

    axum::serve(listener, family_hq::app())
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

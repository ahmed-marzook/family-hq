//! The one place outgoing HTTP clients are set up, so every service the bot
//! calls (GOWA, AlAdhan, Nominatim, Open-Meteo, ntfy) gets the same user
//! agent and, more importantly, the same timeouts.
//!
//! Without a timeout, a server that accepts the connection and then never
//! answers keeps a request waiting forever. The reminder loop awaits each
//! job in turn, so one hung call would quietly stop every reminder.

use std::time::Duration;

use serde::de::DeserializeOwned;

/// Who we are, sent with every request. Nominatim refuses requests without
/// a real User-Agent, and it's polite to the others too.
const USER_AGENT: &str = "family-hq (WhatsApp family bot; github.com/ahmed-marzook/family-hq)";

/// How long to wait for the TCP (and TLS) connection to open. A server
/// that's up answers this in well under a second.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a whole request may take, from connecting to the last byte of
/// the reply. Generous for slow APIs (Open-Meteo's history can take a few
/// seconds), short enough that a hang is noticed within one reminder tick.
const TIMEOUT: Duration = Duration::from_secs(15);

/// The client every service uses: user agent and timeouts set.
pub fn client() -> reqwest::Client {
    client_with(TIMEOUT)
}

/// The same client, but giving up after `timeout` instead. Tests use a
/// short one so they don't wait 15 seconds for a fake server.
pub fn client_with(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        // `min`, so a test's short timeout isn't outlived by the connect one.
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .timeout(timeout)
        .build()
        // `build` only fails if the TLS backend can't start, which happens
        // at startup or never: there's nothing useful to do but stop with
        // a clear message, rather than make every caller handle a `Result`
        // for something that can't go wrong at run time.
        .expect("the HTTP client builds (TLS backend unavailable?)")
}

/// Sends `request` and parses the JSON reply into `T`.
///
/// `T: DeserializeOwned` means "any type serde can build from JSON without
/// borrowing from the input", which is what a reply read off the network
/// needs. The caller picks `T` by the type it stores the result in.
///
/// Fails if the server can't be reached, answers with an error status
/// (4xx/5xx), or sends JSON that doesn't fit `T`.
pub async fn get_json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, reqwest::Error> {
    request
        .send()
        .await?
        // `send` only fails if the server couldn't be reached. A 404 or
        // 500 is still a "successful" HTTP exchange, so turn those into
        // errors too.
        .error_for_status()?
        .json()
        .await
}

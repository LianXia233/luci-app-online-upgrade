// Shared reqwest client (rustls + webpki-roots static TLS stack, no system CA dependency).

use reqwest::Client;
use std::time::Duration;

pub fn client() -> Client {
    Client::builder()
        .user_agent("online-upgrade/2.0")
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .expect("failed to build HTTP client")
}

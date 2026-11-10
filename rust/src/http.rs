// 共享的 reqwest 客户端（使用 rustls + webpki-roots 静态 TLS 栈，不依赖系统 CA）。

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

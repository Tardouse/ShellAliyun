use anyhow::{bail, Context, Result};
use reqwest::{Client, Response, StatusCode};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::time::Duration;

const OPEN_API: &str = "https://openapi.alipan.com";
const API_TIMEOUT: Duration = Duration::from_secs(30);
const OAUTH_EXCHANGE: &str = "OAuth token exchange";

pub struct Api {
    pub http: Client,
    origin: String,
}

impl Api {
    pub fn new() -> Result<Self> {
        Self::at(OPEN_API)
    }

    fn at(origin: &str) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .user_agent(concat!("ShellAliyun/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(API_TIMEOUT)
                .build()
                .context("initialize HTTP client")?,
            origin: origin.to_owned(),
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        token: &str,
        body: &impl Serialize,
    ) -> Result<T> {
        let response = self
            .http
            .post(self.url(path))
            .timeout(API_TIMEOUT)
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .with_context(|| format!("{path} request failed"))?;
        decode(response, path).await
    }

    pub async fn exchange<T: DeserializeOwned>(&self, params: &impl Serialize) -> Result<T> {
        let response = self
            .http
            .post(self.url("/oauth/access_token"))
            .timeout(API_TIMEOUT)
            .form(params)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .context("OAuth token request failed")?;
        decode(response, OAUTH_EXCHANGE).await
    }

    #[cfg(test)]
    pub fn for_test(origin: &str) -> Self {
        Self::at(origin).unwrap()
    }
}

async fn decode<T: DeserializeOwned>(response: Response, action: &str) -> Result<T> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(reqwest::Error::without_url)
        .with_context(|| format!("{action} response interrupted"))?;
    let value = if bytes.is_empty() {
        Ok(Value::Null)
    } else {
        serde_json::from_slice::<Value>(&bytes)
    };

    let code = value
        .as_ref()
        .ok()
        .and_then(|v| v.get("code").or_else(|| v.get("error")))
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 80
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        });

    if status == StatusCode::UNAUTHORIZED {
        bail!("{action}: HTTP {status}. Run 'login' or update SHELLALIYUN_TOKEN.");
    }
    if let Some(code) = code {
        if action == OAUTH_EXCHANGE && code == "InvalidCode" {
            bail!(
                "{action}: HTTP {status} ({code}). Run 'login' again, open the new link, and paste the fresh code into the same terminal prompt."
            );
        }
        bail!("{action}: HTTP {status} ({code})");
    }
    if !status.is_success() {
        bail!("{action}: HTTP {status}");
    }

    let value = value.with_context(|| format!("{action} returned invalid JSON"))?;
    serde_json::from_value(value).map_err(|_| anyhow::anyhow!("{action} returned unexpected JSON"))
}

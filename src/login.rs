use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use colored::Colorize;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{stdin, stdout, Write};
use url::Url;

/// Get a random code_verifier and its corresponding code_challenge (S256)
fn generate_code_verifier_and_challenge() -> (String, String) {
    let code_verifier: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();
    let hash = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = general_purpose::URL_SAFE.encode(&hash);
    (code_verifier, code_challenge)
}

#[derive(Deserialize, Debug)]
struct TokenResponse {
    token_type: String,
    access_token: String,
    expires_in: u64,
}

/// Executing OAuth2 PKCE login flow (does not save token, only prints and sets environment
/// variable)
pub async fn oauth_login() -> Result<String> {
    let (code_verifier, code_challenge) = generate_code_verifier_and_challenge();

    let client_id = "3fab83d6bb26443aa8114c13fd6a5093";
    let redirect_uri = "oob";
    let scope = "user:base,file:all:read,file:all:write";
    let code_challenge_method = "S256";

    let mut auth_url = Url::parse("https://openapi.alipan.com/oauth/authorize")?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", scope)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", code_challenge_method)
        .append_pair("response_type", "code");

    println!("{}", "Visit this URL to authorize:".blue().bold());
    println!("{}", auth_url.as_str().green().underline());

    print!("{}", "Open browser automatically? (y/N): ".yellow());
    stdout().flush()?;
    let mut answer = String::new();
    stdin().read_line(&mut answer)?;
    if answer.trim().eq_ignore_ascii_case("y") {
        let _ = open::that(auth_url.as_str());
    }

    println!(
        "{}",
        "Paste the 'code' parameter from the redirected page:".blue()
    );
    let mut code = String::new();
    stdin().read_line(&mut code)?;
    let code = code.trim();

    let token_url = "https://openapi.alipan.com/oauth/access_token";
    let params = [
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", &code_verifier),
    ];

    let client = Client::new();
    let res = client.post(token_url).form(&params).send().await?;
    if !res.status().is_success() {
        let text = res.text().await?;
        anyhow::bail!("Failed to exchange token: {}", text);
    }

    let token_data: TokenResponse = res.json().await?;
    let token = token_data.access_token;
    println!(
        "{}\n{}",
        "Login successful!".blue().bold(),
        format!("Access Token: {}", token).green()
    );

    std::env::set_var("SHELLALIYUN_TOKEN", &token);
    println!(
        "{}",
        "Token stored temporarily in environment variable SHELLALIYUN_TOKEN (current session only)."
            .blue()
    );

    Ok(token)
}

/// Check if logged in (read from environment variable)
pub fn check_login() -> Result<String> {
    if let Ok(token) = std::env::var("SHELLALIYUN_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }
    anyhow::bail!("Not logged in. Please run 'login' first.")
}

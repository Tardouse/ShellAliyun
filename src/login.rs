use crate::api::Api;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use rand::{distributions::Alphanumeric, Rng};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tempfile::NamedTempFile;

const CLIENT_ID: &str = "3fab83d6bb26443aa8114c13fd6a5093";
const TOKEN_ENV: &str = "SHELLALIYUN_TOKEN";

pub struct Login {
    session: Option<Credentials>,
    use_env: bool,
}

impl Default for Login {
    fn default() -> Self {
        Self {
            session: None,
            use_env: true,
        }
    }
}

impl Login {
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self {
            session: Some(Credentials {
                access_token: "test-token".into(),
                expires_at: u64::MAX,
            }),
            use_env: false,
        }
    }

    pub fn token(&self) -> Result<String> {
        if let Some(credentials) = &self.session {
            return credentials.current_token();
        }
        if self.use_env {
            if let Ok(token) = std::env::var(TOKEN_ENV) {
                if !token.trim().is_empty() {
                    return Ok(token.trim().to_owned());
                }
            }
        }
        let path = credential_path()?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                bail!("Not logged in. Run 'login' or set {TOKEN_ENV}.");
            }
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        let credentials: Credentials = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Invalid saved credentials. Run 'login' again."))?;
        credentials.current_token()
    }

    pub async fn authorize(
        &mut self,
        api: &Api,
        no_browser: bool,
        session_only: bool,
    ) -> Result<()> {
        let verifier: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(64)
            .map(char::from)
            .collect();
        let url = authorization_url(api, &verifier)?;

        eprintln!("Open this URL to authorize ShellAliyun:\n{url}");
        if !no_browser && open::that_detached(url.as_str()).is_err() {
            eprintln!("Could not open a browser. Use the URL above.");
        }
        let prompt = "Paste the one-time authorization code or redirect URL: ";
        let input = if io::stdin().is_terminal() {
            rustyline::DefaultEditor::new()?.readline(prompt)?
        } else {
            eprint!("{prompt}");
            io::stderr().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            input
        };

        let result = exchange_code(api, &verifier, &input).await?;
        if result.access_token.trim().is_empty() || result.expires_in == 0 {
            bail!("OAuth returned an empty or already expired access token");
        }
        let credentials = Credentials {
            access_token: result.access_token,
            expires_at: epoch_seconds()?.saturating_add(result.expires_in),
        };
        if !session_only {
            credentials.save(&credential_path()?)?;
        }
        self.session = Some(credentials);
        Ok(())
    }

    pub fn logout(&mut self) -> Result<()> {
        if let Some(path) = dirs::config_dir().map(|dir| dir.join("shellaliyun/credentials.json")) {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err).with_context(|| format!("remove {}", path.display())),
            }
        }
        self.session = None;
        self.use_env = false;
        Ok(())
    }
}

fn authorization_url(api: &Api, verifier: &str) -> Result<Url> {
    // Keep the padded Base64URL representation used by this app's original Alipan flow.
    let challenge = URL_SAFE.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = Url::parse(&api.url("/oauth/authorize"))?;
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", "oob")
        .append_pair("scope", "user:base,file:all:read,file:all:write")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("response_type", "code");
    Ok(url)
}

async fn exchange_code(api: &Api, verifier: &str, input: &str) -> Result<TokenResponse> {
    let code = authorization_code(input)?;
    api.exchange(&[
        ("client_id", CLIENT_ID),
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("code_verifier", verifier),
    ])
    .await
}

fn authorization_code(input: &str) -> Result<String> {
    let input = input.trim();
    let input = input
        .strip_prefix("授权码：")
        .or_else(|| input.strip_prefix("授权码:"))
        .map_or(input, str::trim);
    let code = if let Ok(url) = Url::parse(input) {
        url.query_pairs()
            .find(|(key, _)| key == "code")
            .map(|(_, code)| code.into_owned())
            .context("The redirect URL has no authorization code")?
    } else if input.starts_with("code=") || input.starts_with("?code=") {
        url::form_urlencoded::parse(input.trim_start_matches('?').as_bytes())
            .find(|(key, _)| key == "code")
            .map(|(_, code)| code.into_owned())
            .context("The copied query has no authorization code")?
    } else {
        input.to_owned()
    };
    if code.is_empty() {
        bail!("No authorization code supplied");
    }
    if code.contains(char::is_whitespace) {
        bail!("Copy only the one-time authorization code or the redirect URL");
    }
    Ok(code)
}

fn credential_path() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|dir| dir.join("shellaliyun/credentials.json"))
        .context(
            "Cannot locate the configuration directory. Use 'login --session-only' in the shell.",
        )
}

fn epoch_seconds() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize, Serialize)]
struct Credentials {
    access_token: String,
    expires_at: u64,
}

impl Credentials {
    fn current_token(&self) -> Result<String> {
        if self.expires_at <= epoch_seconds()? {
            bail!("The login has expired. Run 'login' again.");
        }
        if self.access_token.trim().is_empty() {
            bail!("The saved access token is empty. Run 'login' again.");
        }
        Ok(self.access_token.clone())
    }

    fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().context("Missing configuration directory")?;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(dir)
            .with_context(|| format!("create {}", dir.display()))?;
        let mut temporary = NamedTempFile::new_in(dir)?;
        serde_json::to_writer(&mut temporary, self).context("write OAuth credentials")?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .map_err(|err| err.error)
            .with_context(|| format!("save {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_redirect_or_raw_code() {
        assert_eq!(authorization_code(" abc ").unwrap(), "abc");
        assert_eq!(authorization_code("授权码： abc").unwrap(), "abc");
        assert_eq!(authorization_code("?code=a%2Bb&state=s").unwrap(), "a+b");
        assert_eq!(
            authorization_code("https://example.test/?code=a%2Bb").unwrap(),
            "a+b"
        );
        assert!(authorization_code("https://example.test/?error=denied").is_err());
        assert!(authorization_code("copied page text").is_err());
        assert!(authorization_code("  ").is_err());
    }

    #[tokio::test]
    async fn sends_the_original_alipan_pkce_and_form_format() {
        use crate::test_server::{json_reply, TestServer};
        use serde_json::json;
        use std::collections::HashMap;

        const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM=";
        let server = TestServer::start(|request, _| {
            assert_eq!(request.method(), "POST");
            assert_eq!(request.uri().path(), "/oauth/access_token");
            assert_eq!(
                request.headers()["content-type"],
                "application/x-www-form-urlencoded"
            );
            let form: HashMap<_, _> = url::form_urlencoded::parse(request.body()).collect();
            assert_eq!(form["client_id"], CLIENT_ID);
            assert_eq!(form["grant_type"], "authorization_code");
            assert_eq!(form["code_verifier"], VERIFIER);
            assert_eq!(form["code"], "example+code");
            json_reply(json!({"access_token": "example-token", "expires_in": 3600}))
        })
        .await;
        let api = Api::for_test(&server.origin);
        let url = authorization_url(&api, VERIFIER).unwrap();
        let query: HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(query["client_id"], CLIENT_ID);
        assert_eq!(query["redirect_uri"], "oob");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["code_challenge"], CHALLENGE);
        let result = exchange_code(&api, VERIFIER, "code=example%2Bcode")
            .await
            .unwrap();
        assert_eq!(result.access_token, "example-token");
    }

    #[test]
    fn credentials_are_private_and_replaceable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellaliyun/credentials.json");
        let credentials = Credentials {
            access_token: "first".into(),
            expires_at: 0,
        };
        credentials.save(&path).unwrap();
        assert!(credentials.current_token().is_err());
        let credentials = Credentials {
            access_token: "second".into(),
            expires_at: u64::MAX,
        };
        credentials.save(&path).unwrap();
        let reloaded: Credentials = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(reloaded.current_token().unwrap(), "second");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

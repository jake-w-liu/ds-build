//! ChatGPT-subscription OAuth and credential storage.
//!
//! This module is deliberately independent from `crate::auth`: DeepSeek
//! authentication keeps its existing files, refresh loop, and routing rules.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
use base64::Engine as _;
use fs2::FileExt as _;
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use url::Url;
use uuid::Uuid;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REDIRECT_PORT: u16 = 1455;
const REDIRECT_PATH: &str = "/auth/callback";
const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
const JWT_AUTH_CLAIM: &str = "https://api.openai.com/auth";
const TOKEN_FILE: &str = "openai-codex-auth.json";
const TOKEN_LOCK_FILE: &str = "openai-codex-auth.lock";
const MAX_TOKEN_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TOKEN_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_TOKEN_BYTES: usize = 256 * 1024;
const REFRESH_SKEW: Duration = Duration::from_secs(5 * 60);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

static REFRESH_LOOP_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Tokens {
    pub(super) access: String,
    pub(super) refresh: String,
    /// Absolute Unix epoch time in milliseconds.
    pub(super) expires_at_ms: u64,
    pub(super) account_id: String,
}

impl Tokens {
    pub(crate) fn access(&self) -> &str {
        &self.access
    }

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }

    fn needs_refresh(&self) -> bool {
        self.expires_at_ms <= now_ms().saturating_add(REFRESH_SKEW.as_millis() as u64)
    }
}

#[derive(Clone, Debug)]
struct TokenStore {
    home: PathBuf,
}

impl TokenStore {
    fn production() -> Self {
        Self {
            home: crate::util::ds_home::ds_home(),
        }
    }

    fn token_path(&self) -> PathBuf {
        self.home.join(TOKEN_FILE)
    }

    fn lock_path(&self) -> PathBuf {
        self.home.join(TOKEN_LOCK_FILE)
    }

    fn load(&self) -> anyhow::Result<Option<Tokens>> {
        read_tokens(&self.token_path())
    }

    fn acquire_lock(&self) -> anyhow::Result<File> {
        std::fs::create_dir_all(&self.home)
            .with_context(|| format!("create {}", self.home.display()))?;
        let file = secure_open_lock(&self.lock_path())?;
        file.lock_exclusive()
            .with_context(|| format!("lock {}", self.lock_path().display()))?;
        Ok(file)
    }

    fn save(&self, tokens: &Tokens) -> anyhow::Result<()> {
        let _lock = self.acquire_lock()?;
        write_tokens_atomically(&self.token_path(), tokens)
    }

    fn clear(&self) -> anyhow::Result<bool> {
        let _lock = self.acquire_lock()?;
        remove_if_present(&self.token_path())
    }

    fn clear_if_current(&self, expected: &Tokens) -> anyhow::Result<bool> {
        let _lock = self.acquire_lock()?;
        if self.load()?.as_ref() != Some(expected) {
            return Ok(false);
        }
        remove_if_present(&self.token_path())
    }
}

#[derive(Debug)]
pub(crate) struct ChatgptBearerResolver {
    expected_account_id: String,
}

impl ChatgptBearerResolver {
    pub(crate) fn new(expected_account_id: String) -> Self {
        Self {
            expected_account_id,
        }
    }
}

impl ds_sampler::BearerResolver for ChatgptBearerResolver {
    fn current_bearer(&self) -> Option<String> {
        match load_tokens().ok().flatten() {
            Some(tokens) if tokens.account_id == self.expected_account_id => Some(tokens.access),
            // `None` tells the sampler to reuse its construction-time token.
            // Return a syntactically valid, non-secret override instead so
            // logout/account-switch state fails closed and never sends the
            // construction-time bearer as a fallback.
            _ => Some("invalidated".to_owned()),
        }
    }
}

pub(crate) fn load_tokens() -> anyhow::Result<Option<Tokens>> {
    TokenStore::production().load()
}

pub(crate) fn clear_tokens() -> anyhow::Result<bool> {
    TokenStore::production().clear()
}

pub(crate) fn clear_tokens_if_current(expected: &Tokens) -> anyhow::Result<bool> {
    TokenStore::production().clear_if_current(expected)
}

pub(crate) async fn login() -> anyhow::Result<Tokens> {
    let flow = AuthorizationFlow::new()?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", REDIRECT_PORT))
        .await
        .with_context(|| {
            format!(
                "could not listen on localhost:{REDIRECT_PORT}; close the process using that port"
            )
        })?;

    // stderr is safe for both the standalone CLI and ACP/stdio agents (stdout
    // may be the protocol transport).
    eprintln!("Open this URL to sign in with ChatGPT:\n\n{}\n", flow.url);
    let browser_url = flow.url.clone();
    let browser_opened = tokio::task::spawn_blocking(move || webbrowser::open(&browser_url)).await;
    if !matches!(browser_opened, Ok(Ok(()))) {
        tracing::warn!("could not open the ChatGPT sign-in URL automatically");
    }

    let code = wait_for_callback(listener, &flow.state).await?;
    let tokens = exchange_authorization_code(&code, &flow.verifier).await?;
    TokenStore::production().save(&tokens)?;
    Ok(tokens)
}

pub(crate) async fn ensure_fresh_tokens(force: bool) -> anyhow::Result<Tokens> {
    let store = TokenStore::production();
    let Some(snapshot) = store.load()? else {
        bail!("not signed into ChatGPT; run `ds login --chatgpt`");
    };
    if !force && !snapshot.needs_refresh() {
        return Ok(snapshot);
    }

    // Refresh tokens can rotate. Hold the provider-specific cross-process lock
    // from the generation re-read through the atomic replacement so two ds
    // processes never spend the same refresh token concurrently.
    let lock_store = store.clone();
    let lock = tokio::task::spawn_blocking(move || lock_store.acquire_lock())
        .await
        .context("join ChatGPT refresh lock task")??;
    let current = store
        .load()?
        .ok_or_else(|| anyhow!("ChatGPT was signed out while its session was being refreshed"))?;
    if current != snapshot || (!force && !current.needs_refresh()) {
        drop(lock);
        return Ok(current);
    }

    let refreshed = refresh_access_token(&current).await?;
    if refreshed.account_id != current.account_id {
        drop(lock);
        bail!("ChatGPT returned credentials for a different account during refresh");
    }
    write_tokens_atomically(&store.token_path(), &refreshed)?;
    drop(lock);
    Ok(refreshed)
}

pub(crate) fn spawn_refresh_loop() {
    if load_tokens().ok().flatten().is_none() {
        return;
    }
    if REFRESH_LOOP_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        REFRESH_LOOP_STARTED.store(false, Ordering::Release);
        return;
    };
    handle.spawn(async {
        loop {
            match load_tokens() {
                Ok(Some(tokens)) if tokens.needs_refresh() => {
                    if let Err(error) = ensure_fresh_tokens(false).await {
                        tracing::warn!(%error, "ChatGPT token refresh failed");
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(error) => tracing::warn!(%error, "could not read ChatGPT credentials"),
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        REFRESH_LOOP_STARTED.store(false, Ordering::Release);
    });
}

struct AuthorizationFlow {
    url: String,
    verifier: String,
    state: String,
}

impl AuthorizationFlow {
    fn new() -> anyhow::Result<Self> {
        let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let state = Uuid::new_v4().simple().to_string();
        let mut url = Url::parse(AUTHORIZE_URL)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", CLIENT_ID)
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("scope", SCOPE)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("id_token_add_organizations", "true")
            .append_pair("codex_cli_simplified_flow", "true")
            .append_pair("originator", "codex_cli_rs");
        Ok(Self {
            url: url.to_string(),
            verifier,
            state,
        })
    }
}

async fn wait_for_callback(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> anyhow::Result<String> {
    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    loop {
        let (mut stream, _) = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| anyhow!("ChatGPT sign-in timed out after 10 minutes"))??;
        let mut request = Vec::with_capacity(2048);
        let mut chunk = [0_u8; 2048];
        loop {
            let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                .await
                .map_err(|_| anyhow!("timed out reading the ChatGPT callback"))??;
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            if request.len() > 16 * 1024 {
                write_callback_response(&mut stream, 400, "Invalid callback request").await?;
                request.clear();
                break;
            }
        }

        if request.is_empty() {
            continue;
        }
        let Some(target) = parse_request_target(&request) else {
            write_callback_response(&mut stream, 400, "Invalid callback request").await?;
            continue;
        };
        let Ok(url) = Url::parse(&format!("http://localhost{target}")) else {
            write_callback_response(&mut stream, 400, "Invalid callback URL").await?;
            continue;
        };
        if url.path() != REDIRECT_PATH {
            write_callback_response(&mut stream, 404, "Not found").await?;
            continue;
        }
        let params = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        if params.get("state").map(String::as_str) != Some(expected_state) {
            write_callback_response(&mut stream, 400, "State mismatch").await?;
            continue;
        }
        if let Some(error) = params.get("error") {
            write_callback_response(&mut stream, 400, "ChatGPT sign-in was rejected").await?;
            bail!("ChatGPT sign-in failed: {error}");
        }
        let Some(code) = params.get("code").filter(|value| !value.is_empty()) else {
            write_callback_response(&mut stream, 400, "Missing authorization code").await?;
            continue;
        };
        write_callback_response(
            &mut stream,
            200,
            "ChatGPT sign-in complete. You can close this window.",
        )
        .await?;
        return Ok(code.clone());
    }
}

fn parse_request_target(request: &[u8]) -> Option<&str> {
    let line = std::str::from_utf8(request).ok()?.split("\r\n").next()?;
    let mut parts = line.split_whitespace();
    (parts.next()? == "GET")
        .then(|| parts.next())
        .flatten()
        .filter(|target| target.starts_with('/'))
}

async fn write_callback_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    message: &str,
) -> anyhow::Result<()> {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>ds ChatGPT sign-in</title>\
         <p>{}</p>",
        html_escape(message)
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Deserialize)]
struct RawTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

async fn exchange_authorization_code(code: &str, verifier: &str) -> anyhow::Result<Tokens> {
    let response = http_client()?
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", REDIRECT_URI),
        ])
        .send()
        .await
        .context("exchange ChatGPT authorization code")?;
    parse_token_response(response, None).await
}

async fn refresh_access_token(current: &Tokens) -> anyhow::Result<Tokens> {
    let response = http_client()?
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", current.refresh.as_str()),
            ("scope", SCOPE),
        ])
        .send()
        .await
        .context("refresh ChatGPT access token")?;
    parse_token_response(response, Some(current)).await
}

async fn parse_token_response(
    response: reqwest::Response,
    previous: Option<&Tokens>,
) -> anyhow::Result<Tokens> {
    let status = response.status();
    if !status.is_success() {
        bail!("ChatGPT token endpoint returned HTTP {status}");
    }
    let raw: RawTokenResponse = read_json_bounded(response, MAX_TOKEN_RESPONSE_BYTES).await?;
    let access = valid_token(raw.access_token, "access_token")?;
    let refresh = match raw.refresh_token {
        Some(value) => valid_token(Some(value), "refresh_token")?,
        None => previous
            .map(|tokens| tokens.refresh.clone())
            .ok_or_else(|| anyhow!("ChatGPT token response omitted refresh_token"))?,
    };
    let expires_in = raw
        .expires_in
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| anyhow!("ChatGPT token response omitted a valid expires_in"))?;
    let expires_at_ms = now_ms()
        .checked_add(
            expires_in
                .checked_mul(1000)
                .ok_or_else(|| anyhow!("ChatGPT token expiry overflow"))?,
        )
        .ok_or_else(|| anyhow!("ChatGPT token expiry overflow"))?;
    let account_id = extract_account_id(&access)
        .or_else(|| previous.map(|tokens| tokens.account_id.clone()))
        .ok_or_else(|| anyhow!("ChatGPT access token has no account id"))?;
    Ok(Tokens {
        access,
        refresh,
        expires_at_ms,
        account_id,
    })
}

fn valid_token(value: Option<String>, field: &str) -> anyhow::Result<String> {
    let value = value
        .filter(|token| !token.is_empty())
        .ok_or_else(|| anyhow!("ChatGPT token response omitted {field}"))?;
    if value.len() > MAX_TOKEN_BYTES {
        bail!("ChatGPT {field} exceeded the size limit");
    }
    Ok(value)
}

fn extract_account_id(access_token: &str) -> Option<String> {
    let payload = access_token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get(JWT_AUTH_CLAIM)?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|id| valid_account_id(id))
        .map(str::to_owned)
}

fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(format!("ds/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build ChatGPT HTTP client")
}

pub(crate) async fn read_json_bounded<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    limit: usize,
) -> anyhow::Result<T> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        bail!("ChatGPT response exceeded the {limit}-byte limit");
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read ChatGPT response body")?;
        if body.len().saturating_add(chunk.len()) > limit {
            bail!("ChatGPT response exceeded the {limit}-byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).context("parse ChatGPT response JSON")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn read_tokens(path: &Path) -> anyhow::Result<Option<Tokens>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("stat {}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "ChatGPT credential path is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() > MAX_TOKEN_FILE_BYTES {
        bail!("ChatGPT credential file exceeded the size limit");
    }
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_TOKEN_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_TOKEN_FILE_BYTES {
        bail!("ChatGPT credential file exceeded the size limit");
    }
    let tokens: Tokens =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    validate_stored_tokens(&tokens)?;
    Ok(Some(tokens))
}

fn validate_stored_tokens(tokens: &Tokens) -> anyhow::Result<()> {
    if tokens.access.is_empty()
        || tokens.refresh.is_empty()
        || tokens.account_id.is_empty()
        || tokens.access.len() > MAX_TOKEN_BYTES
        || tokens.refresh.len() > MAX_TOKEN_BYTES
        || !valid_account_id(&tokens.account_id)
        || tokens.expires_at_ms == 0
    {
        bail!("ChatGPT credential file contains invalid fields");
    }
    Ok(())
}

fn write_tokens_atomically(path: &Path, tokens: &Tokens) -> anyhow::Result<()> {
    validate_stored_tokens(tokens)?;
    let bytes = serde_json::to_vec_pretty(tokens)?;
    if bytes.len() as u64 > MAX_TOKEN_FILE_BYTES {
        bail!("ChatGPT credential file exceeded the size limit");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("ChatGPT credential path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{TOKEN_FILE}.tmp-{}-{}",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    let write_result = (|| -> anyhow::Result<()> {
        let mut file = secure_create_new(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        #[cfg(windows)]
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error).with_context(|| format!("replace {}", path.display()));
        }
        std::fs::rename(&temp, path).with_context(|| format!("replace {}", path.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if let Err(error) = std::fs::remove_file(&temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %temp.display(), "could not remove credential temp file");
    }
    write_result
}

fn remove_if_present(path: &Path) -> anyhow::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn secure_create_new(path: &Path) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .with_context(|| format!("create {}", path.display()))
}

fn secure_open_lock(path: &Path) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> Tokens {
        Tokens {
            access: "access".into(),
            refresh: "refresh".into(),
            expires_at_ms: now_ms() + 60_000,
            account_id: "account".into(),
        }
    }

    #[test]
    fn token_store_round_trips_and_clears_only_current_generation() {
        let temp = tempfile::tempdir().unwrap();
        let store = TokenStore {
            home: temp.path().to_path_buf(),
        };
        let first = tokens();
        store.save(&first).unwrap();
        assert_eq!(store.load().unwrap(), Some(first.clone()));

        let mut stale = first.clone();
        stale.access = "stale".into();
        assert!(!store.clear_if_current(&stale).unwrap());
        assert_eq!(store.load().unwrap(), Some(first));
        assert!(store.clear().unwrap());
        assert_eq!(store.load().unwrap(), None);
        assert!(!store.clear().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn token_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let store = TokenStore {
            home: temp.path().to_path_buf(),
        };
        store.save(&tokens()).unwrap();
        let mode = std::fs::metadata(store.token_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn account_id_is_extracted_from_the_namespaced_claim() {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{}");
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_123"}}"#);
        let token = format!("{header}.{payload}.signature");
        assert_eq!(extract_account_id(&token).as_deref(), Some("acct_123"));
        assert_eq!(extract_account_id("not-a-jwt"), None);
    }

    #[test]
    fn authorization_flow_uses_pkce_and_the_registered_callback() {
        let flow = AuthorizationFlow::new().unwrap();
        let url = Url::parse(&flow.url).unwrap();
        let params = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(params.get("client_id").map(String::as_str), Some(CLIENT_ID));
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some(REDIRECT_URI)
        );
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(!flow.verifier.is_empty());
        assert_eq!(params.get("state"), Some(&flow.state));
    }

    #[test]
    fn callback_parser_accepts_only_get_targets() {
        assert_eq!(
            parse_request_target(b"GET /auth/callback?code=x HTTP/1.1\r\n\r\n"),
            Some("/auth/callback?code=x")
        );
        assert_eq!(
            parse_request_target(b"POST /auth/callback HTTP/1.1\r\n\r\n"),
            None
        );
    }
}

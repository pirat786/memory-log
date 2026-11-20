use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{form_urlencoded, Url};
use uuid::Uuid;

use super::windows::{https, open_browser};
use super::SignInAttempt;

const DRIVE: &str = "www.googleapis.com";
const SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";
const BROWSER_TIMEOUT: Duration = Duration::from_secs(60);
pub const PREFIX: &str = "memory-log-sync-v1-";

#[derive(Serialize, Deserialize)]
pub struct Credentials {
    pub client: String,
    pub refresh: String,
    pub account: String,
}

#[derive(Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub permission_id: String,
    pub email_address: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub app_properties: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilePage {
    #[serde(default)]
    files: Vec<File>,
    next_page_token: Option<String>,
    #[serde(default)]
    incomplete_search: bool,
}

fn query(params: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new()).extend_pairs(params.iter().copied()).finish()
}

fn token(client: &str, params: &[(&str, &str)]) -> Result<Token, String> {
    let mut fields = params.to_vec();
    fields.push(("client_id", client));
    if let Some(secret) = option_env!("MEMORY_LOG_GOOGLE_CLIENT_SECRET").filter(|s| !s.is_empty()) {
        // A desktop OAuth client secret is distributed with the app and is not a confidential server credential.
        if Some(client) == option_env!("MEMORY_LOG_GOOGLE_CLIENT_ID") {
            fields.push(("client_secret", secret));
        }
    }
    let form = query(&fields);
    let (status, data) = https("oauth2.googleapis.com", "POST", "/token",
        "Content-Type: application/x-www-form-urlencoded\r\n", form.as_bytes())?;
    if status == 200 {
        return serde_json::from_slice(&data).map_err(|e| e.to_string());
    }
    Err(token_error(status, &data, params.contains(&("grant_type", "authorization_code"))))
}

fn token_error(status: u32, data: &[u8], exchanging_code: bool) -> String {
    #[derive(Deserialize)]
    struct OAuthError<'a> {
        error: &'a str,
        #[serde(default)]
        error_description: String,
    }

    let response = serde_json::from_slice::<OAuthError<'_>>(data).ok();
    match response.as_ref().map(|error| error.error) {
        Some("invalid_grant") if exchanging_code => "sync.code_rejected".into(),
        Some("invalid_grant") => "sync.reauth".into(),
        Some("invalid_client" | "unauthorized_client") => "sync.bad_client".into(),
        Some("invalid_request") if response.is_some_and(|error| error.error_description.contains("client_secret")) => {
            "sync.bad_client".into()
        }
        _ => format!("OAuth HTTP {status}"),
    }
}

fn callback(
    listener: TcpListener,
    expected_state: &str,
    attempt: &SignInAttempt<'_>,
    timeout: Duration,
) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        attempt.check_current()?;
        let (mut stream, _) = match listener.accept() {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        let mut line = String::new();
        let _ = BufReader::new(&stream).take(8192).read_line(&mut line);
        let fields: Vec<_> = line.split_whitespace().collect();
        let params = if fields.first() == Some(&"GET") && fields.get(1).is_some_and(|p| p.starts_with("/oauth?")) {
            Url::parse(&format!("http://127.0.0.1{}", fields[1])).ok().map(|url|
                url.query_pairs().into_owned().collect::<HashMap<_, _>>())
        } else { None };
        let matched = params.as_ref().and_then(|p| p.get("state")).is_some_and(|s| s == expected_state);
        let reply = if matched { "Return to Memory Log to see the sign-in result." } else { "Invalid request." };
        let status = if matched { "200 OK" } else { "400 Bad Request" };
        let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{reply}", reply.len());
        if matched {
            let mut params = params.unwrap_or_default();
            if let Some(error) = params.get("error") {
                return Err(match error.as_str() {
                    "access_denied" => "sync.denied",
                    "server_error" | "temporarily_unavailable" => "sync.oauth_unavailable",
                    _ => "sync.oauth_browser_error",
                }.into());
            }
            attempt.finish_browser()?;
            return params.remove("code").filter(|code| !code.is_empty())
                .ok_or_else(|| "sync.oauth_browser_error".into());
        }
    }
    Err("sync.timeout".into())
}

pub(super) fn sign_in(client: &str, attempt: &SignInAttempt<'_>) -> Result<Token, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let redirect = format!("http://127.0.0.1:{}/oauth", listener.local_addr().map_err(|e| e.to_string())?.port());
    let state = Uuid::new_v4().simple().to_string();
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let args = query(&[
        ("client_id", client), ("redirect_uri", &redirect), ("response_type", "code"),
        ("scope", SCOPE), ("access_type", "offline"), ("prompt", "consent"), ("state", &state),
        ("code_challenge", &challenge), ("code_challenge_method", "S256"),
    ]);
    attempt.check_current()?;
    open_browser(&format!("https://accounts.google.com/o/oauth2/v2/auth?{args}"))?;
    let code = callback(listener, &state, attempt, BROWSER_TIMEOUT)?;
    token(client, &[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", &redirect), ("code_verifier", &verifier)])
}

pub fn refresh(credentials: &Credentials) -> Result<Token, String> {
    token(&credentials.client, &[("grant_type", "refresh_token"), ("refresh_token", &credentials.refresh)])
}

pub struct Drive<'a>(pub &'a str);

impl Drive<'_> {
    fn request(&self, method: &str, path: &str, content_type: Option<&str>, body: &[u8]) -> Result<Vec<u8>, String> {
        let mut headers = format!("Authorization: Bearer {}\r\n", self.0);
        if let Some(content_type) = content_type {
            headers.push_str(&format!("Content-Type: {content_type}\r\n"));
        }
        let (status, contents) = https(DRIVE, method, path, &headers, body)?;
        if (200..300).contains(&status) {
            Ok(contents)
        } else {
            Err(format!("Google Drive HTTP {status}"))
        }
    }

    pub fn user(&self) -> Result<User, String> {
        #[derive(Deserialize)] struct About { user: User }
        let contents = self.request("GET", "/drive/v3/about?fields=user(permissionId,emailAddress)", None, &[])?;
        Ok(serde_json::from_slice::<About>(&contents).map_err(|e| e.to_string())?.user)
    }

    pub fn files(&self) -> Result<BTreeMap<String, File>, String> {
        let mut all = BTreeMap::<String, File>::new();
        let mut cursor = None::<String>;
        let filter = format!("trashed = false and name contains '{PREFIX}'");
        loop {
            let mut params = vec![
                ("spaces", "appDataFolder"), ("q", filter.as_str()), ("pageSize", "1000"),
                ("fields", "nextPageToken,incompleteSearch,files(id,name,version,appProperties)"),
            ];
            if let Some(token) = cursor.as_deref() { params.push(("pageToken", token)); }
            let path = format!("/drive/v3/files?{}", query(&params));
            let page: FilePage = serde_json::from_slice(&self.request("GET", &path, None, &[])?).map_err(|e| e.to_string())?;
            if page.incomplete_search {
                return Err("Google Drive returned an incomplete file list".into());
            }
            for file in page.files {
                if all.get(&file.name).is_none_or(|old| {
                    (version_number(&file.version), &file.id) > (version_number(&old.version), &old.id)
                }) {
                    all.insert(file.name.clone(), file);
                }
            }
            cursor = page.next_page_token.filter(|token| !token.is_empty());
            if cursor.is_none() { return Ok(all); }
        }
    }

    pub fn download(&self, file: &File) -> Result<Vec<u8>, String> {
        let id: String = form_urlencoded::byte_serialize(file.id.as_bytes()).collect();
        self.request("GET", &format!("/drive/v3/files/{id}?alt=media"), None, &[])
    }

    pub fn delete(&self, file: &File) -> Result<(), String> {
        let id: String = form_urlencoded::byte_serialize(file.id.as_bytes()).collect();
        match self.request("DELETE", &format!("/drive/v3/files/{id}"), None, &[]) {
            Ok(_) => Ok(()),
            Err(error) if error == "Google Drive HTTP 404" => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn upload(&self, name: &str, bytes: &[u8], previous: Option<&File>) -> Result<(), String> {
        let digest = format!("{:x}", Sha256::digest(bytes));
        if previous.and_then(|file| file.app_properties.get("sha256")) == Some(&digest) {
            return Ok(());
        }
        let mut metadata = serde_json::json!({ "name": name, "appProperties": { "sha256": digest } });
        let (method, path) = match previous {
            Some(file) => {
                let id: String = form_urlencoded::byte_serialize(file.id.as_bytes()).collect();
                ("PATCH", format!("/upload/drive/v3/files/{id}?uploadType=multipart"))
            }
            None => {
                metadata["parents"] = serde_json::json!(["appDataFolder"]);
                ("POST", "/upload/drive/v3/files?uploadType=multipart".into())
            }
        };
        let boundary = Uuid::new_v4().simple().to_string();
        let mut body = format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: application/gzip\r\n\r\n").into_bytes();
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        self.request(method, &path, Some(&format!("multipart/related; boundary={boundary}")), &body)?;
        Ok(())
    }
}

fn version_number(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::token_error;

    #[test]
    fn identifies_missing_desktop_client_secret() {
        let response = br#"{"error":"invalid_request","error_description":"client_secret is missing."}"#;
        assert_eq!(token_error(400, response, true), "sync.bad_client");
    }

    #[test]
    fn distinguishes_rejected_sign_in_from_an_expired_connection() {
        let response = br#"{"error":"invalid_grant"}"#;
        assert_eq!(token_error(400, response, true), "sync.code_rejected");
        assert_eq!(token_error(400, response, false), "sync.reauth");
    }
}

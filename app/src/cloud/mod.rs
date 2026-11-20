mod google;
#[cfg(windows)]
mod windows;
#[cfg(not(windows))]
mod windows {
    pub fn https(_: &str, _: &str, _: &str, _: &str, _: &[u8]) -> Result<(u32, Vec<u8>), String> {
        Err("sync.windows_only".into())
    }
    pub fn open_browser(_: &str) -> Result<(), String> { Err("sync.windows_only".into()) }
    pub fn write_credentials(_: &[u8]) -> Result<(), String> { Err("sync.windows_only".into()) }
    pub fn read_credentials() -> Result<Vec<u8>, String> { Err("sync.windows_only".into()) }
    pub fn delete_credentials() {}
}

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use google::{Credentials, Drive, Token};
use ml_core::model::OverrideScope;
use ml_storage::sync::{Device, Link, Shard, DAY_SECONDS, META_DAY};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::state::AppState;

const UPLOAD_LIMIT: u32 = 12;
const DOWNLOAD_LIMIT: usize = 16;
const SHARD_LIMIT: u64 = 64 * 1024 * 1024;
const NETWORK_TIMEOUT: &str = "sync.network_timeout";
const FIRST_TIMEOUT_RETRY: Duration = Duration::from_secs(5);
const BACKGROUND_SYNC_INTERVAL: Duration = Duration::from_secs(60);

struct CachedToken {
    account: String,
    access: String,
    expires: Instant,
}

#[derive(Default)]
pub struct SyncControl {
    gate: Mutex<()>,
    stop_requested: AtomicBool,
    sender: Mutex<Option<mpsc::Sender<()>>>,
    token: Mutex<Option<CachedToken>>,
    error: Mutex<Option<String>>,
    progress: Mutex<Option<SyncProgress>>,
    sign_in: Mutex<SignInStatus>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SignInPhase {
    Browser,
    Finishing,
}

#[derive(Default)]
struct SignInStatus {
    current: Option<(Uuid, SignInPhase)>,
    error: Option<String>,
}

struct SignInAttempt<'a> {
    status: &'a Mutex<SignInStatus>,
    id: Uuid,
}

impl<'a> SignInAttempt<'a> {
    fn start(status: &'a Mutex<SignInStatus>) -> Result<Self, String> {
        let mut state = status.lock().map_err(|e| e.to_string())?;
        if matches!(state.current, Some((_, SignInPhase::Finishing))) {
            return Err("sync.sign_in_finishing".into());
        }
        let id = Uuid::new_v4();
        state.current = Some((id, SignInPhase::Browser));
        state.error = None;
        Ok(Self { status, id })
    }

    fn check_current(&self) -> Result<(), String> {
        let state = self.status.lock().map_err(|e| e.to_string())?;
        if state.current.is_some_and(|(id, _)| id == self.id) {
            Ok(())
        } else {
            Err("sync.sign_in_replaced".into())
        }
    }

    fn finish_browser(&self) -> Result<(), String> {
        let mut state = self.status.lock().map_err(|e| e.to_string())?;
        if state.current != Some((self.id, SignInPhase::Browser)) {
            return Err("sync.sign_in_replaced".into());
        }
        state.current = Some((self.id, SignInPhase::Finishing));
        Ok(())
    }

    fn record_result(self, result: &Result<(), String>) {
        if let Ok(mut state) = self.status.lock() {
            if state.current.is_some_and(|(id, _)| id == self.id) {
                state.current = None;
                state.error = result.as_ref().err().cloned();
            }
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
enum SyncProgress {
    Preparing,
    Uploading { done: usize, total: usize },
    Downloading { done: usize, total: usize },
    Finishing,
    Waiting,
}

#[derive(Serialize)]
pub struct SyncStatus {
    device: Device,
    peers: Vec<Device>,
    enabled: bool,
    email: String,
    synced_at: Option<i64>,
    busy: bool,
    configured_client: bool,
    error: Option<String>,
    progress: Option<SyncProgress>,
    sign_in: Option<SignInPhase>,
    sign_in_error: Option<String>,
}

fn default_client() -> Option<&'static str> {
    option_env!("MEMORY_LOG_GOOGLE_CLIENT_ID").filter(|id| !id.is_empty())
}

fn validated_label(label: &str) -> Result<&str, String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
        return Err("sync.bad_label".into());
    }
    Ok(label)
}

fn filename(device: &str, day: i64) -> String {
    let part = if day == META_DAY { "meta".to_string() } else { day.to_string() };
    format!("{}{device}-{part}.json.gz", google::PREFIX)
}

fn parse_filename(name: &str) -> Option<(String, i64)> {
    let (device, part) = name.strip_prefix(google::PREFIX)?.strip_suffix(".json.gz")?.rsplit_once('-')?;
    let id = Uuid::parse_str(device).ok()?;
    if id.simple().to_string() != device { return None; }
    let day = if part == "meta" { META_DAY } else { part.parse().ok()? };
    (day == META_DAY || (0..100_000).contains(&day)).then(|| (device.to_string(), day))
}

fn serialize_shard(shard: &Shard) -> Result<Vec<u8>, String> {
    let mut gzip = GzEncoder::new(Vec::new(), Compression::fast());
    serde_json::to_writer(&mut gzip, shard).map_err(|e| e.to_string())?;
    gzip.finish().map_err(|e| e.to_string())
}

fn deserialize_shard(bytes: &[u8], device: &str, day: i64) -> Result<Shard, String> {
    let mut json = Vec::new();
    GzDecoder::new(bytes).take(SHARD_LIMIT + 1).read_to_end(&mut json).map_err(|e| e.to_string())?;
    if json.len() as u64 > SHARD_LIMIT { return Err("synchronization archive is too large".into()); }
    let shard: Shard = serde_json::from_slice(&json).map_err(|e| e.to_string())?;
    let in_day = |ts: i64| ts.div_euclid(DAY_SECONDS) == day;
    let valid = if day == META_DAY {
        shard.events.is_empty() && shard.bookmarks.is_empty()
            && shard.label.as_deref().is_some_and(|label| validated_label(label).is_ok())
            && shard.rules.iter().all(|r| r.scope != OverrideScope::Visit)
    } else {
        shard.label.is_none() && shard.events.iter().all(|event| in_day(event.timestamp()))
            && shard.bookmarks.iter().all(|bookmark| in_day(bookmark.ts))
            && shard.rules.iter().all(|r| r.scope == OverrideScope::Visit && in_day(r.ts))
    };
    if shard.protocol != 1 || shard.device != device || shard.day != day || !valid {
        return Err("invalid synchronization archive".into());
    }
    Ok(shard)
}

impl SyncControl {
    fn update_error(&self, error: Option<String>) {
        if let Ok(mut stored) = self.error.lock() { *stored = error; }
    }

    fn update_progress(&self, progress: Option<SyncProgress>) {
        if let Ok(mut stored) = self.progress.lock() { *stored = progress; }
    }

    fn set_token(&self, account: &str, token: Token) -> Result<(), String> {
        let seconds = token.expires_in.unwrap_or(3600).clamp(60, 86_400);
        let access = token.access_token;
        if access.is_empty() || access.chars().any(char::is_control) {
            return Err("invalid access token response".into());
        }
        *self.token.lock().map_err(|e| e.to_string())? = Some(CachedToken {
            account: account.to_string(), access, expires: Instant::now() + Duration::from_secs(seconds - 30),
        });
        Ok(())
    }

    fn access_token(&self, link: &Link) -> Result<String, String> {
        if let Some(token) = self.token.lock().map_err(|e| e.to_string())?.as_ref() {
            if token.account == link.account && token.expires > Instant::now() {
                return Ok(token.access.clone());
            }
        }
        let bytes = windows::read_credentials().map_err(|_| "sync.reauth".to_string())?;
        let mut credential: Credentials = serde_json::from_slice(&bytes).map_err(|_| "sync.reauth".to_string())?;
        if credential.account != link.account { return Err("sync.reauth".into()); }
        let mut token = google::refresh(&credential)?;
        if let Some(refresh) = token.refresh_token.take() {
            credential.refresh = refresh;
            windows::write_credentials(&serde_json::to_vec(&credential).map_err(|e| e.to_string())?)?;
        }
        let access = token.access_token.clone();
        self.set_token(&link.account, token)?;
        Ok(access)
    }

    pub fn wake(&self) {
        if let Ok(sender) = self.sender.lock() {
            if let Some(sender) = sender.as_ref() { let _ = sender.send(()); }
        }
    }
}

pub fn initialize(st: &ml_storage::Storage) -> ml_storage::Result<()> {
    let label = std::env::var("COMPUTERNAME").ok()
        .filter(|name| validated_label(name).is_ok()).unwrap_or_else(|| "PC".into());
    st.init_sync_device(&Uuid::new_v4().simple().to_string(), &label)
}

pub fn status(state: &AppState) -> Result<SyncStatus, String> {
    let st = state.storage.lock().map_err(|e| e.to_string())?;
    let link = st.sync_link().map_err(|e| e.to_string())?;
    let progress = *state.sync.progress.lock().map_err(|e| e.to_string())?;
    let sign_in = state.sync.sign_in.lock().map_err(|e| e.to_string())?;
    Ok(SyncStatus {
        device: link.device, peers: st.sync_peers().map_err(|e| e.to_string())?, enabled: link.enabled,
        email: link.email, synced_at: link.synced_at,
        busy: link.enabled && !matches!(progress, None | Some(SyncProgress::Waiting)),
        configured_client: default_client().is_some(),
        error: state.sync.error.lock().map_err(|e| e.to_string())?.clone(),
        progress: if link.enabled { progress } else { None },
        sign_in: sign_in.current.map(|(_, phase)| phase),
        sign_in_error: sign_in.error.clone(),
    })
}

pub fn connect(state: &AppState, label: String, include_history: bool) -> Result<(), String> {
    let client = default_client().ok_or_else(|| "sync.unavailable".to_string())?;
    let attempt = SignInAttempt::start(&state.sync.sign_in)?;
    let result = (|| {
        let _gate = state.sync.gate.lock().map_err(|e| e.to_string())?;
        attempt.check_current()?;
        finish_connect(state, client, validated_label(&label)?, include_history, &attempt)
    })();
    attempt.record_result(&result);
    result
}

fn finish_connect(
    state: &AppState,
    client: &str,
    label: &str,
    include_history: bool,
    attempt: &SignInAttempt<'_>,
) -> Result<(), String> {
    let mut token = google::sign_in(client, attempt).map_err(sign_in_error)?;
    let drive = Drive(&token.access_token);
    let user = match drive.user() {
        Err(error) if error == NETWORK_TIMEOUT => drive.user(),
        result => result,
    }.map_err(sign_in_error)?;
    let account = format!("{:x}", Sha256::digest(format!("{client}:{}", user.permission_id).as_bytes()));
    let refresh = token.refresh_token.take().ok_or_else(|| "sync.reauth".to_string())?;
    let cred = Credentials { client: client.to_string(), refresh, account: account.clone() };
    let previous = windows::read_credentials().ok();
    windows::write_credentials(&serde_json::to_vec(&cred).map_err(|e| e.to_string())?)?;
    let result = state.storage.lock().map_err(|e| e.to_string()).and_then(|st|
        st.enable_sync(&account, user.email_address.as_deref().unwrap_or(""), label, include_history, Utc::now().timestamp())
            .map_err(|e| e.to_string()));
    if let Err(error) = result {
        if let Some(previous) = previous { let _ = windows::write_credentials(&previous); } else { windows::delete_credentials(); }
        return Err(error);
    }
    state.sync.set_token(&account, token)?;
    state.sync.stop_requested.store(false, Ordering::Relaxed);
    state.sync.update_error(None);
    state.sync.wake();
    Ok(())
}

fn sign_in_error(error: String) -> String {
    if error == NETWORK_TIMEOUT { "sync.sign_in_network_timeout".into() } else { error }
}

pub fn disconnect(state: &AppState) -> Result<(), String> {
    state.sync.stop_requested.store(true, Ordering::Relaxed);
    let _gate = state.sync.gate.lock().map_err(|e| e.to_string())?;
    state.storage.lock().map_err(|e| e.to_string())?.disable_sync().map_err(|e| e.to_string())?;
    windows::delete_credentials();
    *state.sync.token.lock().map_err(|e| e.to_string())? = None;
    state.sync.update_error(None);
    state.sync.update_progress(None);
    state.sync.sign_in.lock().map_err(|e| e.to_string())?.error = None;
    Ok(())
}

pub fn rename(state: &AppState, label: &str) -> Result<(), String> {
    let label = validated_label(label)?;
    state.storage.lock().map_err(|e| e.to_string())?.rename_sync_device(label).map_err(|e| e.to_string())?;
    state.sync.wake();
    Ok(())
}

fn sync_once(state: &AppState) -> Result<bool, String> {
    let _gate = match state.sync.gate.try_lock() {
        Ok(lock) => lock,
        Err(_) => return Ok(true),
    };
    let result = sync_batch(state);
    if let Err(error) = &result {
        if error == "Google Drive HTTP 401" {
            if let Ok(mut token) = state.sync.token.lock() { *token = None; }
        }
        state.sync.update_error(Some(error.clone()));
    }
    state.sync.update_progress(if matches!(result, Ok(true)) {
        Some(SyncProgress::Waiting)
    } else {
        None
    });
    result
}

fn sync_batch(state: &AppState) -> Result<bool, String> {
    let with_storage = || state.storage.lock().map_err(|e| e.to_string());
    let link = with_storage()?.sync_link().map_err(|e| e.to_string())?;
    let stopping = || state.sync.stop_requested.load(Ordering::Relaxed);
    if !link.enabled || stopping() { return Ok(false); }
    if matches!(state.sync.error.lock().map_err(|e| e.to_string())?.as_deref(), Some("sync.reauth" | "sync.bad_client")) {
        return Ok(false);
    }
    let progress = |progress| state.sync.update_progress(Some(progress));
    progress(SyncProgress::Preparing);
    let access = state.sync.access_token(&link)?;
    let drive = Drive(&access);
    let files = drive.files()?;
    if stopping() { return Ok(false); }
    let meta_name = filename(&link.device.id, META_DAY);
    let meta = with_storage()?.export_sync_shard(&link, META_DAY).map_err(|e| e.to_string())?;
    if !files.contains_key(&meta_name) {
        with_storage()?.requeue_sync_history(link.since).map_err(|e| e.to_string())?;
    }
    let days = with_storage()?.dirty_sync_days(link.since, UPLOAD_LIMIT).map_err(|e| e.to_string())?;
    let total = days.len() + 1;
    progress(SyncProgress::Uploading { done: 0, total });
    drive.upload(&meta_name, &serialize_shard(&meta)?, files.get(&meta_name))?;
    progress(SyncProgress::Uploading { done: 1, total });
    for (index, &(day, revision)) in days.iter().enumerate() {
        if stopping() { return Ok(false); }
        let shard = with_storage()?.export_sync_shard(&link, day).map_err(|e| e.to_string())?;
        let name = filename(&link.device.id, day);
        let previous = files.get(&name);
        if previous.is_some() || !shard.events.is_empty() || !shard.bookmarks.is_empty() || !shard.rules.is_empty() {
            drive.upload(&name, &serialize_shard(&shard)?, previous)?;
        }
        with_storage()?.acknowledge_sync_day(day, revision).map_err(|e| e.to_string())?;
        progress(SyncProgress::Uploading { done: index + 2, total });
    }
    let cached: HashMap<_, _> = with_storage()?.peer_sync_versions().map_err(|e| e.to_string())?
        .into_iter().map(|(id, day, file, version)| ((id, day), (file, version))).collect();
    let mut live = HashSet::new();
    let mut pending = Vec::new();
    for (name, file) in &files {
        let Some((device, day)) = parse_filename(name) else { continue };
        if device == link.device.id { continue; }
        let key = (device, day);
        if cached.get(&key).is_none_or(|(id, version)| id != &file.id || version != &file.version) {
            pending.push((key.clone(), file));
        }
        live.insert(key);
    }
    pending.sort_by_key(|((_, day), _)| (*day != META_DAY, std::cmp::Reverse(*day)));
    let total = pending.len().min(DOWNLOAD_LIMIT);
    if total > 0 {
        progress(SyncProgress::Downloading { done: 0, total });
    }
    for (index, ((device, day), file)) in pending.iter().take(total).enumerate() {
        if stopping() { return Ok(false); }
        let bytes = drive.download(file)?;
        let shard = deserialize_shard(&bytes, device, *day)?;
        with_storage()?.import_sync_shard(&file.id, &file.version, shard).map_err(|e| e.to_string())?;
        progress(SyncProgress::Downloading { done: index + 1, total });
    }
    if stopping() { return Ok(false); }
    progress(SyncProgress::Finishing);
    with_storage()?.reconcile_sync_peers(&live).map_err(|e| e.to_string())?;
    with_storage()?.finish_sync(Utc::now().timestamp()).map_err(|e| e.to_string())?;
    state.sync.update_error(None);
    Ok(days.len() == UPLOAD_LIMIT as usize || pending.len() > DOWNLOAD_LIMIT)
}

pub fn spawn(state: Arc<AppState>) {
    let (sender, receiver) = mpsc::channel();
    if let Ok(mut slot) = state.sync.sender.lock() { *slot = Some(sender); }
    std::thread::spawn(move || {
        let mut timeout_retry = FIRST_TIMEOUT_RETRY;
        loop {
            let wait = next_sync_delay(sync_once(&state), &mut timeout_retry);
            if matches!(receiver.recv_timeout(wait), Err(mpsc::RecvTimeoutError::Disconnected)) { break; }
            while receiver.try_recv().is_ok() {}
        }
    });
}

fn next_sync_delay(result: Result<bool, String>, timeout_retry: &mut Duration) -> Duration {
    if matches!(&result, Err(error) if error == NETWORK_TIMEOUT) {
        let wait = *timeout_retry;
        *timeout_retry = (*timeout_retry * 2).min(BACKGROUND_SYNC_INTERVAL);
        return wait;
    }
    *timeout_retry = FIRST_TIMEOUT_RETRY;
    match result {
        Ok(true) => FIRST_TIMEOUT_RETRY,
        Ok(false) | Err(_) => BACKGROUND_SYNC_INTERVAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ml_core::ActivityEvent;

    #[test]
    fn network_timeouts_retry_quickly_then_back_off() {
        let mut next = FIRST_TIMEOUT_RETRY;
        for seconds in [5, 10, 20, 40, 60, 60] {
            assert_eq!(next_sync_delay(Err(NETWORK_TIMEOUT.into()), &mut next), Duration::from_secs(seconds));
        }
        assert_eq!(next_sync_delay(Ok(false), &mut next), BACKGROUND_SYNC_INTERVAL);
        assert_eq!(next_sync_delay(Err(NETWORK_TIMEOUT.into()), &mut next), FIRST_TIMEOUT_RETRY);
        assert_eq!(next_sync_delay(Err("sync.reauth".into()), &mut next), BACKGROUND_SYNC_INTERVAL);
    }

    #[test]
    fn archive_round_trip_and_scope_check() {
        let device = Uuid::new_v4().simple().to_string();
        let shard = Shard {
            protocol: 1, device: device.clone(), day: 10, label: None,
            events: vec![ActivityEvent::Idle(10 * DAY_SECONDS)], bookmarks: vec![], rules: vec![],
        };
        assert_eq!(parse_filename(&filename(&device, 10)), Some((device.clone(), 10)));
        assert_eq!(parse_filename(&filename(&device, META_DAY)), Some((device.clone(), META_DAY)));
        let bytes = serialize_shard(&shard).unwrap();
        assert_eq!(deserialize_shard(&bytes, &device, 10).unwrap().events, shard.events);
        assert!(deserialize_shard(&bytes, &device, 11).is_err());
    }
}

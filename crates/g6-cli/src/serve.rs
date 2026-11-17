//! `g6 serve`: a small local HTTP API (Stream Deck, Home Assistant, scripts).
//!
//! Secure by default: it listens on 127.0.0.1 only, every request needs the
//! bearer token kept in the config directory, requests whose `Host` or
//! `Origin` is not loopback are refused (DNS rebinding, browser CSRF), request
//! bodies are capped, and no CORS headers are ever sent. All writes go through
//! the same validation and device calls as the CLI commands.
//!
//! The HTTP/1.1 side is hand-written on `std::net` (the API is a few JSON
//! routes and one event stream), so that every bound is ours: at most
//! [`MAX_IN_FLIGHT`] connections at once (counted at accept, before a byte is
//! read; one request per connection, `Connection: close`), a size limit on
//! the request head and body, one overall deadline for receiving a request
//! (so a client that sends nothing, or one byte at a time, is dropped), and a
//! write deadline on every write, event streams included (a client that stops
//! reading is dropped and frees its slot). See [`Limits`].

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use g6_core::direct::{self, DirectMode};
use g6_core::events::Event;
use g6_core::features::{self, Kind};
use g6_core::preset::{self, PresetStore};
use g6_core::protocol::{rgb_from_hex, MaskTarget, Output};
use g6_core::session::Session;
use g6_core::state::output_from_name;
use g6_core::store::Paths;
use serde_json::{json, Map, Value};

use crate::action::{self, Op};
use crate::format;
use crate::run::{self, CliError, Env, Novelty};

/// Name of the token file in the config directory.
const TOKEN_FILE: &str = "api_token";
/// Largest request body (the biggest real one is a few dozen bytes).
const MAX_BODY: usize = 4096;
/// Most event streams open at once.
const MAX_EVENT_CLIENTS: usize = 8;
/// Most connections open at once (requests being read or answered, and event
/// streams).
const MAX_IN_FLIGHT: usize = 32;
/// Largest request head (request line and headers).
const MAX_HEAD: usize = 8 * 1024;
/// Longest chunk-size or trailer line of a chunked body.
const MAX_LINE: usize = 256;
/// Time to receive a whole request (head and body), counted from the accept.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Time for any one write to the client to complete.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Most refused connections drained at once, each on its own short-lived
/// thread, so the accept thread never waits for a client.
const MAX_REFUSING: usize = 8;
/// How long a refused connection is drained for.
const REFUSE_LINGER: Duration = Duration::from_millis(250);
/// How often an event stream checks that its client has not closed the
/// connection.
const PEER_CHECK: Duration = Duration::from_secs(1);
/// Most unread request bytes thrown away after an answer, so that closing
/// the socket does not reset the connection and lose the answer.
const MAX_DRAIN: usize = 64 * 1024;
/// Idle time after which an event stream gets a comment line (keeps proxies
/// and dead-peer detection honest).
const PING_EVERY: Duration = Duration::from_secs(15);
/// How often background threads look at the stop flag.
const TICK: Duration = Duration::from_millis(100);

/// What one client may hold or send. Tests use short times.
#[derive(Clone, Copy, Debug)]
struct Limits {
    max_connections: usize,
    max_head: usize,
    /// Total time to receive a request, head and body.
    read_timeout: Duration,
    /// Time for one write (an answer, an event, a ping) to complete.
    write_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_connections: MAX_IN_FLIGHT,
            max_head: MAX_HEAD,
            read_timeout: READ_TIMEOUT,
            write_timeout: WRITE_TIMEOUT,
        }
    }
}

// ---------------------------------------------------------------------------
// Request checks (pure, no sockets)
// ---------------------------------------------------------------------------

/// Split `host[:port]` (also `[::1]:port`) into the host and the port text.
/// A malformed authority gives an empty host, which is never loopback.
fn authority_parts(a: &str) -> (&str, Option<&str>) {
    if a.starts_with('[') {
        return match a.find(']') {
            Some(i) => {
                let (host, rest) = a.split_at(i + 1);
                match rest {
                    "" => (host, None),
                    r => r.strip_prefix(':').map_or(("", None), |p| (host, Some(p))),
                }
            }
            None => ("", None),
        };
    }
    match a.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (a, None),
    }
}

fn is_loopback_host(h: &str) -> bool {
    h == "127.0.0.1" || h == "[::1]" || h.eq_ignore_ascii_case("localhost")
}

/// `Host` must be a loopback name with exactly our port.
fn host_ok(host: Option<&str>, port: u16) -> bool {
    let Some(host) = host else { return false };
    let (name, p) = authority_parts(host.trim());
    is_loopback_host(name) && p.and_then(|p| p.parse::<u16>().ok()) == Some(port)
}

/// No `Origin` (not a browser) is fine; a present one must be a loopback page.
fn origin_ok(origin: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false; // includes `null`
    };
    let (name, port) = authority_parts(rest);
    !rest.contains('/') && is_loopback_host(name) && port.is_none_or(|p| p.parse::<u16>().is_ok())
}

/// Constant-time comparison (the length of the token is not a secret).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn auth_ok(header: Option<&str>, token: &str) -> bool {
    let Some((scheme, cred)) = header.and_then(|h| h.trim().split_once(' ')) else {
        return false;
    };
    scheme.eq_ignore_ascii_case("bearer") && ct_eq(cred.trim().as_bytes(), token.as_bytes())
}

/// The error answer for a request.
#[derive(Debug)]
struct ApiError {
    status: u16,
    message: String,
}

fn err(status: u16, message: impl Into<String>) -> ApiError {
    ApiError {
        status,
        message: message.into(),
    }
}

impl From<CliError> for ApiError {
    fn from(e: CliError) -> Self {
        match e {
            CliError::Usage(m) => err(400, m),
            CliError::Failure(m) => err(500, m),
            CliError::BrokenPipe => err(500, "output closed"),
        }
    }
}

/// Host, then Origin, then token: in this order, before anything is routed.
fn gate(
    host: Option<&str>,
    origin: Option<&str>,
    auth: Option<&str>,
    token: &str,
    port: u16,
) -> Result<(), ApiError> {
    if !host_ok(host, port) {
        return Err(err(
            403,
            "Host must be 127.0.0.1, localhost or [::1] with the server's port",
        ));
    }
    if !origin_ok(origin) {
        return Err(err(403, "Origin is not loopback"));
    }
    if !auth_ok(auth, token) {
        return Err(err(401, "missing or wrong bearer token"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Token file
// ---------------------------------------------------------------------------

fn token_error(path: &Path, e: impl fmt::Display) -> CliError {
    CliError::Failure(format!("API token file {}: {e}", path.display()))
}

fn valid_token(t: &str) -> bool {
    t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The API token: read from `path`, or created there (32 random bytes as
/// hex, readable by the owner only). Never printed.
fn load_or_create_token(path: &Path) -> Result<String, CliError> {
    for _ in 0..2 {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let token = text.trim().to_string();
                if !valid_token(&token) {
                    return Err(token_error(
                        path,
                        "does not hold a token; delete the file to create a new one",
                    ));
                }
                check_private(path)?;
                return Ok(token);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(token_error(path, e)),
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|e| token_error(path, format!("no random bytes: {e}")))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| token_error(dir, e))?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        match opts.open(path) {
            Ok(mut f) => {
                f.write_all(token.as_bytes())
                    .and_then(|()| f.sync_all())
                    .map_err(|e| token_error(path, e))?;
                return Ok(token);
            }
            // Another `g6 serve` created it first: read theirs.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(token_error(path, e)),
        }
    }
    Err(token_error(path, "could not be created"))
}

/// A token file other users can read or write is refused: the token may be
/// known to them, so tightening the mode would not help (Unix; on Windows the
/// file inherits the user profile's access rules).
fn check_private(path: &Path) -> Result<(), CliError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|e| token_error(path, e))?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(token_error(
                path,
                "can be read by other users, so the token may be known; \
                 delete the file to create a new one",
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

// ---------------------------------------------------------------------------
// The API
// ---------------------------------------------------------------------------

struct Api {
    session: Session,
    token: String,
    port: u16,
    limits: Limits,
    presets: PresetStore,
    direct: DirectMode,
    /// One write at a time (Direct Mode is a multi-step change).
    write_lock: Mutex<()>,
    /// The output `write` just switched to and already followed with Direct
    /// Mode: the device's echo of it (an `OutputSwitched` event) must not
    /// follow a second time. Cleared by the next output event or reconnect.
    own_output: Mutex<Option<Output>>,
    /// Event streams (id, sender): each gets the JSON of every new event. A
    /// stream removes itself when it ends, so a gone client never counts
    /// towards [`MAX_EVENT_CLIENTS`] while no event is being broadcast. A
    /// slow one loses its sender (`None`) but keeps its place until its
    /// thread ends: that thread may sit in a write for up to
    /// `Limits::write_timeout`, and such threads must stay capped well below
    /// [`MAX_IN_FLIGHT`].
    clients: Mutex<Vec<(u64, Option<SyncSender<String>>)>>,
    next_client: AtomicU64,
    in_flight: AtomicUsize,
    /// Refused connections still being drained (see [`MAX_REFUSING`]).
    refusing: AtomicUsize,
    stop: AtomicBool,
}

/// What to answer.
enum Reply {
    Json(u16, String),
    /// 405, with the method the path does allow.
    NotAllowed(&'static str),
    Events,
}

/// `(path, method)` of every endpoint.
const ROUTES: &[(&str, &str)] = &[
    ("/v1/state", "GET"),
    ("/v1/events", "GET"),
    ("/v1/features", "POST"),
    ("/v1/sbx", "POST"),
    ("/v1/scout", "POST"),
    ("/v1/direct", "POST"),
    ("/v1/output", "POST"),
    ("/v1/light", "POST"),
    ("/v1/preset/apply", "POST"),
];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Api {
    fn route(&self, method: &str, path: &str, query: &str, body: &[u8]) -> Reply {
        let Some(&(_, allowed)) = ROUTES.iter().find(|(p, _)| *p == path) else {
            return json_reply(Err(err(404, format!("no such endpoint '{path}'"))));
        };
        if method != allowed {
            return Reply::NotAllowed(allowed);
        }
        if path == "/v1/events" {
            return Reply::Events;
        }
        json_reply(match path {
            "/v1/state" => self.state(query.split('&').any(|p| p == "refresh=1")),
            "/v1/features" => self.feature(body),
            "/v1/sbx" => self.mask(body, MaskTarget::Sbx),
            "/v1/scout" => self.mask(body, MaskTarget::Scout),
            "/v1/direct" => self.direct_mode(body),
            "/v1/output" => self.output(body),
            "/v1/light" => self.light(body),
            _ => self.preset(body),
        })
    }

    /// The device must be connected and synced for a write.
    fn ready(&self) -> Result<(), ApiError> {
        if self.session.state().connected {
            Ok(())
        } else {
            Err(err(503, "G6 not connected"))
        }
    }

    /// Same JSON as `g6 status --json`. Without a device it is the last known
    /// state, with `"connected": false`. The session's live state (kept in
    /// sync by the device's pushes and by every write here) unless `refresh`
    /// asks for a full re-read (`?refresh=1`, about 70 device reads).
    fn state(&self, refresh: bool) -> Result<String, ApiError> {
        let state = if refresh && self.ready().is_ok() {
            self.session
                .refresh()
                .map_err(|e| err(500, format!("reading state: {e}")))?
        } else {
            self.session.state()
        };
        let direct = self.direct.is_active().unwrap_or(false);
        format::status_json(&state, direct).map_err(|e| err(500, format!("encoding JSON: {e}")))
    }

    /// Playback settings (EQ, SBX, Scout, effects, profiles) are read-only
    /// while Direct Mode is on, as in the app; the caller holds `write_lock`.
    fn check_direct_mode_off(&self) -> Result<(), ApiError> {
        match self.direct.is_active() {
            Ok(false) => Ok(()),
            Ok(true) => Err(err(
                409,
                "Direct Mode is on: EQ, SBX and Scout are read-only",
            )),
            Err(e) => Err(err(500, format!("reading the Direct Mode state: {e}"))),
        }
    }

    /// Run validated writes and answer with what the CLI would print.
    /// `playback`: the writes change playback settings.
    fn write(&self, ops: &[Op], playback: bool) -> Result<String, ApiError> {
        let _one_at_a_time = lock(&self.write_lock);
        if playback {
            self.check_direct_mode_off()?;
        }
        self.ready()?;
        // While Scout is on, the EQ and Smart Volume are Scout's, as in the app.
        let scout_owned = ops
            .iter()
            .any(|op| matches!(op, Op::Feature(f, _) if features::scout_owned(f)));
        if scout_owned && self.session.state().mask.is_some_and(|m| m.scout()) {
            return Err(err(
                409,
                "Scout Mode is on: EQ and Smart Volume are set by the device",
            ));
        }
        let own_output = ops.iter().find_map(|op| match op {
            Op::Output(o) => Some(*o),
            _ => None,
        });
        if own_output.is_some() {
            // Before the write: the echo may arrive while it runs.
            *lock(&self.own_output) = own_output;
        }
        let mut results = Vec::new();
        for op in ops {
            match run::exec_op(self.session.device(), op) {
                Ok(r) => results.push(r),
                Err(e) => {
                    if own_output.is_some() {
                        *lock(&self.own_output) = None;
                    }
                    return Err(e.into());
                }
            }
        }
        if own_output.is_some() {
            self.follow_direct_mode_locked();
        }
        run::sync(&self.session);
        Ok(json!({"ok": true, "result": results}).to_string())
    }

    /// After an output change (`/v1/output`, the headphone jack, a
    /// reconnect), as in the app: Direct Mode on is extended to the new
    /// output; Direct Mode off finishes a restore still pending for it.
    /// `switched`: the output of the event that caused this (`None` for a
    /// reconnect). The echo of our own `/v1/output` write is skipped: `write`
    /// followed it already.
    fn follow_direct_mode(&self, switched: Option<Output>) {
        let _one_at_a_time = lock(&self.write_lock);
        let own = lock(&self.own_output).take();
        if switched.is_some() && own == switched {
            return;
        }
        if self.ready().is_ok() && self.follow_direct_mode_locked() {
            run::sync(&self.session);
        }
    }

    /// [`Api::follow_direct_mode`]; the caller holds `write_lock`. Whether
    /// the device was written to.
    fn follow_direct_mode_locked(&self) -> bool {
        let device = self.session.device();
        match self.direct.is_active() {
            Ok(true) => {
                if let Err(e) = self.direct.enable(device) {
                    log::warn!("Direct Mode could not be extended to the active output: {e}");
                }
                return true;
            }
            Ok(false) => {}
            Err(e) => {
                log::warn!("cannot read the Direct Mode file: {e}");
                return false;
            }
        }
        let pending = match self.direct.pending_outputs() {
            Ok(p) if p.is_empty() => return false,
            Ok(p) => p,
            Err(e) => {
                log::warn!("cannot read the Direct Mode file: {e}");
                return false;
            }
        };
        match device.read_output() {
            Ok(o) if pending.contains(&o) => {
                match self.direct.disable(device) {
                    Ok(()) | Err(direct::Error::NotActive) => {}
                    Err(e) => {
                        log::warn!("Direct Mode settings could not be restored on this output: {e}")
                    }
                }
                true
            }
            Ok(_) => false,
            Err(e) => {
                log::warn!("cannot read the active output: {e}");
                false
            }
        }
    }

    fn feature(&self, body: &[u8]) -> Result<String, ApiError> {
        let m = object(body, &["name", "value"])?;
        let name = text(&m, "name")?;
        let Some(f) = features::by_name(name) else {
            return Err(err(400, format!("unknown feature '{name}'")));
        };
        let op = match (m.get("value"), f.kind) {
            (Some(Value::String(s)), _) => action::choice_op(name, s),
            (Some(Value::Bool(b)), Kind::Bool) => action::feature_op(name, f32::from(*b)),
            (Some(Value::Number(n)), _) => match n.as_f64() {
                Some(v) => action::ui_op(name, name, v as f32),
                None => return Err(err(400, "value is not a finite number")),
            },
            _ => {
                return Err(err(
                    400,
                    "value must be a number, true/false (on/off features) or a choice name",
                ))
            }
        }
        .map_err(|e| err(400, e.0))?;
        self.write(&[op], f.family == features::FAMILY_PLAYBACK)
    }

    fn mask(&self, body: &[u8], target: MaskTarget) -> Result<String, ApiError> {
        let m = object(body, &["on"])?;
        self.write(&[Op::Mask(target, flag(&m, "on")?)], true)
    }

    fn output(&self, body: &[u8]) -> Result<String, ApiError> {
        let m = object(body, &["output"])?;
        let name = text(&m, "output")?;
        match output_from_name(name) {
            Some(o) => self.write(&[Op::Output(o)], false),
            None => Err(err(
                400,
                format!("unknown output '{name}' (headphones or speakers)"),
            )),
        }
    }

    fn light(&self, body: &[u8]) -> Result<String, ApiError> {
        let m = object(body, &["on", "color"])?;
        if m.is_empty() {
            return Err(err(400, "give \"on\" and/or \"color\""));
        }
        let mut ops = Vec::new();
        if m.contains_key("color") {
            let hex = text(&m, "color")?;
            let Some(rgb) = rgb_from_hex(hex) else {
                return Err(err(
                    400,
                    format!("color must be six hex digits RRGGBB (e.g. ff8000), not '{hex}'"),
                ));
            };
            ops.push(Op::LightColor(rgb));
        }
        if m.contains_key("on") {
            ops.push(Op::LightEnabled(flag(&m, "on")?));
        }
        self.write(&ops, false)
    }

    fn direct_mode(&self, body: &[u8]) -> Result<String, ApiError> {
        let m = object(body, &["on"])?;
        let on = flag(&m, "on")?;
        self.ready()?;
        let _one_at_a_time = lock(&self.write_lock);
        let device = self.session.device();
        let r = if on {
            self.direct.enable(device)
        } else {
            self.direct.disable(device)
        };
        r.map_err(|e| match e {
            direct::Error::NotActive => err(409, e.to_string()),
            e => err(500, format!("direct {}: {e}", format::on_off(on))),
        })?;
        run::sync(&self.session);
        Ok(json!({"ok": true, "direct_mode": on}).to_string())
    }

    fn preset(&self, body: &[u8]) -> Result<String, ApiError> {
        let m = object(body, &["name"])?;
        let name = text(&m, "name")?;
        let preset = self.presets.load(name).map_err(|e| match e {
            preset::Error::BadName(_) => err(400, e.to_string()),
            preset::Error::NotFound(_) => err(404, e.to_string()),
            e => err(500, format!("loading preset: {e}")),
        })?;
        let _one_at_a_time = lock(&self.write_lock);
        self.check_direct_mode_off()?;
        self.ready()?;
        let applied = preset::apply(self.session.device(), &preset).map_err(|e| match e {
            preset::Error::NoProfile(_) => err(409, e.to_string()),
            e => err(500, format!("applying preset '{name}': {e}")),
        })?;
        run::sync(&self.session);
        Ok(json!({
            "ok": true,
            "output": g6_core::state::output_name(applied.output),
            "writes": applied.writes,
        })
        .to_string())
    }
}

fn json_reply(r: Result<String, ApiError>) -> Reply {
    match r {
        Ok(body) => Reply::Json(200, body),
        Err(e) => Reply::Json(e.status, json!({"error": e.message}).to_string()),
    }
}

/// The body as a JSON object with only the `allowed` keys.
fn object(body: &[u8], allowed: &[&str]) -> Result<Map<String, Value>, ApiError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| err(400, format!("body is not valid JSON: {e}")))?;
    let Value::Object(m) = value else {
        return Err(err(400, "body must be a JSON object"));
    };
    if let Some(k) = m.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(err(400, format!("unknown field '{k}'")));
    }
    Ok(m)
}

fn text<'a>(m: &'a Map<String, Value>, key: &str) -> Result<&'a str, ApiError> {
    match m.get(key) {
        Some(Value::String(s)) => Ok(s),
        _ => Err(err(400, format!("\"{key}\" must be a string"))),
    }
}

fn flag(m: &Map<String, Value>, key: &str) -> Result<bool, ApiError> {
    match m.get(key) {
        Some(Value::Bool(b)) => Ok(*b),
        _ => Err(err(400, format!("\"{key}\" must be true or false"))),
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// What the API looks at in a request head.
struct Head {
    method: String,
    target: String,
    host: Option<String>,
    origin: Option<String>,
    auth: Option<String>,
    body: BodyKind,
    expect_continue: bool,
}

enum BodyKind {
    None,
    Length(u64),
    Chunked,
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Parse a request head (without the blank line that ends it). Strict: any
/// stray control character, folded header, repeated `Host`, `Origin`,
/// `Authorization`, `Content-Length` or `Transfer-Encoding`, or a request
/// with both a length and a transfer encoding is a 400.
fn parse_head(text: &str) -> Result<Head, ApiError> {
    let mut lines = text.split("\r\n");
    let mut parts = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(err(400, "malformed request line"));
    };
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(err(400, "unsupported HTTP version"));
    }
    if method.is_empty() || !method.bytes().all(is_token_byte) {
        return Err(err(400, "malformed request method"));
    }
    if target.is_empty() || target.bytes().any(|b| b.is_ascii_control()) {
        return Err(err(400, "malformed request target"));
    }
    let (mut host, mut origin, mut auth) = (None, None, None);
    let (mut length, mut encoding, mut expect) = (None, None, None);
    for line in lines {
        if line.starts_with([' ', '\t']) || line.bytes().any(|b| b.is_ascii_control() && b != b'\t')
        {
            return Err(err(400, "malformed header line"));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(err(400, "malformed header line"));
        };
        if name.is_empty() || !name.bytes().all(is_token_byte) {
            return Err(err(400, "malformed header name"));
        }
        let value = value.trim_matches([' ', '\t']);
        let slot = match name.to_ascii_lowercase().as_str() {
            "host" => &mut host,
            "origin" => &mut origin,
            "authorization" => &mut auth,
            "content-length" => &mut length,
            "transfer-encoding" => &mut encoding,
            "expect" => &mut expect,
            _ => continue,
        };
        if slot.replace(value).is_some() {
            return Err(err(400, format!("repeated {name} header")));
        }
    }
    let body = match (length, encoding) {
        (Some(_), Some(_)) => {
            return Err(err(
                400,
                "Content-Length and Transfer-Encoding cannot be combined",
            ))
        }
        (None, Some(te)) if te.eq_ignore_ascii_case("chunked") => BodyKind::Chunked,
        (None, Some(_)) => return Err(err(501, "only chunked Transfer-Encoding is supported")),
        (Some(n), None) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            // Too many digits for a u64 is a body too big, not a bad number.
            BodyKind::Length(n.parse().unwrap_or(u64::MAX))
        }
        (Some(_), None) => return Err(err(400, "bad Content-Length")),
        (None, None) => BodyKind::None,
    };
    Ok(Head {
        method: method.to_string(),
        target: target.to_string(),
        host: host.map(str::to_string),
        origin: origin.map(str::to_string),
        auth: auth.map(str::to_string),
        body,
        expect_continue: expect.is_some_and(|v| v.eq_ignore_ascii_case("100-continue")),
    })
}

/// The request side of a connection. Every read runs against one deadline
/// for the whole request, so a client that trickles bytes cannot keep a
/// connection open.
struct Input<'a> {
    stream: &'a TcpStream,
    /// Bytes read past the end of the head: the start of the body.
    pending: Vec<u8>,
    pos: usize,
    deadline: Instant,
}

impl Read for Input<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.pending.len() {
            let n = buf.len().min(self.pending.len() - self.pos);
            buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

fn read_error(e: io::Error) -> ApiError {
    match e.kind() {
        // WouldBlock is what a socket timeout looks like on Unix.
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            err(408, "request not received in time")
        }
        io::ErrorKind::UnexpectedEof => {
            err(400, "connection closed before the request was complete")
        }
        _ => err(400, format!("reading the request: {e}")),
    }
}

/// Read up to the blank line that ends the request head; what follows it
/// stays in `input` for the body.
fn read_head(input: &mut Input, max_head: usize) -> Result<Head, ApiError> {
    let too_big = || err(431, format!("request head larger than {max_head} bytes"));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let end = loop {
        let n = input.read(&mut chunk).map_err(read_error)?;
        if n == 0 {
            return Err(read_error(io::ErrorKind::UnexpectedEof.into()));
        }
        // The blank line may straddle two reads.
        let from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf[from..].windows(4).position(|w| w == b"\r\n\r\n") {
            break from + i;
        }
        if buf.len() >= max_head {
            return Err(too_big());
        }
    };
    if end + 4 > max_head {
        return Err(too_big());
    }
    input.pending = buf.split_off(end + 4);
    input.pos = 0;
    let text = std::str::from_utf8(&buf[..end])
        .map_err(|_| err(400, "request head is not valid UTF-8"))?;
    parse_head(text)
}

/// One CRLF-terminated line of a chunked body, without the CRLF.
fn read_line(r: &mut impl BufRead) -> Result<String, ApiError> {
    let mut line = Vec::new();
    r.by_ref()
        .take(MAX_LINE as u64)
        .read_until(b'\n', &mut line)
        .map_err(read_error)?;
    if !line.ends_with(b"\r\n") {
        return Err(err(400, "malformed chunked body"));
    }
    line.truncate(line.len() - 2);
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn body_too_big() -> ApiError {
    err(413, format!("request body larger than {MAX_BODY} bytes"))
}

/// A chunked body, decoded, of at most [`MAX_BODY`] bytes.
fn read_chunked(input: &mut Input) -> Result<Vec<u8>, ApiError> {
    let mut r = BufReader::new(input);
    let mut body = Vec::new();
    loop {
        let line = read_line(&mut r)?;
        let size = line.split(';').next().unwrap_or_default().trim();
        if size.is_empty() || size.len() > 8 || !size.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(err(400, "bad chunk size"));
        }
        let size = usize::from_str_radix(size, 16).map_err(|_| err(400, "bad chunk size"))?;
        if size == 0 {
            break;
        }
        if size > MAX_BODY - body.len() {
            return Err(body_too_big());
        }
        let at = body.len();
        body.resize(at + size, 0);
        r.read_exact(&mut body[at..]).map_err(read_error)?;
        if !read_line(&mut r)?.is_empty() {
            return Err(err(400, "malformed chunked body"));
        }
    }
    // Trailers end at a blank line.
    for _ in 0..16 {
        if read_line(&mut r)?.is_empty() {
            return Ok(body);
        }
    }
    Err(err(400, "too many trailer lines"))
}

/// At most [`MAX_BODY`] bytes of the request body.
fn read_body(input: &mut Input, head: &Head, limits: &Limits) -> Result<Vec<u8>, ApiError> {
    let len = match head.body {
        BodyKind::None => return Ok(Vec::new()),
        BodyKind::Length(n) if n > MAX_BODY as u64 => return Err(body_too_big()),
        BodyKind::Length(n) => Some(n as usize),
        BodyKind::Chunked => None,
    };
    if head.expect_continue {
        write_within(
            input.stream,
            b"HTTP/1.1 100 Continue\r\n\r\n",
            limits.write_timeout,
        )
        .map_err(|e| err(400, format!("writing to the client: {e}")))?;
    }
    match len {
        Some(n) => {
            let mut body = vec![0u8; n];
            input.read_exact(&mut body).map_err(read_error)?;
            Ok(body)
        }
        None => read_chunked(input),
    }
}

/// Largest single write in `write_within`.
const WRITE_PIECE: usize = 64 << 10;

/// Write all of `data` within `timeout` in total: a client that stopped
/// reading makes this fail instead of blocking.
fn write_within(stream: &TcpStream, mut data: &[u8], timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut w = stream;
    while !data.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        stream.set_write_timeout(Some(left))?;
        // In bounded pieces: Windows accepts one huge write whole, without ever
        // blocking, so the write timeout would never fire for a stalled client.
        let piece = &data[..data.len().min(WRITE_PIECE)];
        match w.write(piece) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

/// Send a JSON answer. Never any CORS headers. A client that went away or
/// does not read is not our problem.
fn send_json(
    stream: &TcpStream,
    write_timeout: Duration,
    status: u16,
    body: &str,
    allow: Option<&str>,
) {
    let mut out = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nContent-Length: {}\r\nConnection: close\r\n",
        reason(status),
        body.len()
    );
    if status == 401 {
        out.push_str("WWW-Authenticate: Bearer\r\n");
    }
    if let Some(a) = allow {
        out.push_str(&format!("Allow: {a}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    let _ = write_within(stream, out.as_bytes(), write_timeout);
}

/// End a connection after the answer: send FIN, then throw away what the
/// client still sends (bounded), because closing a socket with unread data
/// resets the connection, and the client may then lose the answer.
fn close(stream: &TcpStream, linger: Duration) {
    let _ = stream.shutdown(Shutdown::Write);
    let deadline = Instant::now() + linger;
    let mut room = MAX_DRAIN;
    let mut buf = [0u8; 1024];
    let mut r = stream;
    while room > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => room = room.saturating_sub(n),
        }
    }
}

/// Answer, then end the connection.
fn finish(api: &Api, stream: &TcpStream, status: u16, body: &str, allow: Option<&str>) {
    send_json(stream, api.limits.write_timeout, status, body, allow);
    close(stream, api.limits.read_timeout.min(Duration::from_secs(1)));
}

fn finish_error(api: &Api, stream: &TcpStream, e: ApiError) {
    finish(
        api,
        stream,
        e.status,
        &json!({"error": e.message}).to_string(),
        None,
    );
}

struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Takes an event stream out of [`Api::clients`] when it ends.
struct Registered<'a> {
    api: &'a Api,
    id: u64,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        lock(&self.api.clients).retain(|(id, _)| *id != self.id);
    }
}

/// Serve one connection: one request, then close.
fn handle(api: &Arc<Api>, stream: TcpStream) {
    let mut input = Input {
        stream: &stream,
        pending: Vec::new(),
        pos: 0,
        deadline: Instant::now() + api.limits.read_timeout,
    };
    let head = match read_head(&mut input, api.limits.max_head) {
        Ok(h) => h,
        Err(e) => return finish_error(api, &stream, e),
    };
    let (path, query) = head.target.split_once('?').unwrap_or((&head.target, ""));
    log::debug!("{} {path}", head.method);
    if let Err(e) = gate(
        head.host.as_deref(),
        head.origin.as_deref(),
        head.auth.as_deref(),
        &api.token,
        api.port,
    ) {
        return finish_error(api, &stream, e);
    }
    let body = if head.method == "POST" {
        match read_body(&mut input, &head, &api.limits) {
            Ok(b) => b,
            Err(e) => return finish_error(api, &stream, e),
        }
    } else {
        Vec::new()
    };
    match api.route(&head.method, path, query, &body) {
        Reply::Json(status, body) => finish(api, &stream, status, &body, None),
        Reply::NotAllowed(allow) => finish(
            api,
            &stream,
            405,
            &json!({"error": format!("use {allow} for {path}")}).to_string(),
            Some(allow),
        ),
        Reply::Events => stream_events(api, &stream),
    }
}

/// Whether the client closed its end (or the connection broke). Anything it
/// sends is read and ignored. Takes at most a millisecond.
fn peer_closed(stream: &TcpStream) -> bool {
    if stream
        .set_read_timeout(Some(Duration::from_millis(1)))
        .is_err()
    {
        return true;
    }
    let mut buf = [0u8; 256];
    match (&*stream).read(&mut buf) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => !matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
        ),
    }
}

/// Server-Sent Events: the watch events, one `data:` line of JSON each. Runs
/// on its own thread until the client goes away or stops reading. A client
/// that closed the connection is noticed within [`PEER_CHECK`], event or no
/// event. Every write must complete within `Limits::write_timeout`, but a
/// write only blocks once the socket buffers are full, so a client that
/// stays connected and stops reading is dropped only after enough events
/// (or pings) have been sent to it.
fn stream_events(api: &Arc<Api>, stream: &TcpStream) {
    let (tx, rx) = sync_channel(64);
    let id = api.next_client.fetch_add(1, Ordering::SeqCst);
    {
        let mut clients = lock(&api.clients);
        if clients.len() >= MAX_EVENT_CLIENTS {
            drop(clients);
            return finish_error(api, stream, err(503, "too many event streams"));
        }
        clients.push((id, Some(tx)));
    }
    let _registered = Registered { api, id };
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
                X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n: g6\n\n";
    let send = |s: &str| write_within(stream, s.as_bytes(), api.limits.write_timeout);
    if send(head).is_err() {
        return;
    }
    let mut last = Instant::now();
    let mut checked = Instant::now();
    while !api.stop.load(Ordering::SeqCst) {
        if checked.elapsed() >= PEER_CHECK {
            checked = Instant::now();
            if peer_closed(stream) {
                break;
            }
        }
        let sent = match rx.recv_timeout(TICK) {
            Ok(line) => Some(send(&format!("data: {line}\n\n"))),
            Err(RecvTimeoutError::Timeout) if last.elapsed() >= PING_EVERY => {
                Some(send(": ping\n\n"))
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match sent {
            Some(Ok(())) => last = Instant::now(),
            Some(Err(_)) => break,
            None => {}
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// Feed the event streams from the session's events.
fn broadcast(api: &Api, events: &Receiver<Event>) {
    let mut novelty = Novelty::new();
    while !api.stop.load(Ordering::SeqCst) {
        let e = match events.recv_timeout(TICK) {
            Ok(e) => e,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if novelty.check(&e, &api.session) {
            let line = format::event_json(&e, format::unix_now()).to_string();
            // A full or closed channel is a slow or gone client: drop its
            // sender, which ends the stream once it has caught up.
            for (_, c) in lock(&api.clients).iter_mut() {
                if c.as_ref()
                    .is_some_and(|c| c.try_send(line.clone()).is_err())
                {
                    *c = None;
                }
            }
        }
        match e {
            Event::OutputSwitched(o) => api.follow_direct_mode(Some(o)),
            Event::Connected => api.follow_direct_mode(None),
            _ => {}
        }
    }
}

/// A running API server. Stops when dropped.
pub struct Server {
    api: Arc<Api>,
    addr: SocketAddr,
    accept: Option<JoinHandle<()>>,
    events: Option<JoinHandle<()>>,
}

impl Server {
    /// Listen on `127.0.0.1:port` (0 = any free port), then start the session
    /// with `start`: a port that is taken fails before the device is opened.
    pub fn start(
        port: u16,
        token: String,
        paths: &Paths,
        start: impl FnOnce() -> (Session, Receiver<Event>),
    ) -> Result<Server, CliError> {
        Self::start_with(Limits::default(), port, token, paths, start)
    }

    fn start_with(
        limits: Limits,
        port: u16,
        token: String,
        paths: &Paths,
        start: impl FnOnce() -> (Session, Receiver<Event>),
    ) -> Result<Server, CliError> {
        let cannot =
            |e: io::Error| CliError::Failure(format!("cannot listen on 127.0.0.1:{port}: {e}"));
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(cannot)?;
        let addr = listener.local_addr().map_err(cannot)?;
        let (session, events) = start();
        let api = Arc::new(Api {
            session,
            token,
            port: addr.port(),
            limits,
            presets: PresetStore::from_paths(paths),
            direct: DirectMode::new(paths.direct_file()),
            write_lock: Mutex::new(()),
            own_output: Mutex::new(None),
            refusing: AtomicUsize::new(0),
            clients: Mutex::new(Vec::new()),
            next_client: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        });
        let spawn = |name: &str, f: Box<dyn FnOnce() + Send>| {
            thread::Builder::new()
                .name(name.into())
                .spawn(f)
                .map_err(|e| CliError::Failure(format!("cannot start a thread: {e}")))
        };
        let events = {
            let api = api.clone();
            spawn("g6-api-events", Box::new(move || broadcast(&api, &events)))?
        };
        let accept = {
            let api = api.clone();
            spawn(
                "g6-api-accept",
                Box::new(move || accept_loop(&api, &listener)),
            )?
        };
        Ok(Server {
            api,
            addr,
            accept: Some(accept),
            events: Some(events),
        })
    }

    /// The address it listens on.
    pub fn port(&self) -> u16 {
        self.api.port
    }

    /// Block until the server stops (it only does when dropped elsewhere, so
    /// in practice: until the process is killed).
    pub fn wait(&mut self) {
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.api.stop.store(true, Ordering::SeqCst);
        // The accept thread is blocked in `accept`: a connection wakes it. If
        // even that fails it is left to end on the next connection.
        let woke = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1)).is_ok();
        let accept = if woke { self.accept.take() } else { None };
        for t in [accept, self.events.take()].into_iter().flatten() {
            let _ = t.join();
        }
    }
}

/// One thread per connection, counted from the accept: over
/// `Limits::max_connections` a connection gets a `503` and is closed without
/// a byte of it being read.
fn accept_loop(api: &Arc<Api>, listener: &TcpListener) {
    while !api.stop.load(Ordering::SeqCst) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) => {
                log::warn!("accept failed: {e}");
                thread::sleep(TICK);
                continue;
            }
        };
        if api.stop.load(Ordering::SeqCst) {
            break;
        }
        if api.in_flight.fetch_add(1, Ordering::SeqCst) >= api.limits.max_connections {
            api.in_flight.fetch_sub(1, Ordering::SeqCst);
            let body = json!({"error": "too many requests"}).to_string();
            send_json(&stream, api.limits.write_timeout, 503, &body, None);
            // Drain what the client already sent (so that closing does not
            // reset the connection and lose the 503) on a short-lived,
            // capped thread; over the cap, just close.
            if api.refusing.fetch_add(1, Ordering::SeqCst) < MAX_REFUSING {
                let worker = api.clone();
                let spawned =
                    thread::Builder::new()
                        .name("g6-api-refuse".into())
                        .spawn(move || {
                            close(&stream, REFUSE_LINGER);
                            worker.refusing.fetch_sub(1, Ordering::SeqCst);
                        });
                if spawned.is_err() {
                    api.refusing.fetch_sub(1, Ordering::SeqCst);
                }
            } else {
                api.refusing.fetch_sub(1, Ordering::SeqCst);
                let _ = stream.shutdown(Shutdown::Write);
            }
            continue;
        }
        let api = api.clone();
        let spawned = thread::Builder::new().name("g6-api-request".into()).spawn({
            let api = api.clone();
            move || {
                let _count = InFlight(&api.in_flight);
                handle(&api, stream);
            }
        });
        if spawned.is_err() {
            api.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// `g6 serve`: create the token if needed, listen, and serve until killed.
pub fn run(env: &mut Env, out: &mut dyn io::Write, port: u16) -> Result<(), CliError> {
    let paths = env.paths()?.clone();
    let token_path = paths.config_dir.join(TOKEN_FILE);
    // First: a file problem must not leave the device opened for nothing.
    let token = load_or_create_token(&token_path)?;
    let mut server = Server::start(port, token, &paths, || env.start_session())?;
    run::say(
        out,
        format_args!("g6: API listening on http://127.0.0.1:{}", server.port()),
    )?;
    run::say(
        out,
        format_args!(
            "g6: bearer token in {} (not shown here)",
            token_path.display()
        ),
    )?;
    server.wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use g6_core::events::Config;
    use g6_core::features::by_name;
    use g6_core::protocol::{cmd, Mask, Output};
    use g6_core::session::SessionConfig;
    use g6_core::transport::fake::FakeDevice;
    use g6_core::transport::Backoff;
    use std::net::TcpStream;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "g6-serve-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fast() -> Config {
        Config {
            reply_timeout: Duration::from_millis(300),
            ack_timeout: Duration::from_millis(60),
            poll_interval: Duration::from_millis(5),
            probe_timeout: Duration::from_millis(100),
            backoff: Backoff {
                initial: Duration::from_millis(2),
                max: Duration::from_millis(10),
            },
        }
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    // ---- pure checks ------------------------------------------------------

    #[test]
    fn host_must_be_loopback_with_our_port() {
        for ok in [
            "127.0.0.1:8765",
            "localhost:8765",
            "LocalHost:8765",
            "[::1]:8765",
            " 127.0.0.1:8765 ",
        ] {
            assert!(host_ok(Some(ok), 8765), "{ok:?}");
        }
        for bad in [
            "evil.example:8765",
            "127.0.0.1.evil.example:8765",
            "localhost.evil.example:8765",
            "evil.example",
            "127.0.0.1",        // no port
            "127.0.0.1:80",     // wrong port
            "127.0.0.1:",       // empty port
            "127.0.0.1:8765:1", // not an authority
            "0.0.0.0:8765",
            "192.168.1.5:8765",
            "[::1:8765",
            "[::1]8765",
            "[::2]:8765",
            "",
        ] {
            assert!(!host_ok(Some(bad), 8765), "{bad:?}");
        }
        assert!(!host_ok(None, 8765));
    }

    #[test]
    fn origin_must_be_absent_or_loopback() {
        assert!(origin_ok(None));
        for ok in [
            "http://127.0.0.1:8765",
            "http://localhost:3000",
            "https://localhost",
            "http://[::1]:8765",
        ] {
            assert!(origin_ok(Some(ok)), "{ok:?}");
        }
        for bad in [
            "null",
            "",
            "http://evil.example",
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example:8765",
            "https://evil.example:8765",
            "file://localhost",
            "http://localhost/path",
            "http://localhost:abc",
        ] {
            assert!(!origin_ok(Some(bad)), "{bad:?}");
        }
    }

    #[test]
    fn bearer_token_is_checked_exactly() {
        assert!(auth_ok(Some(&format!("Bearer {TOKEN}")), TOKEN));
        assert!(auth_ok(Some(&format!("bearer {TOKEN}")), TOKEN));
        assert!(!auth_ok(None, TOKEN));
        assert!(!auth_ok(Some(TOKEN), TOKEN));
        assert!(!auth_ok(Some("Bearer "), TOKEN));
        assert!(!auth_ok(Some("Bearer wrong"), TOKEN));
        assert!(!auth_ok(Some(&format!("Basic {TOKEN}")), TOKEN));
        assert!(!auth_ok(Some(&format!("Bearer {}", &TOKEN[1..])), TOKEN));
        assert!(!auth_ok(Some(&format!("Bearer {TOKEN}0")), TOKEN));
        let mut flipped = TOKEN.to_string();
        flipped.replace_range(63..64, "e");
        assert!(!auth_ok(Some(&format!("Bearer {flipped}")), TOKEN));
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn gate_checks_host_then_origin_then_token() {
        let good = Some("127.0.0.1:9");
        let auth = format!("Bearer {TOKEN}");
        let g = |h, o, a: Option<&str>| gate(h, o, a, TOKEN, 9).map_err(|e| e.status);
        assert_eq!(g(good, None, Some(&auth)), Ok(()));
        assert_eq!(g(good, Some("http://localhost:1"), Some(&auth)), Ok(()));
        // Even with the right token, a rebinding Host or foreign Origin fails.
        assert_eq!(g(Some("evil.example:9"), None, Some(&auth)), Err(403));
        assert_eq!(g(None, None, Some(&auth)), Err(403));
        assert_eq!(g(good, Some("http://evil.example"), Some(&auth)), Err(403));
        // Host is judged before the token.
        assert_eq!(g(Some("evil.example:9"), None, None), Err(403));
        assert_eq!(g(good, None, None), Err(401));
        assert_eq!(g(good, None, Some("Bearer nope")), Err(401));
    }

    // ---- token file -------------------------------------------------------

    #[test]
    fn token_is_created_once_and_kept_private() {
        let tmp = TempDir::new();
        let path = tmp.0.join("config").join(TOKEN_FILE);
        let token = load_or_create_token(&path).unwrap();
        assert!(valid_token(&token), "{token}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), token);
        assert_eq!(load_or_create_token(&path).unwrap(), token);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            // A looser file is refused, not quietly tightened: the token
            // may already be known. Deleting it gives a new one.
            for loose in [0o644, 0o640, 0o602] {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(loose)).unwrap();
                let e = load_or_create_token(&path).unwrap_err();
                assert!(
                    matches!(&e, CliError::Failure(m) if m.contains("other users") && m.contains("delete")),
                    "{loose:o}: {e}"
                );
                assert!(!e.to_string().contains(&token));
                assert_eq!(mode(&path), loose, "left as it is");
                assert_eq!(std::fs::read_to_string(&path).unwrap(), token);
            }
            std::fs::remove_file(&path).unwrap();
            let fresh = load_or_create_token(&path).unwrap();
            assert_ne!(fresh, token);
            assert_eq!(mode(&path), 0o600);
        }
        // Two files, two different tokens.
        let other = load_or_create_token(&tmp.0.join("other")).unwrap();
        assert_ne!(other, token);
    }

    #[test]
    fn a_file_that_is_not_a_token_is_refused_not_replaced() {
        let tmp = TempDir::new();
        let path = tmp.0.join(TOKEN_FILE);
        for junk in ["", "short", &"z".repeat(64), &format!("{TOKEN}00")] {
            std::fs::write(&path, junk).unwrap();
            let e = load_or_create_token(&path).unwrap_err();
            assert!(
                matches!(&e, CliError::Failure(m) if m.contains("does not hold a token")),
                "{junk:?}: {e}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), junk);
        }
    }

    // ---- over HTTP, with the fake device -----------------------------------

    struct Resp {
        status: u16,
        head: String,
        body: String,
    }

    impl Resp {
        fn json(&self) -> Value {
            serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
        }
        fn error(&self) -> String {
            self.json()["error"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        }
    }

    struct Rig {
        fake: FakeDevice,
        paths: Paths,
        server: Server,
        token: String,
        _tmp: TempDir,
    }

    impl Rig {
        fn new() -> Rig {
            Rig::with_limits(Limits::default())
        }

        fn with_limits(limits: Limits) -> Rig {
            let tmp = TempDir::new();
            let paths = Paths::under(&tmp.0);
            let fake = FakeDevice::new();
            let token = load_or_create_token(&paths.config_dir.join(TOKEN_FILE)).unwrap();
            let (f, p) = (fake.clone(), paths.clone());
            let server = Server::start_with(limits, 0, token.clone(), &paths, move || {
                Session::start(
                    Box::new(f.connector()),
                    fast(),
                    SessionConfig {
                        paths: Some(p),
                        reapply_on_first_connect: false,
                    },
                )
            })
            .unwrap();
            assert!(server.api.session.wait_connected(Duration::from_secs(3)));
            Rig {
                fake,
                paths,
                server,
                token,
                _tmp: tmp,
            }
        }

        fn raw(&self, request: &str) -> Resp {
            let mut s = TcpStream::connect(("127.0.0.1", self.server.port())).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            s.write_all(request.as_bytes()).unwrap();
            read_response(&mut s)
        }

        /// A request with exactly these headers (plus Content-Length).
        fn with(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Resp {
            let mut req = format!("{method} {path} HTTP/1.0\r\n");
            for (k, v) in headers {
                req.push_str(&format!("{k}: {v}\r\n"));
            }
            req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
            self.raw(&req)
        }

        /// A request as a good client sends it.
        fn call(&self, method: &str, path: &str, body: &str) -> Resp {
            let host = format!("127.0.0.1:{}", self.server.port());
            let auth = format!("Bearer {}", self.token);
            self.with(
                method,
                path,
                &[("Host", &host), ("Authorization", &auth)],
                body,
            )
        }

        fn get(&self, path: &str) -> Resp {
            self.call("GET", path, "")
        }

        fn post(&self, path: &str, body: &str) -> Resp {
            self.call("POST", path, body)
        }

        fn feature(&self, name: &str) -> f32 {
            let f = by_name(name).unwrap();
            self.fake.feature(f.family, f.id).unwrap()
        }

        /// An open event stream, after its first bytes.
        fn events(&self) -> TcpStream {
            let mut s = TcpStream::connect(("127.0.0.1", self.server.port())).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let req = format!(
                "GET /v1/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\n\
                 Accept: text/event-stream\r\nConnection: close\r\n\r\n",
                self.server.port(),
                self.token
            );
            s.write_all(req.as_bytes()).unwrap();
            let head = read_until(&mut s, ": g6");
            assert!(head.starts_with("HTTP/1.1 200"), "{head}");
            assert!(head.contains("text/event-stream"), "{head}");
            s
        }
    }

    /// The answer on `s`, read until the server closes it.
    fn read_response(s: &mut TcpStream) -> Resp {
        let mut bytes = Vec::new();
        let _ = s.read_to_end(&mut bytes);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        Resp {
            status: head
                .split(' ')
                .nth(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| panic!("no status in {text:?}")),
            head: head.to_string(),
            body: body.to_string(),
        }
    }

    /// Read from `s` until the text so far contains `needle`.
    fn read_until(s: &mut TcpStream, needle: &str) -> String {
        let mut got = String::new();
        let mut buf = [0u8; 1024];
        while !got.contains(needle) {
            let n = s
                .read(&mut buf)
                .unwrap_or_else(|e| panic!("waiting for {needle:?}: {e}; got {got:?}"));
            assert!(n > 0, "stream ended waiting for {needle:?}; got {got:?}");
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        got
    }

    /// Read an event stream until an event containing `needle` is complete
    /// (ends in a blank line), and stop exactly there: the text is whole
    /// events, so every `data:` line in it is complete JSON, and nothing of
    /// the next event is consumed.
    fn read_events_until(s: &mut TcpStream, needle: &str) -> String {
        let mut got = String::new();
        let mut event_start = 0;
        let mut byte = [0u8; 1];
        let mut raw = Vec::new();
        loop {
            let n = s
                .read(&mut byte)
                .unwrap_or_else(|e| panic!("waiting for {needle:?}: {e}; got {got:?}"));
            assert!(n > 0, "stream ended waiting for {needle:?}; got {got:?}");
            raw.push(byte[0]);
            if raw.ends_with(b"\n\n") {
                got.push_str(&String::from_utf8_lossy(&raw));
                raw.clear();
                if got[event_start..].contains(needle) {
                    return got;
                }
                event_start = got.len();
            }
        }
    }

    #[test]
    fn requests_without_the_token_or_with_a_foreign_host_are_refused() {
        let rig = Rig::new();
        let host = format!("127.0.0.1:{}", rig.server.port());
        let auth = format!("Bearer {}", rig.token);

        let r = rig.with("GET", "/v1/state", &[("Host", &host)], "");
        assert_eq!(r.status, 401);
        assert!(r.head.contains("WWW-Authenticate: Bearer"), "{}", r.head);
        let r = rig.with(
            "GET",
            "/v1/state",
            &[("Host", &host), ("Authorization", "Bearer wrong")],
            "",
        );
        assert_eq!(r.status, 401);
        assert!(!r.body.contains(&rig.token) && !r.head.contains(&rig.token));

        // DNS rebinding: right token, but the name is the attacker's.
        for bad_host in [
            "evil.example",
            "evil.example:80",
            "localhost",
            "127.0.0.1:1",
        ] {
            let r = rig.with(
                "GET",
                "/v1/state",
                &[("Host", bad_host), ("Authorization", &auth)],
                "",
            );
            assert_eq!(r.status, 403, "Host {bad_host}");
        }
        assert_eq!(
            rig.with("GET", "/v1/state", &[("Authorization", &auth)], "")
                .status,
            403
        );
        // Browser CSRF: a page on another origin.
        for origin in ["http://evil.example", "null"] {
            let r = rig.with(
                "POST",
                "/v1/sbx",
                &[
                    ("Host", &host),
                    ("Authorization", &auth),
                    ("Origin", origin),
                ],
                r#"{"on":true}"#,
            );
            assert_eq!(r.status, 403, "Origin {origin}");
        }
        assert!(
            !Mask(rig.fake.mask()).sbx(),
            "a refused write must not reach the device"
        );

        // Nothing is routed before the checks: unknown paths look the same.
        assert_eq!(rig.with("GET", "/nope", &[("Host", &host)], "").status, 401);

        // A loopback page is still not allowed without the token, and with
        // it there is never an Access-Control header.
        let r = rig.with(
            "GET",
            "/v1/state",
            &[
                ("Host", &host),
                ("Authorization", &auth),
                ("Origin", "http://localhost:3000"),
            ],
            "",
        );
        assert_eq!(r.status, 200);
        assert!(
            !r.head.to_ascii_lowercase().contains("access-control"),
            "{}",
            r.head
        );
    }

    #[test]
    fn routing_errors() {
        let rig = Rig::new();
        let r = rig.get("/v1/nope");
        assert_eq!(r.status, 404);
        assert!(r.error().contains("/v1/nope"));
        let r = rig.get("/v1/features");
        assert_eq!(r.status, 405);
        assert!(r.head.contains("Allow: POST"), "{}", r.head);
        let r = rig.post("/v1/state", "{}");
        assert_eq!(r.status, 405);
        assert!(r.head.contains("Allow: GET"), "{}", r.head);
        // No CORS preflight support: OPTIONS is just not allowed.
        let r = rig.call("OPTIONS", "/v1/features", "");
        assert_eq!(r.status, 405);
        assert!(
            !r.head.to_ascii_lowercase().contains("access-control"),
            "{}",
            r.head
        );
        // A query string does not change the route.
        assert_eq!(rig.get("/v1/state?x=1").status, 200);
    }

    #[test]
    fn state_is_the_status_json() {
        let rig = Rig::new();
        rig.post("/v1/features", r#"{"name":"bass_level","value":0.3}"#);
        let r = rig.get("/v1/state");
        assert_eq!(r.status, 200);
        assert!(r.head.contains("application/json"), "{}", r.head);
        let v = r.json();
        assert_eq!(v["connected"], true);
        assert_eq!(v["output"], "headphones");
        assert_eq!(v["direct_mode"], false);
        assert_eq!(v["dac_filter"], "fast-min");
        assert_eq!(v["headphones"]["bass_level"], 0.3);
        let cli: Value = serde_json::from_str(
            &format::status_json(&rig.server.api.session.refresh().unwrap(), false).unwrap(),
        )
        .unwrap();
        assert_eq!(v, cli);
    }

    #[test]
    fn state_is_served_live_and_read_from_the_device_only_when_asked() {
        let rig = Rig::new();
        let reads = |rig: &Rig| {
            rig.fake
                .written()
                .iter()
                .filter(|f| f[1] == cmd::FEATURE_READ)
                .count()
        };
        let before = reads(&rig);
        for _ in 0..3 {
            assert_eq!(rig.get("/v1/state").status, 200);
        }
        assert_eq!(
            reads(&rig),
            before,
            "GET /v1/state must not read the device"
        );

        // Pushes (the SBX button) are in the live state without a re-read.
        rig.fake.press_mask(0x01);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !rig.server.api.session.state().mask.is_some_and(|m| m.sbx()) {
            assert!(Instant::now() < deadline, "push not applied");
            thread::sleep(Duration::from_millis(10));
        }
        let live = rig.server.api.session.state();
        assert_eq!(
            rig.get("/v1/state").json(),
            serde_json::from_str::<Value>(&format::status_json(&live, false).unwrap()).unwrap()
        );
        assert_eq!(reads(&rig), before);

        // A change the device did not push is only seen on request.
        let bass = by_name("bass_level").unwrap();
        rig.fake.set_feature(bass.family, bass.id, 0.4);
        assert_eq!(rig.get("/v1/state").json()["headphones"]["bass_level"], 0.0);
        let r = rig.get("/v1/state?refresh=1");
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.json()["headphones"]["bass_level"], 0.4);
        assert!(reads(&rig) > before, "?refresh=1 must read the device");
        assert_eq!(rig.get("/v1/state").json()["headphones"]["bass_level"], 0.4);
    }

    #[test]
    fn playback_writes_are_refused_while_direct_mode_is_on() {
        let rig = Rig::new();
        let state = rig.server.api.session.refresh().unwrap();
        PresetStore::from_paths(&rig.paths)
            .save_current("movies", &state)
            .unwrap();
        assert_eq!(rig.post("/v1/direct", r#"{"on":true}"#).status, 200);
        let writes = rig.fake.written().len();
        let mask = rig.fake.mask();
        for (path, body) in [
            ("/v1/sbx", r#"{"on":true}"#),
            ("/v1/scout", r#"{"on":true}"#),
            ("/v1/features", r#"{"name":"eq_1k","value":1}"#),
            ("/v1/features", r#"{"name":"bass","value":true}"#),
            ("/v1/features", r#"{"name":"eq_preamp","value":1}"#),
        ] {
            let r = rig.post(path, body);
            assert_eq!(r.status, 409, "{path} {body}: {}", r.body);
            assert_eq!(
                r.error(),
                "Direct Mode is on: EQ, SBX and Scout are read-only"
            );
        }
        let r = rig.post("/v1/preset/apply", r#"{"name":"movies"}"#);
        assert_eq!(r.status, 409, "{}", r.body);
        assert!(r.error().contains("Direct Mode is on"), "{}", r.body);
        assert_eq!(
            rig.fake.written().len(),
            writes,
            "nothing reached the device"
        );
        assert_eq!(rig.fake.mask(), mask);

        // Mic, decoder, output, light and Direct Mode itself stay available.
        for (path, body) in [
            ("/v1/features", r#"{"name":"mic_nr_level","value":0.5}"#),
            ("/v1/features", r#"{"name":"decoder_mode","value":"night"}"#),
            ("/v1/output", r#"{"output":"speakers"}"#),
            ("/v1/light", r#"{"on":false}"#),
        ] {
            let r = rig.post(path, body);
            assert_eq!(r.status, 200, "{path} {body}: {}", r.body);
        }
        // Bad input is still a 400.
        assert_eq!(rig.post("/v1/sbx", "{}").status, 400);
        assert_eq!(rig.post("/v1/direct", r#"{"on":false}"#).status, 200);
        let r = rig.post("/v1/sbx", r#"{"on":true}"#);
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(Mask(rig.fake.mask()).sbx());
    }

    #[test]
    fn feature_writes_go_through_the_allow_list() {
        let rig = Rig::new();
        let r = rig.post("/v1/features", r#"{"name":"eq_1k","value":-1.5}"#);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.json(), json!({"ok": true, "result": ["eq_1k: -1.5 dB"]}));
        assert_eq!(rig.feature("eq_1k"), -1.5);
        // UI scale: mic NR 0..1 is device 0..0.5.
        assert_eq!(
            rig.post("/v1/features", r#"{"name":"mic_nr_level","value":1}"#)
                .status,
            200
        );
        assert_eq!(rig.feature("mic_nr_level"), 0.5);
        // On/off as a boolean or 1/0; a choice by name.
        assert_eq!(
            rig.post("/v1/features", r#"{"name":"bass","value":true}"#)
                .status,
            200
        );
        assert_eq!(rig.feature("bass"), 1.0);
        assert_eq!(
            rig.post("/v1/features", r#"{"name":"bass","value":0}"#)
                .status,
            200
        );
        assert_eq!(rig.feature("bass"), 0.0);
        assert_eq!(
            rig.post("/v1/features", r#"{"name":"decoder_mode","value":"night"}"#)
                .status,
            200
        );
        assert_eq!(rig.feature("decoder_mode"), 3.0);
        let status = rig.get("/v1/state").json();
        assert_eq!(status["headphones"]["eq_1k"], -1.5);

        let writes = rig.fake.written().len();
        for (body, needle) in [
            (
                r#"{"name":"eq_1k","value":13}"#,
                "outside the range -12..12",
            ),
            (r#"{"name":"eq_1k","value":0.3}"#, "not a multiple of 0.5"),
            (
                r#"{"name":"eq_preamp","value":7}"#,
                "outside the range -6..6",
            ),
            (
                r#"{"name":"surround_distance","value":5}"#,
                "outside the range 10..300",
            ),
            (
                r#"{"name":"mic_nr_level","value":1.5}"#,
                "outside the range 0..1",
            ),
            (r#"{"name":"nope","value":1}"#, "unknown feature 'nope'"),
            (r#"{"name":"firmware","value":1}"#, "unknown feature"),
            (r#"{"name":"decoder_mode","value":"loud"}"#, "unknown value"),
            (r#"{"name":"bass","value":"on"}"#, "unknown value"),
            (r#"{"name":"eq_1k","value":true}"#, "value must be"),
            (r#"{"name":"eq_1k","value":null}"#, "value must be"),
            (r#"{"name":"eq_1k"}"#, "value must be"),
            (r#"{"value":1}"#, "\"name\" must be a string"),
            (
                r#"{"name":"eq_1k","value":1,"extra":1}"#,
                "unknown field 'extra'",
            ),
            (r#"{"name":"eq_1k","value":1e999}"#, "not valid JSON"),
            ("[1]", "must be a JSON object"),
            ("nope", "not valid JSON"),
            ("", "not valid JSON"),
        ] {
            let r = rig.post("/v1/features", body);
            assert_eq!(r.status, 400, "{body}: {}", r.body);
            assert!(r.error().contains(needle), "{body}: {}", r.body);
        }
        assert_eq!(
            rig.fake.written().len(),
            writes,
            "rejected input must not reach the device"
        );
        assert_eq!(rig.feature("eq_1k"), -1.5);
    }

    #[test]
    fn sbx_scout_output_and_light() {
        let rig = Rig::new();
        let r = rig.post("/v1/sbx", r#"{"on":true}"#);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.json(), json!({"ok": true, "result": ["sbx: on"]}));
        assert!(Mask(rig.fake.mask()).sbx());
        assert_eq!(rig.post("/v1/sbx", r#"{"on":false}"#).status, 200);
        assert!(!Mask(rig.fake.mask()).sbx());
        assert_eq!(rig.post("/v1/scout", r#"{"on":true}"#).status, 200);
        assert!(Mask(rig.fake.mask()).scout());
        assert_eq!(rig.post("/v1/sbx", r#"{"on":"yes"}"#).status, 400);
        assert_eq!(rig.post("/v1/sbx", "{}").status, 400);

        let r = rig.post("/v1/output", r#"{"output":"speakers"}"#);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(rig.fake.output(), Output::Speakers);
        assert_eq!(rig.get("/v1/state").json()["output"], "speakers");
        let r = rig.post("/v1/output", r#"{"output":"tv"}"#);
        assert_eq!(r.status, 400);
        assert!(r.error().contains("unknown output 'tv'"));

        let r = rig.post("/v1/light", r##"{"on":false,"color":"#00FF00"}"##);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(
            r.json(),
            json!({"ok": true, "result": ["light color: 00ff00", "light: off"]})
        );
        assert!(!rig.fake.lighting_enabled());
        assert_eq!(rig.fake.lighting_color(), [0, 255, 0]);
        assert_eq!(rig.post("/v1/light", r#"{"on":true}"#).status, 200);
        assert!(rig.fake.lighting_enabled());
        for body in [
            r#"{}"#,
            r#"{"color":"red"}"#,
            r#"{"color":5}"#,
            r#"{"on":1}"#,
        ] {
            assert_eq!(rig.post("/v1/light", body).status, 400, "{body}");
        }
    }

    #[test]
    fn direct_mode_on_and_off() {
        let rig = Rig::new();
        rig.post("/v1/sbx", r#"{"on":true}"#);
        let r = rig.post("/v1/direct", r#"{"on":true}"#);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.json(), json!({"ok": true, "direct_mode": true}));
        assert!(rig.fake.hw_direct());
        assert!(!Mask(rig.fake.mask()).sbx());
        assert_eq!(rig.get("/v1/state").json()["direct_mode"], true);
        assert_eq!(rig.post("/v1/direct", r#"{"on":false}"#).status, 200);
        assert!(!rig.fake.hw_direct());
        assert!(Mask(rig.fake.mask()).sbx());
        // Off when it is not on is a conflict, not a crash.
        assert_eq!(rig.post("/v1/direct", r#"{"on":false}"#).status, 409);
        assert_eq!(rig.post("/v1/direct", r#"{"on":3}"#).status, 400);
    }

    #[test]
    fn preset_apply() {
        let rig = Rig::new();
        rig.post("/v1/features", r#"{"name":"eq_62","value":2}"#);
        let state = rig.server.api.session.refresh().unwrap();
        PresetStore::from_paths(&rig.paths)
            .save_current("movies", &state)
            .unwrap();
        rig.post("/v1/features", r#"{"name":"eq_62","value":-4}"#);
        let r = rig.post("/v1/preset/apply", r#"{"name":"movies"}"#);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.json()["output"], "headphones");
        assert_eq!(rig.feature("eq_62"), 2.0);
        let r = rig.post("/v1/preset/apply", r#"{"name":"nope"}"#);
        assert_eq!(r.status, 404);
        assert!(r.error().contains("not found"));
        let r = rig.post("/v1/preset/apply", r#"{"name":"../x"}"#);
        assert_eq!(r.status, 400);
        assert!(r.error().contains("invalid preset name"));
        assert_eq!(rig.post("/v1/preset/apply", "{}").status, 400);
    }

    #[test]
    fn oversized_bodies_are_refused() {
        let rig = Rig::new();
        let big = format!(
            r#"{{"name":"eq_1k","value":1,"pad":"{}"}}"#,
            "x".repeat(MAX_BODY)
        );
        let r = rig.post("/v1/features", &big);
        assert_eq!(r.status, 413, "{}", r.body);
        assert!(r.error().contains("larger than"));
        // Exactly at the cap still gets parsed (and refused for its content).
        let pad = MAX_BODY - r#"{"name":"eq_1k","value":1,"pad":""}"#.len();
        let at_cap = format!(
            r#"{{"name":"eq_1k","value":1,"pad":"{}"}}"#,
            "x".repeat(pad)
        );
        assert_eq!(at_cap.len(), MAX_BODY);
        assert_eq!(rig.post("/v1/features", &at_cap).status, 400);
        // A body without a Content-Length is cut off at the cap too.
        let host = format!("127.0.0.1:{}", rig.server.port());
        let r = rig.raw(&format!(
            "POST /v1/features HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\n\
             Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            rig.token,
            MAX_BODY + 10,
            "x".repeat(MAX_BODY + 10)
        ));
        assert_eq!(r.status, 413, "{}", r.body);
    }

    #[test]
    fn without_a_device_state_is_last_known_and_writes_fail() {
        let rig = Rig::new();
        rig.fake.unplug();
        let deadline = Instant::now() + Duration::from_secs(3);
        while rig.server.api.session.state().connected {
            assert!(Instant::now() < deadline, "unplug not noticed");
            thread::sleep(Duration::from_millis(10));
        }
        let r = rig.get("/v1/state");
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.json()["connected"], false);
        for (path, body) in [
            ("/v1/features", r#"{"name":"bass","value":true}"#),
            ("/v1/sbx", r#"{"on":true}"#),
            ("/v1/direct", r#"{"on":true}"#),
            ("/v1/output", r#"{"output":"speakers"}"#),
            ("/v1/light", r#"{"on":true}"#),
        ] {
            let r = rig.post(path, body);
            assert_eq!(r.status, 503, "{path}: {}", r.body);
            assert!(r.error().contains("not connected"));
        }
        // Bad input is still a 400, not a 503: it never needed the device.
        assert_eq!(
            rig.post("/v1/features", r#"{"name":"nope","value":1}"#)
                .status,
            400
        );
    }

    #[test]
    fn events_stream_the_watch_events_as_sse() {
        let rig = Rig::new();
        let mut a = rig.events();
        let mut b = rig.events();
        rig.fake.press_mask(0x01);
        rig.fake.switch_output(Output::Speakers);
        for s in [&mut a, &mut b] {
            let got = read_events_until(s, "\"event\":\"output\"");
            assert!(got.contains("data: {"), "{got}");
            assert!(got.contains("\"event\":\"mask\""), "{got}");
            assert!(got.contains("\"sbx\":true"), "{got}");
            assert!(got.contains("\"output\":\"speakers\""), "{got}");
            // One JSON object per `data:` line.
            for line in got.lines().filter(|l| l.starts_with("data: ")) {
                let v: Value = serde_json::from_str(&line["data: ".len()..]).unwrap();
                assert!(v["ts"].is_number() && v["event"].is_string(), "{line}");
            }
        }
        // A write through the API shows up on the stream too.
        rig.post("/v1/light", r#"{"on":false}"#);
        let got = read_events_until(&mut a, "\"event\":\"light\"");
        assert!(got.contains("\"on\":false"), "{got}");
    }

    #[test]
    fn event_streams_need_auth_and_are_limited() {
        let rig = Rig::new();
        let host = format!("127.0.0.1:{}", rig.server.port());
        assert_eq!(
            rig.with("GET", "/v1/events", &[("Host", &host)], "").status,
            401
        );
        let open: Vec<TcpStream> = (0..MAX_EVENT_CLIENTS).map(|_| rig.events()).collect();
        let r = rig.get("/v1/events");
        assert_eq!(r.status, 503, "{}", r.body);
        assert!(r.error().contains("too many event streams"));
        // A stream that is closed frees its place.
        drop(open);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // The broadcaster notices closed streams when it sends to them.
            rig.fake.press_mask(0x00);
            rig.fake.press_mask(0x01);
            if rig.get("/v1/events").status != 503 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "closed streams were not released"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_stalled_event_stream_keeps_its_place_until_its_thread_ends() {
        let rig = Rig::new();
        // A client whose channel is full (it stopped reading): capacity 0
        // and nobody receiving.
        let (tx, _rx) = sync_channel(0);
        lock(&rig.server.api.clients).push((u64::MAX, Some(tx)));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            rig.fake.press_mask(0x00);
            rig.fake.press_mask(0x01);
            if lock(&rig.server.api.clients)
                .iter()
                .any(|(id, c)| *id == u64::MAX && c.is_none())
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the full channel was not dropped"
            );
            thread::sleep(Duration::from_millis(20));
        }
        // Its sender is gone, but it still counts until its thread ends: that
        // thread may be in a write (for up to the write timeout), and only
        // MAX_EVENT_CLIENTS of them may exist.
        for i in 1..MAX_EVENT_CLIENTS - 1 {
            lock(&rig.server.api.clients).push((u64::MAX - i as u64, None));
        }
        let _last = rig.events();
        let r = rig.get("/v1/events");
        assert_eq!(r.status, 503, "{}", r.body);
        // Other requests are still served.
        assert_eq!(rig.get("/v1/state").status, 200);
    }

    /// Limits with short times, for tests that wait for them.
    fn short(read_ms: u64, write_ms: u64) -> Limits {
        Limits {
            read_timeout: Duration::from_millis(read_ms),
            write_timeout: Duration::from_millis(write_ms),
            ..Limits::default()
        }
    }

    fn connect(rig: &Rig) -> TcpStream {
        let s = TcpStream::connect(("127.0.0.1", rig.server.port())).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_client_that_sends_nothing_is_dropped_after_the_read_timeout() {
        let rig = Rig::with_limits(short(300, 300));
        let mut s = connect(&rig);
        let t = Instant::now();
        let r = read_response(&mut s);
        assert_eq!(r.status, 408, "{}", r.body);
        assert!(r.error().contains("not received in time"), "{}", r.body);
        let waited = t.elapsed();
        assert!(waited >= Duration::from_millis(200), "{waited:?}");
        assert!(waited < Duration::from_secs(3), "{waited:?}");
        wait_for("the slot was not released", || {
            rig.server.api.in_flight.load(Ordering::SeqCst) == 0
        });
        assert_eq!(rig.get("/v1/state").status, 200);
    }

    #[test]
    fn a_client_that_trickles_or_stalls_mid_request_is_dropped() {
        let rig = Rig::with_limits(short(400, 300));
        // One byte at a time, forever: the deadline is for the whole request,
        // not for each read.
        let mut s = connect(&rig);
        let mut writer = s.try_clone().unwrap();
        let drip = thread::spawn(move || {
            let _ = writer.write_all(b"GET /v1/state HTTP/1.1\r\n");
            for _ in 0..100 {
                thread::sleep(Duration::from_millis(50));
                if writer.write_all(b"X").is_err() {
                    break;
                }
            }
        });
        let t = Instant::now();
        let r = read_response(&mut s);
        assert_eq!(r.status, 408, "{}", r.body);
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        let _ = s.shutdown(Shutdown::Both);
        drip.join().unwrap();

        // A complete head promising a body that never comes.
        let host = format!("127.0.0.1:{}", rig.server.port());
        let r = rig.raw(&format!(
            "POST /v1/sbx HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\n\
             Content-Length: 10\r\n\r\n{{\"o",
            rig.token
        ));
        assert_eq!(r.status, 408, "{}", r.body);
        assert!(!Mask(rig.fake.mask()).sbx());
        assert_eq!(rig.get("/v1/state").status, 200);
    }

    #[test]
    fn oversized_heads_are_refused() {
        let rig = Rig::new();
        let host = format!("127.0.0.1:{}", rig.server.port());
        let head = |pad: usize| {
            format!(
                "GET /v1/state HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\n\
                 X-Pad: {}\r\n\r\n",
                rig.token,
                "a".repeat(pad)
            )
        };
        let base = head(0).len();
        // Exactly at the limit is served, one byte more is not.
        assert_eq!(rig.raw(&head(MAX_HEAD - base)).status, 200);
        let r = rig.raw(&head(MAX_HEAD - base + 1));
        assert_eq!(r.status, 431, "{}", r.body);
        assert!(r.error().contains("larger than"), "{}", r.body);
        // Far too big, and one that never ends: refused without waiting.
        assert_eq!(rig.raw(&head(100_000)).status, 431);
        let t = Instant::now();
        let r = rig.raw(&format!(
            "GET /v1/state HTTP/1.1\r\nX-Pad: {}",
            "a".repeat(20_000)
        ));
        assert_eq!(r.status, 431, "{}", r.body);
        assert!(t.elapsed() < Duration::from_secs(3));
        assert_eq!(rig.get("/v1/state").status, 200);
    }

    #[test]
    fn connections_over_the_cap_are_refused_and_the_slots_come_back() {
        let rig = Rig::with_limits(Limits {
            max_connections: 4,
            ..short(5000, 300)
        });
        let held: Vec<TcpStream> = (0..4).map(|_| connect(&rig)).collect();
        wait_for("the connections were not accepted", || {
            rig.server.api.in_flight.load(Ordering::SeqCst) == 4
        });
        // Refused before anything is read from it.
        let mut over = connect(&rig);
        let r = read_response(&mut over);
        assert_eq!(r.status, 503, "{}", r.body);
        assert!(r.error().contains("too many requests"), "{}", r.body);
        // A client that sent a whole request (with a body) still gets the
        // 503, not a reset.
        for _ in 0..5 {
            let r = rig.post(
                "/v1/features",
                &format!(r#"{{"pad":"{}"}}"#, "x".repeat(3000)),
            );
            assert_eq!(r.status, 503, "{}", r.body);
            assert!(r.error().contains("too many requests"), "{}", r.body);
        }
        assert_eq!(rig.server.api.in_flight.load(Ordering::SeqCst), 4);
        // Closed clients free their slots.
        drop(held);
        wait_for("the slots were not released", || {
            rig.server.api.in_flight.load(Ordering::SeqCst) == 0
        });
        assert_eq!(rig.get("/v1/state").status, 200);
    }

    #[test]
    fn a_stalled_event_stream_is_dropped_and_its_slot_is_reused() {
        let rig = Rig::with_limits(short(5000, 300));
        let mut open: Vec<TcpStream> = (0..MAX_EVENT_CLIENTS).map(|_| rig.events()).collect();
        assert_eq!(rig.get("/v1/events").status, 503);
        // The first client stops reading while the server has more for it
        // than the socket buffers hold (the server only ever sends small
        // events, so this is put in its channel directly).
        let tx = lock(&rig.server.api.clients)[0].1.clone().unwrap();
        let t = Instant::now();
        tx.send("x".repeat(32 << 20)).unwrap();
        wait_for("the stalled stream was not dropped", || {
            lock(&rig.server.api.clients).len() == MAX_EVENT_CLIENTS - 1
        });
        assert!(t.elapsed() < Duration::from_secs(4), "{:?}", t.elapsed());
        drop(tx);
        // Its slot is free for a new stream, which gets events; the other
        // streams were not touched, and nothing else was blocked.
        let mut fresh = rig.events();
        assert_eq!(rig.get("/v1/events").status, 503);
        assert_eq!(rig.get("/v1/state").status, 200);
        rig.post("/v1/light", r#"{"on":false}"#);
        read_events_until(&mut fresh, "\"event\":\"light\"");
        read_events_until(&mut open[1], "\"event\":\"light\"");
    }

    #[test]
    fn writes_to_a_client_that_does_not_read_time_out() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server_side, _) = listener.accept().unwrap();
        let t = Instant::now();
        let e = write_within(
            &server_side,
            &vec![0u8; 64 << 20],
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(
            matches!(
                e.kind(),
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
            ),
            "{e}"
        );
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        drop(client);
    }

    #[test]
    fn request_heads_are_parsed_strictly() {
        let ok = parse_head("POST /v1/sbx?x=1 HTTP/1.0\r\nhost:  h \r\nContent-Length: 5").unwrap();
        assert_eq!(
            (ok.method.as_str(), ok.target.as_str()),
            ("POST", "/v1/sbx?x=1")
        );
        assert_eq!(ok.host.as_deref(), Some("h"));
        assert!(matches!(ok.body, BodyKind::Length(5)));
        let chunked = parse_head("POST / HTTP/1.1\r\nTransfer-Encoding: Chunked").unwrap();
        assert!(matches!(chunked.body, BodyKind::Chunked));
        let huge =
            parse_head("POST / HTTP/1.1\r\nContent-Length: 99999999999999999999999").unwrap();
        assert!(matches!(huge.body, BodyKind::Length(u64::MAX)));

        for (head, status) in [
            ("", 400),
            ("GET /", 400),
            ("GET / HTTP/2", 400),
            ("GET  / HTTP/1.1", 400),
            ("GE T / HTTP/1.1", 400),
            ("GET /\x01 HTTP/1.1", 400),
            ("GET / HTTP/1.1\r\nHost", 400),
            ("GET / HTTP/1.1\r\n Host: a", 400),
            ("GET / HTTP/1.1\r\nHost : a", 400),
            ("GET / HTTP/1.1\r\nHost: a\nb", 400),
            ("GET / HTTP/1.1\r\nHost: a\0", 400),
            ("GET / HTTP/1.1\r\nHost: a\r\nhost: b", 400),
            (
                "GET / HTTP/1.1\r\nAuthorization: a\r\nAuthorization: a",
                400,
            ),
            ("POST / HTTP/1.1\r\nContent-Length: -1", 400),
            ("POST / HTTP/1.1\r\nContent-Length: 1, 1", 400),
            ("POST / HTTP/1.1\r\nContent-Length: ", 400),
            (
                "POST / HTTP/1.1\r\nContent-Length: 1\r\nTransfer-Encoding: chunked",
                400,
            ),
            ("POST / HTTP/1.1\r\nTransfer-Encoding: gzip", 501),
        ] {
            let e = parse_head(head).err().unwrap_or_else(|| panic!("{head:?}"));
            assert_eq!(e.status, status, "{head:?}: {}", e.message);
        }
    }

    #[test]
    fn a_closed_event_stream_client_is_noticed_without_any_event() {
        let rig = Rig::new();
        let open: Vec<TcpStream> = (0..3).map(|_| rig.events()).collect();
        assert_eq!(lock(&rig.server.api.clients).len(), 3);
        drop(open);
        // No event is sent: the streams look at their client themselves.
        wait_for("closed clients were not noticed", || {
            lock(&rig.server.api.clients).is_empty()
        });
    }

    /// A POST to /v1/sbx with `body` sent as-is after the head; the client then
    /// closes its sending side.
    fn chunked(rig: &Rig, body: &str) -> Resp {
        let host = format!("127.0.0.1:{}", rig.server.port());
        let mut s = connect(rig);
        s.write_all(
            format!(
                "POST /v1/sbx HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\n\
                 Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n{body}",
                rig.token
            )
            .as_bytes(),
        )
        .unwrap();
        // Done sending: a body that stops early ends in EOF, not a wait.
        s.shutdown(Shutdown::Write).unwrap();
        read_response(&mut s)
    }

    #[test]
    fn chunked_bodies_are_decoded_and_routed() {
        let rig = Rig::new();
        // Two chunks (hex sizes, one with an extension), then a trailer.
        let r = chunked(
            &rig,
            "5\r\n{\"on\"\r\n6;ext=1\r\n:true}\r\n0\r\nX-Sum: 1\r\n\r\n",
        );
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.json(), json!({"ok": true, "result": ["sbx: on"]}));
        assert!(Mask(rig.fake.mask()).sbx());
        // No trailer.
        let r = chunked(&rig, "C\r\n{\"on\":false}\r\n0\r\n\r\n");
        assert_eq!(r.status, 200, "{}", r.body);
        assert!(!Mask(rig.fake.mask()).sbx());
        // An empty chunked body is an empty body.
        let r = chunked(&rig, "0\r\n\r\n");
        assert_eq!(r.status, 400);
        assert!(r.error().contains("not valid JSON"), "{}", r.body);

        let writes = rig.fake.written().len();
        for (body, needle) in [
            ("zz\r\n{}\r\n0\r\n\r\n", "bad chunk size"),
            ("\r\n{}\r\n0\r\n\r\n", "bad chunk size"),
            ("-1\r\n{}\r\n0\r\n\r\n", "bad chunk size"),
            ("123456789\r\n{}\r\n0\r\n\r\n", "bad chunk size"),
            // No CRLF after the chunk data.
            ("2\r\n{}XX0\r\n\r\n", "malformed chunked body"),
            // A bare LF instead of CRLF.
            ("2\n{}\r\n0\r\n\r\n", "malformed chunked body"),
            // Ends in the middle of a chunk, or without the last chunk.
            ("5\r\n{}", "before the request was complete"),
            ("2\r\n{}\r\n", "malformed chunked body"),
            // Trailers that never end.
            (
                &format!("0\r\n{}", "X: 1\r\n".repeat(20)),
                "too many trailer lines",
            ),
            // A size line longer than the limit.
            (
                &format!("{}\r\n", "0".repeat(MAX_LINE + 10)),
                "malformed chunked body",
            ),
        ] {
            let r = chunked(&rig, body);
            assert_eq!(r.status, 400, "{body:?}: {}", r.body);
            assert!(r.error().contains(needle), "{body:?}: {}", r.body);
        }
        // Over the cap in several chunks.
        let chunk = "x".repeat(MAX_BODY / 2 + 1);
        let big = format!(
            "{0:x}\r\n{chunk}\r\n{0:x}\r\n{chunk}\r\n0\r\n\r\n",
            chunk.len()
        );
        assert_eq!(chunked(&rig, &big).status, 413);
        assert_eq!(
            rig.fake.written().len(),
            writes,
            "nothing reached the device"
        );
    }

    #[test]
    fn the_echo_of_our_own_output_switch_is_not_followed_again() {
        let rig = Rig::new();
        assert_eq!(rig.post("/v1/direct", r#"{"on":true}"#).status, 200);
        let api = &rig.server.api;
        let frames = || rig.fake.written().len();
        // `write` followed the switch and noted it: its echo does nothing.
        *lock(&api.own_output) = Some(Output::Speakers);
        let before = frames();
        api.follow_direct_mode(Some(Output::Speakers));
        assert_eq!(frames(), before, "the echo was followed");
        assert!(lock(&api.own_output).is_none());
        // Without the note (the headphone jack), or for another output, or a
        // reconnect, Direct Mode follows.
        for (own, switched) in [
            (None, Some(Output::Speakers)),
            (Some(Output::Headphones), Some(Output::Speakers)),
            (Some(Output::Speakers), None),
        ] {
            *lock(&api.own_output) = own;
            let before = frames();
            api.follow_direct_mode(switched);
            assert!(frames() > before, "{own:?} {switched:?}");
            assert!(lock(&api.own_output).is_none());
        }
    }

    #[test]
    fn scout_owned_features_are_refused_while_scout_is_on() {
        let rig = Rig::new();
        assert_eq!(rig.post("/v1/scout", r#"{"on":true}"#).status, 200);
        let writes = rig.fake.written().len();
        for body in [
            r#"{"name":"eq_1k","value":6}"#,
            r#"{"name":"eq","value":false}"#,
            r#"{"name":"smartvol","value":false}"#,
            r#"{"name":"smartvol_level","value":0.5}"#,
        ] {
            let r = rig.post("/v1/features", body);
            assert_eq!(r.status, 409, "{body}: {}", r.body);
            assert!(r.error().starts_with("Scout Mode is on"), "{}", r.body);
        }
        assert_eq!(
            rig.fake.written().len(),
            writes,
            "nothing reached the device"
        );
        // The pre-amp and the other effects are not Scout's.
        for body in [
            r#"{"name":"eq_preamp","value":-1}"#,
            r#"{"name":"bass_level","value":0.3}"#,
        ] {
            let r = rig.post("/v1/features", body);
            assert_eq!(r.status, 200, "{body}: {}", r.body);
        }
        assert_eq!(rig.post("/v1/scout", r#"{"on":false}"#).status, 200);
        let r = rig.post("/v1/features", r#"{"name":"eq_1k","value":6}"#);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(rig.feature("eq_1k"), 6.0);
    }

    #[test]
    fn direct_mode_follows_output_switches() {
        let rig = Rig::new();
        let dm = DirectMode::new(rig.paths.direct_file());
        let eq = by_name("eq").unwrap();
        let wait_eq = |want: f32, what: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while rig.feature("eq") != want {
                assert!(Instant::now() < deadline, "{what}");
                thread::sleep(Duration::from_millis(5));
            }
        };
        assert_eq!(
            rig.post("/v1/features", r#"{"name":"eq","value":true}"#)
                .status,
            200
        );
        assert_eq!(rig.post("/v1/direct", r#"{"on":true}"#).status, 200);
        assert_eq!(rig.feature("eq"), 0.0);

        // The fake keeps one set of values: make the speakers' EQ look on.
        rig.fake.set_feature(eq.family, eq.id, 1.0);
        let r = rig.post("/v1/output", r#"{"output":"speakers"}"#);
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(rig.feature("eq"), 0.0, "EQ off on the new output too");
        assert_eq!(dm.pending_outputs().unwrap().len(), 2);
        // The device's echo of our own switch was seen and consumed.
        wait_for("the output echo was not seen", || {
            lock(&rig.server.api.own_output).is_none()
        });
        assert_eq!(rig.get("/v1/state").json()["direct_mode"], true);

        // The headphone jack: the same, without a request.
        rig.fake.set_feature(eq.family, eq.id, 1.0);
        rig.fake.switch_output(Output::Headphones);
        wait_eq(0.0, "EQ was not switched off after the jack switch");

        // Off on headphones restores them; the speakers' entry waits.
        assert_eq!(rig.post("/v1/direct", r#"{"on":false}"#).status, 200);
        assert_eq!(rig.feature("eq"), 1.0);
        assert_eq!(dm.pending_outputs().unwrap(), [Output::Speakers]);

        // Speakers again, still showing Direct Mode's state: the restore is
        // finished without a request.
        rig.fake.set_feature(eq.family, eq.id, 0.0);
        rig.fake.switch_output(Output::Speakers);
        wait_eq(1.0, "the speakers' EQ was not restored");
        let deadline = Instant::now() + Duration::from_secs(5);
        while dm.path().exists() {
            assert!(Instant::now() < deadline, "nothing left to restore");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(rig.get("/v1/state").json()["direct_mode"], false);
    }

    // ---- `g6 serve` --------------------------------------------------------

    #[test]
    fn serve_fails_before_the_device_when_the_port_is_taken() {
        let tmp = TempDir::new();
        let paths = Paths::under(&tmp.0);
        let taken = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let mut env = Env::new(
            Box::new(|| panic!("the device must not be opened")),
            Some(paths.clone()),
            Duration::from_secs(1),
        );
        let mut out = Vec::new();
        let e = run(&mut env, &mut out, port).unwrap_err();
        assert!(
            matches!(&e, CliError::Failure(m) if m.contains("cannot listen on 127.0.0.1")),
            "{e}"
        );
        // The token exists by then, and is not in anything printed.
        let token = std::fs::read_to_string(paths.config_dir.join(TOKEN_FILE)).unwrap();
        assert!(valid_token(&token));
        assert!(!e.to_string().contains(&token));
        assert!(out.is_empty());
    }

    #[test]
    fn serve_needs_a_config_directory_for_its_token() {
        let mut env = Env::new(
            Box::new(|| panic!("the device must not be opened")),
            None,
            Duration::from_secs(1),
        );
        let e = run(&mut env, &mut Vec::new(), 0).unwrap_err();
        assert!(
            matches!(&e, CliError::Failure(m) if m.contains("config directory")),
            "{e}"
        );
    }
}

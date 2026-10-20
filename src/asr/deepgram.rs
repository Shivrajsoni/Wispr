//! Deepgram live transcription over a WebSocket.
//!
//! One [`StreamingSession`] per utterance, each on its own thread. The control thread pushes
//! 16 kHz PCM16 chunks while the user speaks; on release it sends `Finalize` + `CloseStream`
//! and polls [`StreamingSession::try_result`], which resolves once Deepgram has flushed every
//! final result and sent its closing `Metadata` message. The session thread connects while audio
//! is already being queued, so connection setup (≈4 network round trips) overlaps with speaking;
//! [`StreamingSession::is_connected`] tells the daemon whether that has finished yet.
//!
//! Failures are typed ([`FailureKind`]) so the daemon's circuit breaker can tell a rejected key
//! (stop trying for a long time) from a network problem (back off briefly).

use std::fmt;
use std::io;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::HandshakeError;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub const US_HOST: &str = "api.deepgram.com";
pub const EU_HOST: &str = "api.eu.deepgram.com";
/// Poll interval of the session thread's socket reads.
const READ_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct DeepgramOptions {
    pub host: String,
    pub api_key: String,
    pub model: String,
    pub language: String,
    pub keyterms: Vec<String>,
    pub smart_format: bool,
    pub spoken_punctuation: bool,
    pub connect_timeout: Duration,
    pub finalize_timeout: Duration,
}

impl DeepgramOptions {
    pub fn url(&self) -> String {
        let mut url = format!(
            "wss://{}/v1/listen?model={}&language={}&encoding=linear16&sample_rate=16000&channels=1\
             &punctuate=true&smart_format={}&interim_results=false",
            self.host,
            encode(&self.model),
            encode(&self.language),
            self.smart_format,
        );
        if self.spoken_punctuation {
            url.push_str("&dictation=true");
        }
        // Keyterm prompting is a Nova-3 feature.
        if self.model.starts_with("nova-3") {
            for term in self.keyterms.iter().take(100) {
                url.push_str("&keyterm=");
                url.push_str(&encode(term));
            }
        }
        url
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Key rejected or out of credit: retrying soon is pointless.
    Auth,
    /// DNS, TCP, TLS, timeouts, dropped connections, server-side errors.
    Network,
    /// The daemon hung up (cancel or fallback); not Deepgram's fault.
    Cancelled,
}

#[derive(Debug)]
pub struct SessionError {
    pub kind: FailureKind,
    pub message: String,
}

impl SessionError {
    fn network(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Network,
            message: message.into(),
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SessionError {}

impl From<tungstenite::Error> for SessionError {
    fn from(e: tungstenite::Error) -> Self {
        Self::network(format!("Deepgram connection error: {e}"))
    }
}

impl From<io::Error> for SessionError {
    fn from(e: io::Error) -> Self {
        Self::network(format!("socket error: {e}"))
    }
}

pub type SessionResult = Result<String, SessionError>;

enum Command {
    Audio(Vec<u8>),
    Finish,
}

/// Dropping the session closes the command channel, which tells the thread to hang up.
pub struct StreamingSession {
    commands: Sender<Command>,
    results: Receiver<SessionResult>,
    connected: Arc<AtomicBool>,
}

impl StreamingSession {
    pub fn start(opts: DeepgramOptions) -> Self {
        let (commands, command_rx) = mpsc::channel();
        let (result_tx, results) = mpsc::channel();
        let connected = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&connected);
        // If the OS can't create a thread, the receiver sees a disconnected channel and
        // `try_result` reports a network failure, so the dictation falls back to Whisper.
        let spawned = thread::Builder::new()
            .name("deepgram".into())
            .spawn(move || {
                let outcome = run(&opts, &command_rx, &flag);
                let _ = result_tx.send(outcome);
            });
        if let Err(e) = spawned {
            crate::warn!("cannot start a Deepgram session thread: {e}");
        }
        Self {
            commands,
            results,
            connected,
        }
    }

    pub fn send_audio(&self, pcm16: Vec<u8>) {
        let _ = self.commands.send(Command::Audio(pcm16));
    }

    pub fn finish(&self) {
        let _ = self.commands.send(Command::Finish);
    }

    /// True once the WebSocket handshake has completed.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// `None` while still working; `Some` exactly once with the final transcript or the error.
    pub fn try_result(&self) -> Option<SessionResult> {
        match self.results.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(SessionError::network(
                "Deepgram session ended unexpectedly",
            ))),
        }
    }

    /// Blocks until the session produces its result or `timeout` elapses.
    pub fn wait(&self, timeout: Duration) -> SessionResult {
        self.results.recv_timeout(timeout).unwrap_or_else(|_| {
            Err(SessionError::network(format!(
                "Deepgram did not respond within {timeout:?}"
            )))
        })
    }
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn run(
    opts: &DeepgramOptions,
    commands: &Receiver<Command>,
    connected: &AtomicBool,
) -> SessionResult {
    let mut ws = connect(opts)?;
    connected.store(true, Ordering::Release);
    let mut finals: Vec<String> = Vec::new();
    let mut finishing: Option<Instant> = None;
    let mut sent_audio = false;

    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Audio(pcm)) => {
                    sent_audio = true;
                    ws.send(Message::binary(pcm))?;
                }
                Ok(Command::Finish) if finishing.is_none() => {
                    if !sent_audio {
                        let _ = ws.close(None);
                        return Ok(String::new());
                    }
                    // Finalize flushes buffered audio immediately; CloseStream guarantees the
                    // server ends the session even if there was nothing left to finalize.
                    ws.send(Message::text(r#"{"type":"Finalize"}"#))?;
                    ws.send(Message::text(r#"{"type":"CloseStream"}"#))?;
                    finishing = Some(Instant::now());
                }
                Ok(Command::Finish) => {}
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    return Err(SessionError {
                        kind: FailureKind::Cancelled,
                        message: "cancelled".into(),
                    });
                }
            }
        }

        if let Some(t) = finishing
            && t.elapsed() > opts.finalize_timeout
        {
            let _ = ws.close(None);
            return Err(SessionError::network(format!(
                "Deepgram did not finalize within {:?}",
                opts.finalize_timeout
            )));
        }

        match ws.read() {
            Ok(Message::Text(text)) => match parse_event(&text) {
                Event::Transcript { text, is_final, .. } => {
                    if is_final && !text.is_empty() {
                        finals.push(text);
                    }
                }
                // `from_finalize` can arrive while earlier audio is still being processed, so
                // the only reliable end marker is the Metadata sent after CloseStream.
                Event::Metadata if finishing.is_some() => return Ok(finals.join(" ")),
                Event::Error(msg) => {
                    return Err(SessionError::network(format!("Deepgram error: {msg}")));
                }
                _ => {}
            },
            Ok(Message::Close(_)) if finishing.is_some() => return Ok(finals.join(" ")),
            Ok(Message::Close(frame)) => {
                let reason = frame.map(|f| f.reason.to_string()).unwrap_or_default();
                return Err(SessionError::network(format!(
                    "Deepgram closed the connection: {reason}"
                )));
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if is_timeout(&e) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed)
                if finishing.is_some() =>
            {
                return Ok(finals.join(" "));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn resolve(host: &str) -> Result<Vec<SocketAddr>, SessionError> {
    (host, 443)
        .to_socket_addrs()
        .map(Iterator::collect)
        .map_err(|e| SessionError::network(format!("cannot resolve {host} (offline?): {e}")))
}

fn connect(opts: &DeepgramOptions) -> Result<Socket, SessionError> {
    let host = opts.host.as_str();
    let mut request = opts
        .url()
        .into_client_request()
        .map_err(|e| SessionError::network(format!("bad request: {e}")))?;
    let auth = format!("Token {}", opts.api_key)
        .parse()
        .map_err(|_| SessionError {
            kind: FailureKind::Auth,
            message: "the API key contains invalid characters".into(),
        })?;
    request.headers_mut().insert("Authorization", auth);

    let tcp = resolve(host)?
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, opts.connect_timeout).ok())
        .ok_or_else(|| {
            SessionError::network(format!(
                "cannot reach {host} within {:?}",
                opts.connect_timeout
            ))
        })?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(opts.connect_timeout))?;
    tcp.set_write_timeout(Some(opts.connect_timeout))?;

    let (ws, _response) = tungstenite::client_tls(request, tcp).map_err(|e| match e {
        HandshakeError::Failure(tungstenite::Error::Http(resp)) => {
            let status = resp.status();
            match status.as_u16() {
                401 | 403 => SessionError {
                    kind: FailureKind::Auth,
                    message: format!("Deepgram rejected the API key (HTTP {status})"),
                },
                402 => SessionError {
                    kind: FailureKind::Auth,
                    message: "Deepgram account is out of credit (HTTP 402)".into(),
                },
                _ => SessionError::network(format!(
                    "Deepgram refused the connection (HTTP {status})"
                )),
            }
        }
        HandshakeError::Failure(e) => {
            SessionError::network(format!("Deepgram handshake failed: {e}"))
        }
        HandshakeError::Interrupted(_) => SessionError::network("Deepgram handshake timed out"),
    })?;

    // From here on reads poll, so the loop can interleave sending audio and receiving results.
    match ws.get_ref() {
        MaybeTlsStream::NativeTls(s) => s.get_ref().set_read_timeout(Some(READ_POLL))?,
        MaybeTlsStream::Plain(s) => s.set_read_timeout(Some(READ_POLL))?,
        _ => {}
    }
    Ok(ws)
}

/// Opens and cleanly closes a session; returns the connection setup time.
pub fn check_connectivity(opts: &DeepgramOptions) -> Result<Duration, SessionError> {
    let t = Instant::now();
    let mut ws = connect(opts)?;
    let elapsed = t.elapsed();
    let _ = ws.send(Message::text(r#"{"type":"CloseStream"}"#));
    let _ = ws.close(None);
    Ok(elapsed)
}

/// TCP connect time to a host (one network round trip), or `None` if unreachable.
pub fn probe(host: &str, timeout: Duration) -> Option<Duration> {
    let addr = resolve(host).ok()?.into_iter().next()?;
    let t = Instant::now();
    TcpStream::connect_timeout(&addr, timeout).ok()?;
    Some(t.elapsed())
}

/// Probes the US and EU endpoints in parallel and returns the faster one (US if neither answers).
pub fn fastest_host(timeout: Duration) -> (&'static str, Vec<(&'static str, Option<Duration>)>) {
    let handles: Vec<_> = [US_HOST, EU_HOST]
        .into_iter()
        .map(|h| (h, thread::spawn(move || probe(h, timeout))))
        .collect();
    let results: Vec<(&'static str, Option<Duration>)> = handles
        .into_iter()
        .map(|(h, j)| (h, j.join().ok().flatten()))
        .collect();
    let best = results
        .iter()
        .filter_map(|(h, d)| d.map(|d| (*h, d)))
        .min_by_key(|(_, d)| *d)
        .map_or(US_HOST, |(h, _)| h);
    (best, results)
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Transcript {
        text: String,
        is_final: bool,
        from_finalize: bool,
    },
    Metadata,
    Error(String),
    Other,
}

pub fn parse_event(json: &str) -> Event {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Event::Other;
    };
    match v["type"].as_str() {
        Some("Results") => Event::Transcript {
            text: v["channel"]["alternatives"][0]["transcript"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string(),
            is_final: v["is_final"].as_bool().unwrap_or(false),
            from_finalize: v["from_finalize"].as_bool().unwrap_or(false),
        },
        Some("Metadata") => Event::Metadata,
        Some("Error") => Event::Error(
            v["description"]
                .as_str()
                .or_else(|| v["message"].as_str())
                .unwrap_or("unknown error")
                .to_string(),
        ),
        _ => Event::Other,
    }
}

/// Percent-encodes a query parameter value (RFC 3986 unreserved characters pass through).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> DeepgramOptions {
        DeepgramOptions {
            host: US_HOST.into(),
            api_key: "k".into(),
            model: "nova-3".into(),
            language: "en".into(),
            keyterms: vec!["PostgreSQL".into(), "Shiv Raj".into(), "C++".into()],
            smart_format: true,
            spoken_punctuation: false,
            connect_timeout: Duration::from_secs(1),
            finalize_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn url_contains_audio_format_and_encoded_keyterms() {
        let url = opts().url();
        assert!(url.starts_with("wss://api.deepgram.com/v1/listen?model=nova-3"));
        assert!(url.contains("encoding=linear16&sample_rate=16000&channels=1"));
        assert!(url.contains("&keyterm=PostgreSQL&keyterm=Shiv%20Raj&keyterm=C%2B%2B"));
        assert!(!url.contains("dictation"));
    }

    #[test]
    fn url_uses_selected_region() {
        let mut o = opts();
        o.host = EU_HOST.into();
        assert!(o.url().starts_with("wss://api.eu.deepgram.com/v1/listen?"));
    }

    #[test]
    fn keyterms_only_for_nova3() {
        let mut o = opts();
        o.model = "nova-2".into();
        assert!(!o.url().contains("keyterm"));
    }

    #[test]
    fn parses_final_results() {
        let json = r#"{"type":"Results","is_final":true,"speech_final":true,"from_finalize":true,
            "channel":{"alternatives":[{"transcript":" Hello there. ","confidence":0.99}]}}"#;
        assert_eq!(
            parse_event(json),
            Event::Transcript {
                text: "Hello there.".into(),
                is_final: true,
                from_finalize: true
            }
        );
    }

    #[test]
    fn parses_errors_and_metadata() {
        assert_eq!(
            parse_event(r#"{"type":"Metadata","request_id":"x"}"#),
            Event::Metadata
        );
        assert_eq!(
            parse_event(r#"{"type":"Error","description":"bad audio"}"#),
            Event::Error("bad audio".into())
        );
        assert_eq!(parse_event("not json"), Event::Other);
    }
}

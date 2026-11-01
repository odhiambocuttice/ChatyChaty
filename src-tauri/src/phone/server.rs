//! The HTTPS and WebSocket server that the phone connects to over local Wi-Fi.
//!
//! Every request must carry the pairing token as `?t=<token>`. `/` serves the
//! phone page; `/ws` is the WebSocket that carries audio and button presses.
//! Only one phone is connected at a time: a newer `/ws` connection closes the
//! older one.

use super::capture::{decode_pcm16le, PhoneCapture};
use super::identity::TlsIdentity;
use super::protocol::{
    parse_client_message, parse_request_head, render_page, state_message, token_matches,
    ClientMessage, PhoneState,
};
use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use handy_recorder::Sink;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};
use tokio_rustls::rustls;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// The longest request head the server reads.
const MAX_HEAD_BYTES: usize = 8 * 1024;
/// Time allowed for the TLS handshake plus the request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const PING_INTERVAL: Duration = Duration::from_secs(5);
/// The connection closes when nothing arrives from the phone for this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// Time allowed for one WebSocket send.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Everything the phone server needs from the app.
pub struct ServerConfig<S: Sink> {
    pub port: u16,
    pub token: String,
    pub identity: TlsIdentity,
    pub capture: PhoneCapture<S>,
    pub on_toggle: Arc<dyn Fn() + Send + Sync>,
    pub state: watch::Receiver<PhoneState>,
    pub on_connection_change: Arc<dyn Fn(bool) + Send + Sync>,
}

/// Which `/ws` connection is the current phone.
struct Current {
    /// Id of the current connection. `0` means no connection.
    id: u64,
    next_id: u64,
    stopped: bool,
}

struct Shared<S: Sink> {
    token: String,
    capture: PhoneCapture<S>,
    on_toggle: Arc<dyn Fn() + Send + Sync>,
    state: watch::Receiver<PhoneState>,
    on_connection_change: Arc<dyn Fn(bool) + Send + Sync>,
    current: Mutex<Current>,
    /// Tells connection tasks when the current id changes.
    current_tx: watch::Sender<u64>,
}

impl<S: Sink> Shared<S> {
    fn lock_current(&self) -> std::sync::MutexGuard<'_, Current> {
        self.current.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Makes a new connection the current phone. Returns `None` after `stop`.
    fn register(&self) -> Option<u64> {
        let mut current = self.lock_current();
        if current.stopped {
            return None;
        }
        current.next_id += 1;
        current.id = current.next_id;
        self.current_tx.send_replace(current.id);
        // Under the lock, so the change events keep the order of the connections.
        self.capture.set_connected(true);
        (self.on_connection_change)(true);
        Some(current.id)
    }

    /// Marks the phone disconnected, but only if `id` is still the current phone.
    fn unregister(&self, id: u64) {
        let mut current = self.lock_current();
        if current.id != id || current.stopped {
            return;
        }
        current.id = 0;
        self.current_tx.send_replace(0);
        self.capture.set_connected(false);
        (self.on_connection_change)(false);
    }
}

/// Stops the server when called.
type StopFn = Box<dyn FnOnce() + Send>;

/// A server that is accepting connections.
pub struct RunningServer {
    pub port: u16,
    accept_task: JoinHandle<()>,
    stop_connections: StopFn,
}

impl RunningServer {
    /// Stops accepting, closes the live connection and marks the phone disconnected.
    ///
    /// Returns after the listening socket is closed, so the port is free again.
    /// Does not call `on_connection_change`.
    pub async fn stop(self) {
        self.accept_task.abort();
        (self.stop_connections)();
        // The listener lives in the accept task; it closes when the task ends.
        if let Err(err) = self.accept_task.await {
            if !err.is_cancelled() {
                log::warn!("Phone server: accept task failed: {err}");
            }
        }
    }
}

/// Binds `0.0.0.0:port` (or any free port if that one is busy) and starts
/// serving on the current tokio runtime.
pub async fn start_server<S: Sink>(config: ServerConfig<S>) -> anyhow::Result<RunningServer> {
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("TLS protocol versions")?
    .with_no_client_auth()
    .with_single_cert(config.identity.cert_chain, config.identity.key)
    .context("TLS certificate")?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));

    let listener = match TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.port)).await {
        Ok(listener) => listener,
        Err(err) if config.port != 0 => {
            log::warn!(
                "Phone server: port {} is unavailable ({err}); using a free port",
                config.port
            );
            TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?
        }
        Err(err) => return Err(err.into()),
    };
    let port = listener.local_addr()?.port();

    let shared = Arc::new(Shared {
        token: config.token,
        capture: config.capture,
        on_toggle: config.on_toggle,
        state: config.state,
        on_connection_change: config.on_connection_change,
        current: Mutex::new(Current {
            id: 0,
            next_id: 0,
            stopped: false,
        }),
        current_tx: watch::channel(0).0,
    });

    let accept_task = tokio::spawn(accept_loop(listener, acceptor, shared.clone()));
    let stop_connections: StopFn = Box::new(move || {
        let mut current = shared.lock_current();
        current.stopped = true;
        current.id = 0;
        // Every live connection is now stale and closes itself.
        shared.current_tx.send_replace(0);
        shared.capture.set_connected(false);
    });

    log::info!("Phone server listening on port {port}");
    Ok(RunningServer {
        port,
        accept_task,
        stop_connections,
    })
}

async fn accept_loop<S: Sink>(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    shared: Arc<Shared<S>>,
) {
    loop {
        match listener.accept().await {
            Ok((tcp, peer)) => {
                tokio::spawn(handle_connection(
                    tcp,
                    peer,
                    acceptor.clone(),
                    shared.clone(),
                ));
            }
            Err(err) => {
                log::warn!("Phone server: accept failed: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle_connection<S: Sink>(
    tcp: TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
    shared: Arc<Shared<S>>,
) {
    let head = timeout(HEAD_TIMEOUT, async {
        let mut stream = acceptor.accept(tcp).await?;
        let (head, rest) = read_head(&mut stream).await?;
        Ok::<_, std::io::Error>((stream, head, rest))
    })
    .await;
    let (mut stream, head, rest) = match head {
        Ok(Ok(parts)) => parts,
        Ok(Err(err)) => {
            log::debug!("Phone server: request from {peer} failed: {err}");
            return;
        }
        Err(_) => {
            log::debug!("Phone server: request from {peer} timed out");
            return;
        }
    };

    let request = parse_request_head(&head).filter(|request| {
        request
            .token
            .as_deref()
            .is_some_and(|token| token_matches(&shared.token, token))
    });
    let Some(request) = request else {
        log::warn!("Phone server: rejected a request from {peer} without a valid token");
        respond(&mut stream, "403 Forbidden", &[], b"").await;
        return;
    };

    match (request.path.as_str(), request.websocket_key) {
        ("/", _) => {
            let page = render_page(&shared.token);
            respond(
                &mut stream,
                "200 OK",
                &[
                    ("Content-Type", "text/html; charset=utf-8"),
                    ("Cache-Control", "no-store"),
                ],
                page.as_bytes(),
            )
            .await;
        }
        ("/ws", Some(key)) => {
            let response = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                derive_accept_key(key.as_bytes())
            );
            let written = timeout(SEND_TIMEOUT, stream.write_all(response.as_bytes())).await;
            if !matches!(written, Ok(Ok(()))) {
                log::debug!("Phone server: WebSocket upgrade for {peer} failed");
                return;
            }
            let ws = WebSocketStream::from_partially_read(stream, rest, Role::Server, None).await;
            run_phone(ws, peer, shared).await;
        }
        _ => respond(&mut stream, "404 Not Found", &[], b"").await,
    }
}

/// Reads up to the blank line that ends the request head. Returns the head and
/// any bytes read after it.
async fn read_head<R: AsyncRead + Unpin>(stream: &mut R) -> std::io::Result<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let searched_from = buf.len().saturating_sub(3);
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf[searched_from..]
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
        {
            let end = searched_from + pos + 4;
            if end > MAX_HEAD_BYTES {
                break;
            }
            let rest = buf.split_off(end);
            return Ok((String::from_utf8_lossy(&buf).into_owned(), rest));
        }
        if buf.len() > MAX_HEAD_BYTES {
            break;
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "request head is too long",
    ))
}

/// Writes one HTTP response and closes the connection.
async fn respond<W: AsyncWrite + Unpin>(
    stream: &mut W,
    status: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let mut response = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    let mut bytes = response.into_bytes();
    bytes.extend_from_slice(body);
    let result = timeout(SEND_TIMEOUT, async {
        stream.write_all(&bytes).await?;
        stream.shutdown().await
    })
    .await;
    if let Ok(Err(err)) = result {
        log::debug!("Phone server: writing the response failed: {err}");
    }
}

/// Serves one phone WebSocket until it closes, goes idle or a newer phone replaces it.
async fn run_phone<T, S>(mut ws: WebSocketStream<T>, peer: SocketAddr, shared: Arc<Shared<S>>)
where
    T: AsyncRead + AsyncWrite + Unpin,
    S: Sink,
{
    // Subscribe before registering: the first change seen is our own id, and a
    // newer phone that registers right after us is never missed.
    let mut current_rx = shared.current_tx.subscribe();
    let Some(id) = shared.register() else {
        let _ = ws.close(None).await;
        return;
    };
    log::info!("Phone connected from {peer}");

    let mut state_rx = shared.state.clone();
    let mut state_open = true;
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.reset(); // The first tick would fire at once.
    let mut last_seen = Instant::now();

    let first = state_message(*state_rx.borrow_and_update());
    let mut replaced = false;
    if send(&mut ws, Message::text(first)).await {
        loop {
            tokio::select! {
                message = ws.next() => {
                    let Some(Ok(message)) = message else { break };
                    last_seen = Instant::now();
                    match message {
                        Message::Binary(bytes) => shared.capture.push_samples(&decode_pcm16le(&bytes)),
                        Message::Text(text) => {
                            if parse_client_message(text.as_str()) == Some(ClientMessage::Toggle) {
                                (shared.on_toggle)();
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                _ = ping.tick() => {
                    if last_seen.elapsed() >= IDLE_TIMEOUT {
                        log::info!("Phone at {peer} stopped responding");
                        break;
                    }
                    if !send(&mut ws, Message::Ping(Default::default())).await {
                        break;
                    }
                }
                changed = state_rx.changed(), if state_open => {
                    if changed.is_err() {
                        state_open = false;
                        continue;
                    }
                    let text = state_message(*state_rx.borrow_and_update());
                    if !send(&mut ws, Message::text(text)).await {
                        break;
                    }
                }
                _ = current_rx.changed() => {
                    if *current_rx.borrow_and_update() != id {
                        replaced = true;
                        break;
                    }
                }
            }
        }
    }

    if replaced {
        log::info!("Phone at {peer} was closed: a newer phone connected or the server stopped");
        let _ = timeout(SEND_TIMEOUT, ws.close(None)).await;
    } else {
        log::info!("Phone at {peer} disconnected");
        shared.unregister(id);
    }
}

/// Sends one message. Returns `false` if the connection is gone or stuck.
async fn send<T: AsyncRead + AsyncWrite + Unpin>(
    ws: &mut WebSocketStream<T>,
    message: Message,
) -> bool {
    matches!(timeout(SEND_TIMEOUT, ws.send(message)).await, Ok(Ok(())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone::identity::load_or_create_identity;
    use futures_util::{SinkExt, StreamExt};
    use handy_recorder::CollectingSink;
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio_rustls::client::TlsStream;
    use tokio_rustls::rustls::client::danger::{
        HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
    };
    use tokio_rustls::rustls::crypto::{ring, CryptoProvider};
    use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
    use tokio_rustls::TlsConnector;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::WebSocketStream;

    /// Test-only: the server's certificate is self-signed.
    #[derive(Debug)]
    struct AcceptAnyCert(Arc<CryptoProvider>);

    impl ServerCertVerifier for AcceptAnyCert {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    struct Harness {
        server: Option<RunningServer>,
        port: u16,
        token: String,
        capture: PhoneCapture<CollectingSink>,
        toggles: Arc<AtomicUsize>,
        connection_changes: Arc<Mutex<Vec<bool>>>,
        _state: tokio::sync::watch::Sender<PhoneState>,
        _dir: tempfile::TempDir,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            if let Some(server) = self.server.take() {
                tokio::spawn(server.stop());
            }
        }
    }

    async fn start() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let identity = load_or_create_identity(dir.path(), Ipv4Addr::LOCALHOST).unwrap();
        let token = "0123456789abcdef0123456789abcdef".to_string();
        let capture = PhoneCapture::new(4);
        let toggles = Arc::new(AtomicUsize::new(0));
        let connection_changes = Arc::new(Mutex::new(Vec::new()));
        let (state_tx, state_rx) = tokio::sync::watch::channel(PhoneState::Idle);
        let server = start_server(ServerConfig {
            port: 0,
            token: token.clone(),
            identity,
            capture: capture.clone(),
            on_toggle: {
                let toggles = toggles.clone();
                Arc::new(move || {
                    toggles.fetch_add(1, Ordering::SeqCst);
                })
            },
            state: state_rx,
            on_connection_change: {
                let changes = connection_changes.clone();
                Arc::new(move |connected| changes.lock().unwrap().push(connected))
            },
        })
        .await
        .unwrap();
        Harness {
            port: server.port,
            server: Some(server),
            token,
            capture,
            toggles,
            connection_changes,
            _state: state_tx,
            _dir: dir,
        }
    }

    async fn tls_connect(port: u16) -> TlsStream<TcpStream> {
        let provider = Arc::new(ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
            .with_no_client_auth();
        let tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
            .await
            .unwrap()
    }

    /// Sends one GET request and returns the whole response as text.
    async fn get(port: u16, target: &str) -> String {
        let mut stream = tls_connect(port).await;
        let request = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
                Ok(Ok(0)) | Ok(Err(_)) => break,
                Ok(Ok(n)) => response.extend_from_slice(&buf[..n]),
                Err(_) => panic!("response did not finish"),
            }
        }
        String::from_utf8_lossy(&response).into_owned()
    }

    async fn ws_connect(h: &Harness) -> WebSocketStream<TlsStream<TcpStream>> {
        let stream = tls_connect(h.port).await;
        let url = format!("wss://127.0.0.1:{}/ws?t={}", h.port, h.token);
        let (ws, _response) = tokio_tungstenite::client_async(url, stream).await.unwrap();
        ws
    }

    async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn bad_token_gets_403() {
        let h = start().await;
        let response = get(h.port, "/?t=wrong").await;
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    }

    #[tokio::test]
    async fn page_served_with_token() {
        let h = start().await;
        let response = get(h.port, &format!("/?t={}", h.token)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("Record"));
    }

    #[tokio::test]
    async fn audio_reaches_capture() {
        let h = start().await;
        let mut ws = ws_connect(&h).await;
        wait_until("phone connected", || h.capture.is_connected()).await;
        h.capture.start(CollectingSink::new()).unwrap();
        for _ in 0..4 {
            ws.send(Message::binary(vec![0x00, 0x40])).await.unwrap();
        }
        // The server handles messages in order, so once the toggle arrives the
        // audio sent before it has reached the capture.
        ws.send(Message::text(r#"{"type":"toggle"}"#))
            .await
            .unwrap();
        wait_until("toggle", || h.toggles.load(Ordering::SeqCst) == 1).await;
        assert_eq!(h.capture.stop().unwrap().into_samples(), vec![0.5; 4]);
    }

    #[tokio::test]
    async fn toggle_calls_callback() {
        let h = start().await;
        let mut ws = ws_connect(&h).await;
        ws.send(Message::text(r#"{"type":"toggle"}"#))
            .await
            .unwrap();
        wait_until("toggle", || h.toggles.load(Ordering::SeqCst) == 1).await;
    }

    #[tokio::test]
    async fn stop_frees_the_port() {
        let mut h = start().await;
        h.server.take().unwrap().stop().await;
        let rebound = tokio::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, h.port)).await;
        assert!(rebound.is_ok(), "port {} is still in use", h.port);
    }

    #[tokio::test]
    async fn new_connection_replaces_old() {
        let h = start().await;
        let mut first = ws_connect(&h).await;
        wait_until("phone connected", || h.capture.is_connected()).await;
        let _second = ws_connect(&h).await;

        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(message) = first.next().await {
                if matches!(message, Ok(Message::Close(_))) {
                    return true;
                }
                if message.is_err() {
                    return false;
                }
            }
            false
        })
        .await
        .expect("first connection was not closed");
        assert!(closed, "first connection ended without a close frame");
        // Let the first connection's task finish before checking the state.
        tokio::time::timeout(Duration::from_secs(5), async {
            while first.next().await.is_some() {}
        })
        .await
        .expect("first connection did not end");

        assert!(h.capture.is_connected());
        assert_eq!(*h.connection_changes.lock().unwrap(), vec![true, true]);
    }
}

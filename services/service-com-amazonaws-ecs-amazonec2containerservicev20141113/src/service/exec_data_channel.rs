// SPDX-License-Identifier: CC0-1.0

//! The DATA CHANNEL an execute-command session's stream is carried over: the opening
//! handshake this plane completes, the agent messages it then exchanges, and the session
//! inside the container they are carried to and from.
//!
//! A session cannot be served through the emulator's front door: that door serves ONE buffered
//! request per call and refuses a protocol upgrade, so the channel is served on a socket the
//! core lends this service for it ([`crate::state::ensure_exec_data_channel`] opens it and
//! owns its lifetime) and the `streamUrl` a session reports is that socket's own address.
//!
//! # What this module does with a connection, in order
//!
//! 1. ENCRYPTION, negotiated in band: the connection's first byte says whether the client
//!    dialled `wss://` or `ws://`, and the TLS half is the core's own server
//!    ([`::service_common::in_band_tls`]), so this crate never touches a key or a certificate.
//! 2. The HANDSHAKE, whose request line names the SESSION on the data-channel path
//!    ([`crate::state::session_id_from_data_channel_path`]). A handshake naming no session is
//!    refused; the TOKEN is not read here, because it travels in band — the first message a
//!    Session Manager client sends on the channel is the one that opens it.
//! 3. The channel's OPENING MESSAGE, the JSON a client sends with the `tokenValue` its
//!    `ExecuteCommand` answered. A token that is not the one that session was opened with, a
//!    session that is over, and a session whose stream is already being carried are each
//!    closed on the channel rather than refused at the handshake, which is where a client
//!    looks for them.
//! 4. The STREAM. The agent states the session it is about to carry (the handshake its own
//!    protocol carries), and from then on every message the client sends is one of three
//!    things: the keystrokes of the command it typed, the SIZE of its terminal, or its answer
//!    to that handshake. Keystrokes are echoed as a terminal echoes them and each completed
//!    line is run in the container's own session ([`crate::state::ContainerShell`]), whose
//!    output goes back as the stream's own messages — so what the client reads is what the
//!    container wrote.
//! 5. The CLOSE: the client closing the connection, a shell that exited, a session the clock
//!    found idle or a task that is gone each end the stream, and the session is over from then
//!    on — its token opens nothing further.
//!
//! Every decision is read from this service's own store under one lock, never held across an
//! await, and the session's own program runs where the connection does: nothing of a session
//! outlives the channel carrying it.

pub mod agent_message;

use ::std::pin::Pin;
use ::std::sync::{Arc, Mutex, PoisonError};
use ::std::time::{Duration, SystemTime};

use ::emulator_server::{ListenerConn, NativeHandler, RemoteOrigins, TlsUpgrader};
use ::futures_util::{SinkExt as _, StreamExt as _};
use ::tokio::io::{AsyncRead, AsyncWrite};
use ::tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request as HandshakeRequest, Response as HandshakeResponse,
};
use ::tokio_tungstenite::tungstenite::Message;

use agent_message::AgentMessage;

/// The largest MESSAGE this channel reads from a client, in bytes.
///
/// Stated here rather than left to the WebSocket implementation's default, so the bound a
/// caller's bytes are buffered under is citable: one message carries the keystrokes of a
/// typed line, a terminal size or a handshake answer, and this is far above all three while
/// keeping one local client from buffering the host's memory.
const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// The largest single FRAME it reads, under the same rule.
const MAX_FRAME_BYTES: usize = MAX_MESSAGE_BYTES;

/// How many bytes may wait on the WRITE side of one connection before the implementation
/// stops buffering: one message's worth for each of the lines a session holds for a reader
/// ([`crate::state::MAX_COLLECTED_LINES`] is what the session itself holds), so a client that
/// has stopped reading cannot pin more than the output that session was already bounded by.
const MAX_WRITE_BUFFER_BYTES: usize = 4 * MAX_MESSAGE_BYTES;

/// The explicit size bounds every connection of this channel is read and written under.
fn socket_limits() -> ::tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    ::tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES))
        .write_buffer_size(0)
        .max_write_buffer_size(MAX_WRITE_BUFFER_BYTES)
}

/// How long one carried session may run for. THIS NUMBER IS THE EMULATOR'S: it is the
/// deadline the session inside the container is given, so a line a client typed cannot hold
/// this process beyond it. The session's own idle timeout
/// ([`crate::state::EXEC_SESSION_IDLE_TIMEOUT`]) is what ends a session nobody is typing into.
const SESSION_LIFETIME: Duration = Duration::from_secs(3_600);

/// How long the channel waits for the client's opening message before giving up on the
/// connection. THIS NUMBER IS THE EMULATOR'S: a client that completed the handshake and then
/// said nothing holds a connection, and the opening message is the first thing a Session
/// Manager client sends.
const OPENING_MESSAGE_WAIT: Duration = Duration::from_secs(30);

/// How many bytes of a typed LINE this channel holds before the line is run as it stands.
/// THIS NUMBER IS THE EMULATOR'S: a client that types without ever pressing return would
/// otherwise grow this buffer without end. Far above any command line.
const MAX_TYPED_LINE_BYTES: usize = 64 * 1024;

/// The member a client's opening message carries its session token in.
const TOKEN_VALUE_MEMBER: &str = "TokenValue";

/// The agent version this channel reports in its handshake request. The handshake states who
/// is on the container's end of the channel, and what is on it here is this emulator's own
/// session rather than a released Systems Manager agent, so the version it reports says so.
const AGENT_VERSION: &str = "aws-emulator";

/// The session type the handshake states: an execute-command call runs the command it names,
/// which is the interactive-commands session a Session Manager client carries for Amazon ECS.
const SESSION_TYPE: &str = "InteractiveCommands";

/// The action a handshake asks the client to take, which is to agree the session type.
const SESSION_TYPE_ACTION: &str = "SessionType";

/// What a terminal writes to end a line: an interactive session is carried over a terminal,
/// which ends every line with both characters.
const TERMINAL_NEWLINE: &str = "\r\n";

/// The prompt the session writes when it is ready for a line. `PS1`'s default for the
/// superuser a container's exec session runs as, which is what a client reads before it types.
const SHELL_PROMPT: &str = "# ";

/// The handler the core hands each connection accepted on the sessions' data channel.
///
/// It carries the shared ENGINE rather than this service's store alone, because a session
/// resolves the volumes mounted into its container, which are other services' own records, and
/// the in-process callback its AWS calls are made over.
pub struct SessionSocket<Eng> {
    engine: Arc<Eng>,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    upgrader: Option<TlsUpgrader>,
}

impl<Eng> SessionSocket<Eng> {
    /// The socket handler over `engine`, with the core's TLS server for the `wss://` half of
    /// the announced address.
    #[must_use]
    pub fn new(
        engine: Arc<Eng>,
        callback: ::smithy_server_runtime::EmbeddedEmulator,
        upgrader: Option<TlsUpgrader>,
    ) -> Self {
        Self {
            engine,
            callback,
            upgrader,
        }
    }
}

impl<Eng: Send + Sync + 'static> NativeHandler for SessionSocket<Eng> {
    /// One call per accepted connection. The handles are cloned into the connection's own
    /// future, which is what the core's `'static` handler contract asks for.
    fn serve(
        &self,
        conn: ListenerConn,
    ) -> Pin<Box<dyn ::std::future::Future<Output = ()> + Send>> {
        let engine = Arc::clone(&self.engine);
        let callback = self.callback.clone();
        let upgrader = self.upgrader.clone();
        Box::pin(async move {
            serve_connection(engine, callback, upgrader, conn).await;
        })
    }
}

/// How a handshake this channel does not complete is answered: an HTTP status and the sentence
/// the client reads with it.
#[derive(Debug, Clone)]
struct Refusal {
    /// The HTTP status the handshake is refused with.
    status: u16,
    /// The sentence the client reads with it.
    message: &'static str,
}

impl Refusal {
    /// The refusal as the RFC 6455 implementation writes it. A status the `http` crate cannot
    /// build is answered as the internal failure it is.
    fn rendered(&self) -> ErrorResponse {
        ::http::Response::builder()
            .status(self.status)
            .header("content-type", "application/json")
            .body(Some(
                ::serde_json::json!({ "message": self.message }).to_string(),
            ))
            .unwrap_or_else(|_| {
                ::http::Response::builder()
                    .status(500)
                    .body(None)
                    .expect("500 with no body builds")
            })
    }
}

/// What a handshake whose path names no session is answered with.
const NO_SESSION_ON_THE_PATH: &str = "The request does not name a data channel of a session.";

/// What a handshake a browser made from another site is answered with.
const CROSS_SITE: &str = "This endpoint does not serve cross-site requests.";

/// Serve one accepted connection: negotiate encryption, complete (or refuse) the handshake,
/// then carry the session the path names until either side ends it.
// The handshake callback's `Err` is the REFUSAL RESPONSE the WebSocket implementation writes
// (`http::Response<Option<String>>`), which is that dependency's own signature and is large by
// the `http` crate's measure. Boxing it is not available: the callback's type is the one
// `accept_hdr_async_with_config` takes.
#[allow(clippy::result_large_err)]
async fn serve_connection<Eng: Send + Sync + 'static>(
    engine: Arc<Eng>,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    upgrader: Option<TlsUpgrader>,
    conn: ListenerConn,
) {
    let Some(conn) = ::service_common::in_band_tls::negotiated(conn, upgrader).await else {
        return;
    };
    // Which session the connection is for is read inside the handshake callback the
    // implementation calls with the request it parsed, so nothing here reads or replays the
    // handshake's own bytes.
    let addressed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let naming = Arc::clone(&addressed);
    let decide = move |request: &HandshakeRequest,
                       response: HandshakeResponse|
     -> Result<HandshakeResponse, ErrorResponse> {
        match admission(request) {
            Ok(session_id) => {
                *naming.lock().unwrap_or_else(PoisonError::into_inner) = Some(session_id);
                Ok(response)
            }
            Err(refusal) => {
                ::tracing::debug!(
                    status = refusal.status,
                    message = refusal.message,
                    "refusing an execute-command data-channel handshake"
                );
                Err(refusal.rendered())
            }
        }
    };
    let socket = match ::tokio_tungstenite::accept_hdr_async_with_config(
        conn,
        decide,
        Some(socket_limits()),
    )
    .await
    {
        Ok(socket) => socket,
        Err(error) => {
            // Either the refusal above (already written to the client by the handshake) or a
            // client that did not complete the handshake at all.
            ::tracing::debug!(%error, "an execute-command data channel was not opened");
            return;
        }
    };
    let Some(session_id) = addressed
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    else {
        return;
    };
    carry_session(engine, callback, session_id, socket).await;
}

/// Which SESSION this opening handshake is for, or the refusal it earns.
///
/// The browser check the core applies to every request on one of its own HTTP listeners is
/// applied first, because an opening handshake is a request a page can make to a loopback port
/// without a CORS preflight.
fn admission(request: &HandshakeRequest) -> Result<String, Refusal> {
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    if let ::emulator_server::BrowserVerdict::Forbid(reason) =
        ::emulator_server::classify_browser_request(
            header("origin"),
            header("sec-fetch-site"),
            // This socket is this emulator's, reached from the machine it runs on: the same
            // default a core HTTP listener carries.
            &RemoteOrigins::SameOriginOnly,
            false,
        )
    {
        ::tracing::info!(%reason, "refusing a data-channel handshake a browser made cross-site");
        return Err(Refusal {
            status: 403,
            message: CROSS_SITE,
        });
    }
    crate::state::session_id_from_data_channel_path(request.uri().path())
        .map(str::to_string)
        .ok_or(Refusal {
            status: 400,
            message: NO_SESSION_ON_THE_PATH,
        })
}

/// Carry one session over an established connection: admit it on the token its opening message
/// presents, run the container's session for the lines the client types, and end when either
/// side does.
async fn carry_session<Eng: Send + Sync + 'static, S>(
    engine: Arc<Eng>,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    session_id: String,
    socket: ::tokio_tungstenite::WebSocketStream<S>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(state) = crate::state::service_state(&*engine) else {
        return;
    };
    let (mut outgoing, mut incoming) = socket.split();
    let mut stream = Stream::new(&session_id);

    // The channel's opening message, which carries the token the session was opened with.
    let opened = match ::tokio::time::timeout(OPENING_MESSAGE_WAIT, incoming.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => presented_token(text.as_str()),
        Ok(Some(Ok(Message::Binary(bytes)))) => {
            ::std::str::from_utf8(&bytes).ok().and_then(presented_token)
        }
        _ => None,
    };
    let admitted = opened.is_some_and(|token| {
        state
            .lock()
            .admit_exec_data_channel(&session_id, &token, SystemTime::now())
    });
    if !admitted {
        // A token that is not this session's, a session that is over and one already being
        // carried are each told so ON the channel: that is where a client looks for them.
        ::tracing::info!(
            %session_id,
            "a data-channel connection presented no token this session admits"
        );
        let _ = outgoing
            .send(Message::Binary(stream.channel_closed().into()))
            .await;
        let _ = outgoing.close().await;
        return;
    }

    // The session INSIDE the container, built where the connection is carried: resolving the
    // volumes mounted into the container reads the stores of the services that hold them, so
    // no guard of this store is held while it is done.
    let Some(mut carried) = crate::state::exec_session_shell(
        &*engine,
        &callback,
        &session_id,
        SESSION_LIFETIME,
    ) else {
        ::tracing::info!(%session_id, "nothing of this session's container is left to carry it");
        state.lock().end_exec_session(&session_id);
        let _ = outgoing
            .send(Message::Binary(stream.channel_closed().into()))
            .await;
        let _ = outgoing.close().await;
        return;
    };
    carried
        .shell
        .resize_terminal(crate::state::DEFAULT_TERMINAL_ROWS, crate::state::DEFAULT_TERMINAL_COLUMNS);

    // The agent states the session it is about to carry. A client that answers it is sent the
    // completion before the stream starts; one that does not is carried all the same, because
    // what it then sends is the stream itself.
    if outgoing
        .send(Message::Binary(stream.handshake_request().into()))
        .await
        .is_err()
    {
        state.lock().end_exec_session(&session_id);
        return;
    }

    let mut typed = String::new();
    let mut prompted = false;
    loop {
        // What either service holds of this session, brought into line before the stream is
        // carried further: a session Systems Manager has been asked to end is one this
        // connection carries no further ([`crate::state::settle_exec_session_records`]).
        crate::state::settle_exec_session_records(&*engine, SystemTime::now());
        if !state.lock().exec_session_is_live(&session_id) {
            break;
        }
        let arrived = ::tokio::time::timeout(
            crate::state::EXEC_SESSION_IDLE_TIMEOUT,
            incoming.next(),
        )
        .await;
        let Ok(Some(Ok(frame))) = arrived else {
            // A client that closed, a connection that failed, and a session nothing typed
            // into for its whole idle timeout each end the stream.
            break;
        };
        let bytes = match frame {
            Message::Binary(bytes) => bytes.to_vec(),
            Message::Text(text) => text.as_str().as_bytes().to_vec(),
            Message::Close(_) => break,
            // Ping, Pong and Frame are the protocol's own: the implementation answers a ping
            // itself and no agent message is carried in one.
            _ => continue,
        };
        let Some(message) = AgentMessage::decode(&bytes) else {
            ::tracing::debug!(%session_id, "a frame that is not an agent message was read");
            continue;
        };
        state.lock().touch_exec_session(&session_id, SystemTime::now());
        // Every message a client sends is acknowledged, as the channel's own protocol
        // acknowledges one.
        if message.message_type == agent_message::INPUT_STREAM_DATA
            && outgoing
                .send(Message::Binary(stream.acknowledge(&message).into()))
                .await
                .is_err()
        {
            break;
        }
        if message.message_type == agent_message::CHANNEL_CLOSED {
            break;
        }
        if message.message_type != agent_message::INPUT_STREAM_DATA {
            continue;
        }
        let mut written = String::new();
        match message.payload_type {
            agent_message::PAYLOAD_TYPE_HANDSHAKE_RESPONSE => {
                // The client agreed the session type, so the agent says the handshake is
                // COMPLETE and the stream itself starts: the shell's prompt is the first thing
                // on it, which is what says the session is ready for a line.
                if outgoing
                    .send(Message::Binary(stream.handshake_complete().into()))
                    .await
                    .is_err()
                {
                    break;
                }
                if !prompted {
                    prompted = true;
                    written.push_str(SHELL_PROMPT);
                }
            }
            agent_message::PAYLOAD_TYPE_SIZE => {
                if let Some((rows, columns)) = terminal_size(message.payload_text()) {
                    carried.shell.resize_terminal(rows, columns);
                }
            }
            agent_message::PAYLOAD_TYPE_OUTPUT => {
                let Some(text) = message.payload_text() else {
                    continue;
                };
                // A terminal ECHOES what is typed into it, so the client reads its own line
                // back before the output of running it.
                written.push_str(&echoed(text));
                typed.push_str(&normalised(text));
            }
            // Every other payload is one this channel does not carry: an encryption challenge
            // for a session this emulator seals none of, a flag about a port nothing forwards.
            _ => continue,
        }
        if message.payload_type == agent_message::PAYLOAD_TYPE_HANDSHAKE_RESPONSE && !prompted {
            prompted = true;
            written.push_str(SHELL_PROMPT);
        }
        if message.payload_type == agent_message::PAYLOAD_TYPE_SIZE && typed.is_empty() {
            // A resize carries nothing to run, and the client reads nothing back for it.
            if !written.is_empty()
                && outgoing
                    .send(Message::Binary(stream.output(&written).into()))
                    .await
                    .is_err()
            {
                break;
            }
            continue;
        }
        // Each COMPLETE line is a line the shell reads; what is left over waits for the rest
        // of it, exactly as a shell reading its input waits.
        while let Some(end) = typed.find('\n') {
            let line: String = typed.drain(..=end).collect();
            carried.shell.run_typed_line(line.trim_end_matches('\n')).await;
            written.push_str(&delivered(&carried.output.take()));
            if carried.shell.has_ended() {
                break;
            }
            written.push_str(SHELL_PROMPT);
            prompted = true;
        }
        if typed.len() > MAX_TYPED_LINE_BYTES {
            // A client typing without end is not held: the line it has typed so far is run as
            // it stands, which is what the bound above is for.
            let line = ::std::mem::take(&mut typed);
            carried.shell.run_typed_line(&line).await;
            written.push_str(&delivered(&carried.output.take()));
        }
        if !written.is_empty()
            && outgoing
                .send(Message::Binary(stream.output(&written).into()))
                .await
                .is_err()
        {
            break;
        }
        if carried.shell.has_ended() {
            // The shell EXITED, so the session it was is over: the channel says so and the
            // store holds it as ended from here.
            let _ = outgoing
                .send(Message::Binary(stream.channel_closed().into()))
                .await;
            break;
        }
    }
    state.lock().end_exec_session(&session_id);
    // The SESSION is this store's and its Session Manager RECORD is that service's, so the
    // record of a session that is over is ended too — with no guard of this store held
    // ([`crate::state::settle_exec_session_records`]).
    crate::state::settle_exec_session_records(&*engine, SystemTime::now());
    let _ = outgoing.close().await;
    ::tracing::info!(%session_id, "an execute-command session's data channel is closed");
}

/// The `tokenValue` a client's opening message presents, and `None` for a message that is not
/// one — a body that is not an object, or one carrying no token.
fn presented_token(text: &str) -> Option<String> {
    let body: ::serde_json::Value = ::serde_json::from_str(text).ok()?;
    body.get(TOKEN_VALUE_MEMBER)?
        .as_str()
        .map(::std::string::ToString::to_string)
}

/// The terminal size a `Size` payload states, and `None` for a payload that states none.
///
/// The members are the ones a Session Manager client sends its terminal's size in,
/// `{"cols":…,"rows":…}`, and a value outside what a terminal can be is no size at all.
fn terminal_size(payload: Option<&str>) -> Option<(u32, u32)> {
    let body: ::serde_json::Value = ::serde_json::from_str(payload?).ok()?;
    let rows = u32::try_from(body.get("rows")?.as_u64()?).ok()?;
    let columns = u32::try_from(body.get("cols")?.as_u64()?).ok()?;
    (rows > 0 && columns > 0).then_some((rows, columns))
}

/// `text` with every line ending written as a terminal writes one ([`TERMINAL_NEWLINE`]).
fn normalised(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// What a terminal ECHOES for `text`: the characters themselves, with each line ending written
/// as the terminal writes one.
fn echoed(text: &str) -> String {
    normalised(text).replace('\n', TERMINAL_NEWLINE)
}

/// The lines a session wrote, as the stream delivers them: each on its own line, ended the way
/// a terminal ends one.
fn delivered(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| format!("{line}{TERMINAL_NEWLINE}"))
        .collect()
}

/// The agent's own side of one channel: the sequence its messages stand in, and the ids they
/// carry.
///
/// A stream's messages are NUMBERED, so a client reads them in the order the agent wrote them,
/// and the first one carries the flag that says where the sequence starts.
struct Stream {
    /// What this session is, which its message ids are derived from.
    session_id: String,
    /// How many messages this side has written.
    written: i64,
}

impl Stream {
    /// The agent's side of the channel carrying `session_id`, with nothing written yet.
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            written: 0,
        }
    }

    /// One message of this stream, taking the next sequence number.
    fn message(&mut self, message_type: &str, payload_type: u32, payload: Vec<u8>) -> Vec<u8> {
        let sequence = self.written;
        self.written += 1;
        let flags = if sequence == 0 {
            agent_message::FLAG_FIRST
        } else {
            0
        };
        let created = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        AgentMessage::new(
            message_type,
            sequence,
            payload_type,
            payload,
            message_id(&self.session_id, sequence),
            u64::try_from(created).unwrap_or(u64::MAX),
            flags,
        )
        .encode()
    }

    /// The handshake request: what is on the container's end of this channel, and the session
    /// it is about to carry.
    fn handshake_request(&mut self) -> Vec<u8> {
        let payload = ::serde_json::json!({
            "AgentVersion": AGENT_VERSION,
            "RequestedClientActions": [{
                "ActionType": SESSION_TYPE_ACTION,
                "ActionParameters": { "SessionType": SESSION_TYPE },
            }],
        })
        .to_string();
        self.message(
            agent_message::OUTPUT_STREAM_DATA,
            agent_message::PAYLOAD_TYPE_HANDSHAKE_REQUEST,
            payload.into_bytes(),
        )
    }

    /// The handshake COMPLETION, which is what says the stream itself starts next.
    ///
    /// It carries how long the handshake took and the message the customer reads with it; the
    /// span is this channel's own and the message is empty, because there is nothing about the
    /// session a client has to be told before its stream.
    fn handshake_complete(&mut self) -> Vec<u8> {
        let payload = ::serde_json::json!({
            "HandshakeTimeToComplete": 0,
            "CustomerMessage": "",
        })
        .to_string();
        self.message(
            agent_message::OUTPUT_STREAM_DATA,
            agent_message::PAYLOAD_TYPE_HANDSHAKE_COMPLETE,
            payload.into_bytes(),
        )
    }

    /// One message of the session's OUTPUT.
    fn output(&mut self, text: &str) -> Vec<u8> {
        self.message(
            agent_message::OUTPUT_STREAM_DATA,
            agent_message::PAYLOAD_TYPE_OUTPUT,
            text.as_bytes().to_vec(),
        )
    }

    /// The acknowledgement of `message`, naming the message it acknowledges exactly as the
    /// channel's protocol names one.
    fn acknowledge(&mut self, message: &AgentMessage) -> Vec<u8> {
        let payload = ::serde_json::json!({
            "AcknowledgedMessageType": message.message_type,
            "AcknowledgedMessageId": message.message_id_text(),
            "AcknowledgedMessageSequenceNumber": message.sequence_number,
            "IsSequentialMessage": true,
        })
        .to_string();
        self.message(
            agent_message::ACKNOWLEDGE,
            agent_message::PAYLOAD_TYPE_OUTPUT,
            payload.into_bytes(),
        )
    }

    /// The message that says this channel is over.
    fn channel_closed(&mut self) -> Vec<u8> {
        let payload = ::serde_json::json!({
            "MessageType": agent_message::CHANNEL_CLOSED,
            "SessionId": self.session_id,
        })
        .to_string();
        self.message(
            agent_message::CHANNEL_CLOSED,
            agent_message::PAYLOAD_TYPE_OUTPUT,
            payload.into_bytes(),
        )
    }
}

/// The id of the `sequence`-th message of the session `session_id`: the 16 bytes a message
/// carries its id in, derived from the session and that number so no two messages of this
/// emulator share one and nothing of the session is readable out of it.
fn message_id(session_id: &str, sequence: i64) -> [u8; 16] {
    use ::sha2::Digest as _;
    let mut hasher = ::sha2::Sha256::new();
    hasher.update(session_id.as_bytes());
    hasher.update(sequence.to_be_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a client's opening message presents, read off the member the channel's protocol
    /// carries it in. The rows span a well-formed message, one carrying other members beside
    /// it, and the forms that present no token at all.
    #[test]
    fn the_token_a_client_presents_is_read_off_its_opening_message() {
        let rows: [(&str, &str, Option<&str>); 5] = [
            ("the message a client sends", r#"{"TokenValue":"abc"}"#, Some("abc")),
            (
                "the whole message a client sends",
                r#"{"MessageSchemaVersion":"1.0","RequestId":"r","TokenValue":"t","ClientId":"c"}"#,
                Some("t"),
            ),
            ("a message carrying no token", r#"{"ClientId":"c"}"#, None),
            ("a token that is not text", r#"{"TokenValue":7}"#, None),
            ("a body that is not an object", "[]", None),
        ];
        for (case, body, expected) in rows {
            assert_eq!(
                presented_token(body).as_deref(),
                expected,
                "case {case:?}: expected {expected:?} off {body}"
            );
        }
    }

    /// The terminal size a client states, read off the members it states it in. The rows span
    /// a stated size, one carrying other members, and the values that state no size.
    #[test]
    fn the_terminal_size_a_client_states_is_read_off_its_payload() {
        /// One row: what the case is, the payload the client sent, and the size it states.
        type SizeRow = (&'static str, &'static str, Option<(u32, u32)>);
        let rows: [SizeRow; 5] = [
            ("a stated size", r#"{"cols":120,"rows":40}"#, Some((40, 120))),
            (
                "a size beside other members",
                r#"{"rows":24,"cols":80,"other":1}"#,
                Some((24, 80)),
            ),
            ("a size of no rows", r#"{"cols":80,"rows":0}"#, None),
            ("a payload naming one side", r#"{"cols":80}"#, None),
            ("a payload that is not an object", "7", None),
        ];
        for (case, payload, expected) in rows {
            assert_eq!(
                terminal_size(Some(payload)),
                expected,
                "case {case:?}: expected {expected:?} off {payload}"
            );
        }
        assert_eq!(
            terminal_size(None),
            None,
            "a payload that is not text states no size"
        );
    }

    /// What a terminal echoes and what it delivers each end a line the way a terminal ends
    /// one, whichever ending the other side wrote. The rows span the three line endings a
    /// client can type and text carrying none.
    #[test]
    fn a_terminal_ends_every_line_the_way_a_terminal_ends_one() {
        let rows: [(&str, &str, &str); 4] = [
            ("a line ended with a newline", "ls\n", "ls\r\n"),
            ("a line ended with a return", "ls\r", "ls\r\n"),
            ("a line ended with both", "ls\r\n", "ls\r\n"),
            ("text ending no line", "ls", "ls"),
        ];
        for (case, typed, expected) in rows {
            assert_eq!(
                echoed(typed),
                expected,
                "case {case:?}: expected {expected:?} echoed for {typed:?}"
            );
        }
        assert_eq!(
            delivered(&["one".to_string(), "two".to_string()]),
            "one\r\ntwo\r\n",
            "every line a session wrote is delivered ended as a terminal ends one"
        );
    }

    /// Two messages of one stream never share an id or a sequence number, and the first one
    /// carries the flag that says where the sequence starts.
    #[test]
    fn a_stream_numbers_its_messages_and_flags_the_first() {
        let mut stream = Stream::new("ecs-execute-command-1");
        let first = AgentMessage::decode(&stream.output("one"))
            .expect("the message a stream writes reads back");
        let second = AgentMessage::decode(&stream.output("two"))
            .expect("the message a stream writes reads back");
        assert_eq!(
            (first.sequence_number, first.flags),
            (0, agent_message::FLAG_FIRST),
            "expected the first message to open the sequence, got {first:?}"
        );
        assert_eq!(
            (second.sequence_number, second.flags),
            (1, 0),
            "expected the second message to follow it, got {second:?}"
        );
        assert_ne!(
            first.message_id, second.message_id,
            "expected two messages of one stream to carry distinct ids"
        );
    }
}

// SPDX-License-Identifier: CC0-1.0

//! A broker's WEB CONSOLE: the socket this emulator serves it on, who may open it, and the
//! page it answers with.
//!
//! RULE: a URL a control plane publishes for a resource's console is an address this
//! emulator ANSWERS an HTTP request at. `BrokerInstance$consoleURL` is "The brokers web
//! console URL" — an address an operator opens verbatim — so it is the address of a socket
//! the CORE opened for that broker (`engine.listeners().http`, which owns the socket, the
//! bind interface, the port range, the TLS and the browser checks), and the console behind
//! it is answered from the broker's own records. A name under Amazon MQ's own DNS zone, or
//! any host this process does not answer on, resolves to nothing on the machine this
//! emulator runs on, so a broker that published one would report a healthy RUNNING console
//! at an address no operator could open.
//!
//! WHO may open it is the broker's own users. The ActiveMQ Web Console authenticates a
//! broker user and admits the ones the broker grants console access to
//! (`User$consoleAccess`, "Enables access to the ActiveMQ Web Console for the ActiveMQ user.
//! Does not apply to RabbitMQ brokers"), so a request carrying no credentials or credentials
//! that are not a user's is answered `401` and one from a user the broker grants no console
//! access `403`. The users are read from the control plane's store on every request, so a
//! user created or deleted is in force for the next one.
//!
//! WHAT it answers is rendered from state: the broker as the store holds it now, the users
//! it holds now, and nothing composed from the request. No password ever reaches the page or
//! a log line.
//!
//! The socket's life is the broker's: the handle lives on the store under the broker's own
//! id, and dropping it (the delete) closes the port.

use std::future::Future;
use std::pin::Pin;
use std::time::SystemTime;

use super::*;

/// The media type the console page is served as: a web page, served from this emulator's own
/// origin.
const CONSOLE_MEDIA_TYPE: &str = "text/html; charset=utf-8";

/// The HTTP authentication scheme the console challenges with: the user-name-and-password
/// scheme a broker user signs in with (RFC 7617).
const CONSOLE_AUTH_SCHEME: &str = "Basic";

/// The protection space the challenge names, which is the broker whose console it is.
fn console_realm(broker: &Broker) -> String {
    format!("{} broker {}", reported_engine_type(&broker.engine_type), broker.broker_id)
}

/// One broker's console, as the store holds it: where it answers, and the socket's handle.
///
/// Dropping it closes the port, so it is held for exactly as long as the broker it belongs
/// to.
pub struct BrokerConsole {
    endpoint: ServedAddress,
    /// The socket itself. Never read: it is held because dropping it closes the port.
    _listener: ::emulator_server::ResourceListener,
}

impl std::fmt::Debug for BrokerConsole {
    /// Everything about this record that is not a live socket: where it answers.
    /// `ResourceListener` is not `Debug`.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrokerConsole")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl BrokerConsole {
    /// The address an operator opens this console at.
    pub fn endpoint(&self) -> &ServedAddress {
        &self.endpoint
    }
}

/// Open the socket a broker's web console is served on, or `None` where no socket could be
/// opened at all — on which the broker publishes the port Amazon MQ publishes its engine's
/// console on, under the host this emulator answered the create on.
///
/// Called by [`super::serve_broker`] once the record is stored, with the listeners the core
/// vends.
pub(super) async fn open_broker_console(
    listeners: &::std::sync::Arc<::emulator_server::Listeners>,
    state: &::engine::ServiceState<ComAmazonawsMqMqState>,
    broker_id: &str,
) -> Option<BrokerConsole> {
    let handler = BrokerConsoleServer {
        state: state.clone(),
        broker_id: broker_id.to_string(),
    };
    let listeners = ::std::sync::Arc::clone(listeners);
    // Opened on a task of its OWN, for the same reason the broker's wire socket is: a
    // runtime that cannot bind a socket at all — one built with no IO driver, which is what
    // a host driving this emulator in process may hand a request — leaves this create
    // without a console socket instead of taking the request down with it.
    let opened = ::tokio::spawn(async move {
        listeners
            .http(handler, ::emulator_server::HttpPolicy::default())
            .await
            .ok()
    })
    .await;
    let listener = opened.ok().flatten()?;
    Some(BrokerConsole {
        // The core's own reading of the address it bound ([`served_address_of`]), shared
        // with the broker's wire socket so the two cannot drift on what a client dials.
        endpoint: served_address_of(&listener),
        _listener: listener,
    })
}

/// Serves ONE broker's web console on the core's listener. The core runs the browser checks
/// and owns CORS; this answers for the broker the address belongs to.
struct BrokerConsoleServer {
    /// The control plane's store, so the page and the sign-in read the broker as it is NOW.
    state: ::engine::ServiceState<ComAmazonawsMqMqState>,
    /// Which broker this console belongs to.
    broker_id: String,
}

impl ::emulator_server::HttpHandler for BrokerConsoleServer {
    fn handle(
        &self,
        request: ::http::Request<Vec<u8>>,
    ) -> Pin<Box<dyn Future<Output = ::http::Response<Vec<u8>>> + Send>> {
        let state = self.state.clone();
        let broker_id = self.broker_id.clone();
        Box::pin(async move { console_response(&state, &broker_id, &request) })
    }
}

/// One request to a broker's console, answered from the store — the decision the listener's
/// handler serves, and the one this service's tests drive.
///
/// A READ: the page is rendered from the broker as it stands, so opening the console changes
/// nothing a later caller sees. The one write behind it is the clock's own
/// ([`Broker::apply_due_changes`] through [`ComAmazonawsMqMqState::settled_broker`]), owed at
/// this instant whether or not anybody opened the console.
pub fn console_response(
    state: &::engine::ServiceState<ComAmazonawsMqMqState>,
    broker_id: &str,
    request: &::http::Request<Vec<u8>>,
) -> ::http::Response<Vec<u8>> {
    let method = request.method();
    // A console is READ; nothing is submitted to it here, so every other method is answered
    // with the methods this address does serve.
    if method != ::http::Method::GET && method != ::http::Method::HEAD {
        let mut response = page(
            ::http::StatusCode::METHOD_NOT_ALLOWED,
            &refusal_page("Method not allowed"),
            false,
        );
        response.headers_mut().insert(
            ::http::header::ALLOW,
            ::http::HeaderValue::from_static("GET, HEAD"),
        );
        return response;
    }
    let now = SystemTime::now();
    // The record is copied out and the control plane's store RELEASED before anything else
    // is read, so no two locks are ever held at once.
    let broker_record = {
        let mut store = state.lock();
        store.settled_broker(broker_id, now).cloned()
    };
    let Some(broker_record) = broker_record else {
        // The broker is gone — a delete closes this socket, so this is the window
        // between the two — and its console publishes nothing of another broker's.
        return page(
            ::http::StatusCode::NOT_FOUND,
            &refusal_page("No such broker"),
            method == ::http::Method::HEAD,
        );
    };
    let head = method == ::http::Method::HEAD;
    match admitted(&broker_record, presented_credentials(request.headers())) {
        Admission::Granted(username) => page(
            ::http::StatusCode::OK,
            &console_page(&broker_record, &username, now),
            head,
        ),
        Admission::Unauthenticated => challenge(&broker_record, head),
        Admission::Forbidden => page(
            ::http::StatusCode::FORBIDDEN,
            &refusal_page("This user is not granted console access"),
            head,
        ),
    }
}

/// What the console does with the credentials one request carried.
enum Admission {
    /// They are a user the broker admits: the console is served, signed in as that user.
    Granted(String),
    /// The request carried no credentials, or none that are a user of this broker — the
    /// same answer either way, so a challenge says nothing about which users exist.
    Unauthenticated,
    /// They are a user of the broker, which grants it no console access.
    Forbidden,
}

/// Whether `presented` admits its holder to `broker`'s console.
///
/// The broker's own users decide it, read from the control plane's store, so the users the
/// broker HAS are the users that can sign in. `User$consoleAccess` "Enables access to the
/// ActiveMQ Web Console for the ActiveMQ user. Does not apply to RabbitMQ brokers", so the
/// flag decides for an ActiveMQ broker and every user of a RabbitMQ broker may open its
/// management console.
fn admitted(broker: &Broker, presented: Option<(String, String)>) -> Admission {
    let Some((username, password)) = presented else {
        return Admission::Unauthenticated;
    };
    let Some(user) = broker.users.get(&username) else {
        return Admission::Unauthenticated;
    };
    if user.password != password {
        return Admission::Unauthenticated;
    }
    if broker.engine_type != ENGINE_TYPE_RABBITMQ && !user.console_access {
        return Admission::Forbidden;
    }
    Admission::Granted(username)
}

/// The user name and password one request presented, from its `Authorization` header: the
/// `Basic` scheme's `base64(user-id ":" password)` (RFC 7617), where the user id carries no
/// colon and the password may carry any.
///
/// `None` for a request carrying no credentials, another scheme, or a value that is not that
/// encoding — each of which presents no user, and is answered with the challenge rather than
/// with a reading of the request.
fn presented_credentials(headers: &::http::HeaderMap) -> Option<(String, String)> {
    let value = headers.get(::http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, credentials) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case(CONSOLE_AUTH_SCHEME) {
        return None;
    }
    let decoded = ::aws_smithy_types::base64::decode(credentials.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (username, password) = text.split_once(':')?;
    Some((username.to_string(), password.to_string()))
}

/// The answer to a request that presented no user of this broker: the challenge naming the
/// scheme and the protection space, which is what makes a browser ask for a sign-in.
fn challenge(broker: &Broker, head: bool) -> ::http::Response<Vec<u8>> {
    let mut response = page(
        ::http::StatusCode::UNAUTHORIZED,
        &refusal_page("Sign in with a broker user"),
        head,
    );
    let realm = format!(
        "{CONSOLE_AUTH_SCHEME} realm=\"{}\", charset=\"UTF-8\"",
        console_realm(broker)
    );
    if let Ok(value) = ::http::HeaderValue::from_str(&realm) {
        response
            .headers_mut()
            .insert(::http::header::WWW_AUTHENTICATE, value);
    }
    response
}

/// One console answer: `body` as a web page, with no body at all for a `HEAD` request —
/// which asks for the headers the same `GET` would answer with.
fn page(status: ::http::StatusCode, body: &str, head: bool) -> ::http::Response<Vec<u8>> {
    let length = body.len();
    let bytes = match head {
        true => Vec::new(),
        false => body.as_bytes().to_vec(),
    };
    ::http::Response::builder()
        .status(status)
        .header(::http::header::CONTENT_TYPE, CONSOLE_MEDIA_TYPE)
        .header(::http::header::CONTENT_LENGTH, length.to_string())
        .body(bytes)
        .unwrap_or_else(|_| ::http::Response::new(Vec::new()))
}

/// The page a request this console does not serve is answered with: the reason, and nothing
/// about the broker — a caller that has not signed in reads no state.
fn refusal_page(reason: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>Broker web console</title>\n</head>\n<body>\n<h1>{}</h1>\n</body>\n</html>\n",
        escaped(reason)
    )
}

/// The page a signed-in operator opening a broker's console reads: the broker as the store
/// holds it right now, and the users it holds right now.
///
/// Every value comes from the stored record, so the page cannot disagree with what
/// `DescribeBroker` reports about the same broker. A name is caller-supplied text, so it is
/// written into the document through the `html-escape` crate's escaping rather than inlined;
/// no password is on the page at all — no read of a broker's users reports one.
fn console_page(broker: &Broker, username: &str, now: SystemTime) -> String {
    let mut document = String::new();
    let title = format!(
        "{} web console — {}",
        reported_engine_type(&broker.engine_type),
        broker.broker_name
    );
    document.push_str(&format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n</head>\n<body>\n<h1>{heading}</h1>\n\
         <p>Signed in as {user}</p>\n<dl>\n",
        title = escaped(&title),
        heading = escaped(&broker.broker_name),
        user = escaped(username),
    ));
    for (term, value) in [
        ("Broker", broker.broker_id.as_str()),
        ("State", broker.broker_state(now)),
        ("Engine", broker.engine_version.as_str()),
        ("Deployment mode", broker.deployment_mode.as_str()),
        ("Host instance type", broker.host_instance_type.as_str()),
    ] {
        document.push_str(&format!(
            "<dt>{}</dt><dd>{}</dd>\n",
            escaped(term),
            escaped(value)
        ));
    }
    document.push_str("</dl>\n<h2>Users</h2>\n<ul>\n");
    for user in broker.users.values() {
        document.push_str(&format!("<li>{}</li>\n", escaped(&user.username)));
    }
    document.push_str("</ul>\n</body>\n</html>\n");
    document
}

/// `value` as text inside an HTML element: the `html-escape` crate's own escaping, so no
/// stored string can open or close an element of the page it is written into.
fn escaped(value: &str) -> String {
    ::html_escape::encode_safe(value).into_owned()
}

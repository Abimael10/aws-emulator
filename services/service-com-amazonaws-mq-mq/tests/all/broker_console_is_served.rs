// SPDX-License-Identifier: CC0-1.0

//! The BROKER WEB CONSOLE a running broker publishes, opened at the address it published.
//!
//! RULE: a URL a control plane publishes for a resource's console is an address this
//! emulator ANSWERS an HTTP request at — `BrokerInstance$consoleURL` is "The brokers web
//! console URL", which an operator opens verbatim — and what answers there is the broker's
//! own console: it admits the broker's own users, the ones it grants console access to
//! (`User$consoleAccess`, "Enables access to the ActiveMQ Web Console for the ActiveMQ user.
//! Does not apply to RabbitMQ brokers"), refuses everyone else, and renders the broker out of
//! the state the control plane holds.
//!
//! What that rules out is a SYNTHESIZED authority: a name composed from the broker's id,
//! this service's label and a Region resolves to nothing on the machine this emulator runs
//! on, so a broker publishing one would report a console no operator could open.
//!
//! The rows span what the rule generalises over: the engine, which decides whether console
//! access is a flag of the user at all, and the credentials a request presents — none, a
//! user the broker admits, that user with the wrong password, a user the broker does not
//! have, a user it grants no console access, and the forms an `Authorization` header can
//! carry that present no user.
//!
//! The console is served over the TLS the core terminates on its own listener, so each case
//! dials the published `host:port`, completes that handshake and writes the bytes an HTTP
//! client writes. The broker reads, the poll loop and the certificate verifier are the
//! crate's existing test helpers (`crate::broker_lifecycle`, `crate::broker_restart`,
//! `crate::activemq_data_plane`, `crate::rabbitmq_data_plane`); `tests/all/main.rs` is this
//! binary's crate root, so each is reached through `crate::<suite>`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use service_com_amazonaws_mq_mq::state::BROKER_STATE_RUNNING;

use crate::activemq_data_plane::authority_of;
use crate::broker_lifecycle::{create_body, string_at, Mq};
use crate::broker_restart::poll_until_state;
use crate::rabbitmq_data_plane::AcceptTheEmulatorsCertificate;

/// How long a case waits for a condition it polls for. Generous: it bounds a condition that
/// should already hold, and a loaded host must not fail on it.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits between looks.
const POLL_EVERY: Duration = Duration::from_millis(20);

/// The user every broker in this suite grants console access to.
const CONSOLE_USER: &str = "console-operator";

/// That user's password.
const CONSOLE_PASSWORD: &str = "Str0ng-Console-Pass";

/// The user an ActiveMQ broker of this suite grants no console access to.
const WIRE_USER: &str = "wire-only-user";

/// That user's password.
const WIRE_PASSWORD: &str = "Str0ng-Wire-Pass";

/// The loopback address the core binds a resource listener on by default, and so the only
/// address a case here dials. The host the console URL names is asserted to BE this address
/// before anything is dialled.
const LOOPBACK: &str = "127.0.0.1";

/// One engine whose brokers publish a console: how a broker of it is created, and whether
/// its users carry the console-access flag at all.
struct ConsoleEngine {
    case: &'static str,
    engine_type: &'static str,
    engine_version: &'static str,
    host_instance_type: &'static str,
    /// Whether `User$consoleAccess` applies to this engine: it "Does not apply to RabbitMQ
    /// brokers", whose every user may open the management console.
    console_access_applies: bool,
}

/// The engines this suite drives, with the modelled values a create accepts for each.
const CONSOLE_ENGINES: &[ConsoleEngine] = &[
    ConsoleEngine {
        case: "an ActiveMQ broker's web console",
        engine_type: "ACTIVEMQ",
        engine_version: "5.18",
        host_instance_type: "mq.t3.micro",
        console_access_applies: true,
    },
    ConsoleEngine {
        case: "a RabbitMQ broker's management console",
        engine_type: "RABBITMQ",
        engine_version: "3.13",
        host_instance_type: "mq.m5.large",
        console_access_applies: false,
    },
];

/// One HTTP answer read off a console socket: the status, its headers lower-cased, and the
/// body.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    /// The value of the response header `name`, if the answer carries one.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Create one broker of `engine` with the users above and answer its id, without waiting
/// for it: a create is answered before the clock has made the broker RUNNING.
async fn create(mq: &Mq, engine: &ConsoleEngine, name: &str) -> String {
    let mut body = create_body(name);
    body["engineType"] = json!(engine.engine_type);
    body["engineVersion"] = json!(engine.engine_version);
    body["deploymentMode"] = json!("SINGLE_INSTANCE");
    body["hostInstanceType"] = json!(engine.host_instance_type);
    body["publiclyAccessible"] = json!(false);
    body["users"] = json!([
        {
            "username": CONSOLE_USER,
            "password": CONSOLE_PASSWORD,
            "consoleAccess": true,
        },
        {
            "username": WIRE_USER,
            "password": WIRE_PASSWORD,
            "consoleAccess": false,
        },
    ]);
    let (status, created) = mq.create_broker(body).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected 200 from create, got {status} {created:?}",
        engine.case
    );
    string_at(&created, "/brokerId")
}

/// The console URL a RUNNING broker publishes, and the `host:port` it names — held to this
/// machine's own loopback address before any case dials it, so a composed name fails the
/// case before anything is opened.
async fn published_console(mq: &Mq, case: &str, broker_id: &str) -> (String, u16) {
    let described = poll_until_state(mq, broker_id, BROKER_STATE_RUNNING).await;
    let console_url = string_at(
        &described
            .pointer("/brokerInstances/0")
            .cloned()
            .unwrap_or(Value::Null),
        "/consoleURL",
    );
    assert!(
        console_url.starts_with("https://"),
        "case {case:?}: expected an https console URL, got {console_url:?}"
    );
    let (host, port) = authority_of(case, &console_url);
    assert_eq!(
        host, LOOPBACK,
        "case {case:?}: the published console must be an address this emulator answers on, \
         got {console_url:?}"
    );
    (console_url, port)
}

/// Write one HTTP/1.1 request on the console's own socket and read the whole answer.
///
/// Written over a raw TLS connection rather than with an HTTP client crate because what is
/// under test is that the ADDRESS the control plane published answers an HTTP request at
/// all: the bytes written are the bytes a browser writes.
async fn exchange(
    case: &str,
    port: u16,
    method: &str,
    target: &str,
    authorization: Option<&str>,
) -> Answer {
    // The target is this machine's loopback address, named as the literal that
    // `published_console` pinned the published host to before this is reached.
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the published console must accept a connection, got {error}")
        });
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .expect("the provider supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptTheEmulatorsCertificate(provider)))
        .with_no_client_auth();
    // The one protocol these bytes are written in, so the answer is read in it too.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name =
        rustls::pki_types::ServerName::try_from("localhost").expect("localhost is a name");
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the console must complete the TLS handshake https promises, got {error}")
        });
    let mut request = format!("{method} {target} HTTP/1.1\r\nHost: {LOOPBACK}:{port}\r\n");
    if let Some(credentials) = authorization {
        request.push_str(&format!("Authorization: {credentials}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    tls.write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request must be written, got {error}"));
    tls.flush()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request must flush, got {error}"));
    let mut raw = Vec::new();
    tls.read_to_end(&mut raw)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the answer must be read, got {error}"));
    parsed(case, &raw)
}

/// One raw HTTP/1.1 answer, split into its status, its headers and its body.
fn parsed(case: &str, raw: &[u8]) -> Answer {
    let text = String::from_utf8_lossy(raw).into_owned();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: expected an HTTP answer, got {text:?}"));
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {text:?}"));
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status code, got {status_line:?}"));
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    Answer {
        status,
        headers,
        body: body.to_string(),
    }
}

/// The `Authorization: Basic` value for `user:password`, encoded as RFC 7617 states it.
fn basic(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        ::aws_smithy_types::base64::encode(format!("{user}:{password}"))
    )
}

/// One credential a case presents, and the status the console answers it with.
struct Presented {
    case: &'static str,
    authorization: Option<String>,
    /// The status for an engine whose users carry the console-access flag, and the one for
    /// an engine whose users do not — the same for every row but the user that is granted
    /// no console access.
    status: u16,
    status_without_console_access_flag: u16,
}

/// RULE: a broker's published console admits the broker's own users and only the ones it
/// grants console access to — a request presenting no user at all is challenged, one
/// presenting a user the broker does not have or a wrong password is challenged the same
/// way, and a user the broker grants no console access is refused — while a user it admits
/// reads the console itself.
#[tokio::test]
async fn the_published_console_admits_only_a_user_the_broker_grants_access_to() {
    let mq = Mq::new("us-west-2");

    // Every broker is created first and polled afterwards: the move to RUNNING is owed by
    // the clock, so the brokers come due together rather than one after another.
    let mut brokers: Vec<(&ConsoleEngine, String)> = Vec::new();
    for engine in CONSOLE_ENGINES {
        let name = format!("console-{}", engine.engine_type.to_lowercase());
        brokers.push((engine, create(&mq, engine, &name).await));
    }

    for (engine, broker_id) in &brokers {
        let (console_url, port) = published_console(&mq, engine.case, broker_id).await;
        let rows: &[Presented] = &[
            Presented {
                case: "no credentials at all",
                authorization: None,
                status: 401,
                status_without_console_access_flag: 401,
            },
            Presented {
                case: "a user the broker grants console access to",
                authorization: Some(basic(CONSOLE_USER, CONSOLE_PASSWORD)),
                status: 200,
                status_without_console_access_flag: 200,
            },
            Presented {
                case: "that user with the wrong password",
                authorization: Some(basic(CONSOLE_USER, "Wr0ng-Console-Pass")),
                status: 401,
                status_without_console_access_flag: 401,
            },
            Presented {
                case: "a user the broker does not have",
                authorization: Some(basic("nobody-here", CONSOLE_PASSWORD)),
                status: 401,
                status_without_console_access_flag: 401,
            },
            Presented {
                case: "a user the broker grants no console access",
                authorization: Some(basic(WIRE_USER, WIRE_PASSWORD)),
                status: 403,
                // `consoleAccess` does not apply to a RabbitMQ broker, whose every user
                // may open the management console.
                status_without_console_access_flag: 200,
            },
            Presented {
                case: "another authentication scheme",
                authorization: Some("Bearer an-opaque-token".to_string()),
                status: 401,
                status_without_console_access_flag: 401,
            },
            Presented {
                case: "credentials that are not the scheme's encoding",
                authorization: Some("Basic not-base64!!".to_string()),
                status: 401,
                status_without_console_access_flag: 401,
            },
            Presented {
                case: "an encoding carrying no password",
                authorization: Some(format!(
                    "Basic {}",
                    ::aws_smithy_types::base64::encode(CONSOLE_USER)
                )),
                status: 401,
                status_without_console_access_flag: 401,
            },
        ];
        for row in rows {
            let case = format!("{} / {}", engine.case, row.case);
            let expected = match engine.console_access_applies {
                true => row.status,
                false => row.status_without_console_access_flag,
            };
            let answer = exchange(&case, port, "GET", "/", row.authorization.as_deref()).await;
            assert_eq!(
                answer.status, expected,
                "case {case:?}: expected {expected} from {console_url:?}, got {} {:?}",
                answer.status, answer.body
            );
            if answer.status == 401 {
                let challenge = answer.header("www-authenticate").unwrap_or_default();
                assert!(
                    challenge.starts_with("Basic realm="),
                    "case {case:?}: a challenge names the scheme and the protection space, got \
                     {challenge:?}"
                );
            }
            // Nothing about the broker is readable without signing in.
            if answer.status != 200 {
                assert!(
                    !answer.body.contains(broker_id.as_str()),
                    "case {case:?}: a refused request must read nothing of the broker, got {:?}",
                    answer.body
                );
            }
        }

        // The console is READ: the methods it does not serve are refused naming the ones it
        // does, and a HEAD answers the headers its GET would with no body at all.
        let case = format!("{} / a method the console does not serve", engine.case);
        let answer = exchange(
            &case,
            port,
            "POST",
            "/",
            Some(&basic(CONSOLE_USER, CONSOLE_PASSWORD)),
        )
        .await;
        assert_eq!(
            answer.status, 405,
            "case {case:?}: expected 405, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.header("allow"),
            Some("GET, HEAD"),
            "case {case:?}: a refusal of a method names the methods served, got {:?}",
            answer.headers
        );
        let case = format!("{} / a HEAD of the console", engine.case);
        let answer = exchange(
            &case,
            port,
            "HEAD",
            "/",
            Some(&basic(CONSOLE_USER, CONSOLE_PASSWORD)),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected 200, got {} {:?}",
            answer.status, answer.body
        );
        assert!(
            answer.body.is_empty(),
            "case {case:?}: a HEAD carries no body, got {:?}",
            answer.body
        );
    }
}

/// RULE: the page a broker's console answers with is rendered from that broker's own state —
/// the broker the address belongs to and nobody else's, and the users it holds now — so what
/// an operator reads on it cannot disagree with what the control plane reports.
#[tokio::test]
async fn the_console_page_is_rendered_from_the_brokers_own_state() {
    let mq = Mq::new("eu-west-1");
    let engine = &CONSOLE_ENGINES[0];

    // TWO brokers, so what one console publishes can be told from what the other does.
    let first = create(&mq, engine, "console-state-first").await;
    let second = create(&mq, engine, "console-state-second").await;
    let (_, first_port) = published_console(&mq, "the first broker", &first).await;
    let (_, second_port) = published_console(&mq, "the second broker", &second).await;
    assert_ne!(
        first_port, second_port,
        "each broker's console is a socket of its own, got the port {first_port} twice"
    );

    let credentials = basic(CONSOLE_USER, CONSOLE_PASSWORD);
    let case = "the first broker's console";
    let page = exchange(case, first_port, "GET", "/", Some(&credentials))
        .await
        .body;
    for expected in [
        first.as_str(),
        "console-state-first",
        BROKER_STATE_RUNNING,
        CONSOLE_USER,
        WIRE_USER,
    ] {
        assert!(
            page.contains(expected),
            "case {case:?}: expected the page to carry {expected:?}, got {page:?}"
        );
    }
    assert!(
        !page.contains(second.as_str()) && !page.contains("console-state-second"),
        "case {case:?}: a console reads its OWN broker, got {page:?}"
    );
    assert!(
        !page.contains(CONSOLE_PASSWORD) && !page.contains(WIRE_PASSWORD),
        "case {case:?}: no password belongs on the page"
    );

    // A user created on the broker is in force for the next request, the page being read
    // from the store rather than composed once.
    let added = "added-operator";
    let (status, created) = mq
        .call(
            "POST",
            &format!("/v1/brokers/{first}/users/{added}"),
            Some(json!({ "password": CONSOLE_PASSWORD, "consoleAccess": true })),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from the user create, got {status} {created:?}"
    );
    let page = exchange(case, first_port, "GET", "/", Some(&credentials))
        .await
        .body;
    assert!(
        page.contains(added),
        "case {case:?}: expected the page to carry the user just created, got {page:?}"
    );
    assert!(
        !page.contains(CONSOLE_PASSWORD),
        "case {case:?}: no password belongs on the page"
    );
}

/// RULE: a broker's console socket lives exactly as long as the broker — deleting the broker
/// closes the port, so the console URL it published answers nothing.
#[tokio::test]
async fn deleting_a_broker_closes_the_console_it_published() {
    let mq = Mq::new("us-east-2");
    let engine = &CONSOLE_ENGINES[0];
    let broker_id = create(&mq, engine, "console-delete").await;
    let (console_url, port) = published_console(&mq, "before the delete", &broker_id).await;
    let answer = exchange(
        "before the delete",
        port,
        "GET",
        "/",
        Some(&basic(CONSOLE_USER, CONSOLE_PASSWORD)),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case \"before the delete\": expected 200 from {console_url:?}, got {} {:?}",
        answer.status, answer.body
    );

    let (status, deleted) = mq
        .call("DELETE", &format!("/v1/brokers/{broker_id}"), None)
        .await;
    assert_eq!(
        status, 200,
        "expected 200 from the delete, got {status} {deleted:?}"
    );

    // Polled for the condition: the listener is torn down with the record. The target is
    // this machine's loopback address, named as the literal the published console was held
    // to above.
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        match TcpStream::connect(("127.0.0.1", port)).await {
            Err(_) => break,
            Ok(_) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "case \"after the delete\": {console_url:?} must stop accepting connections \
                     once the broker is deleted"
                );
                tokio::time::sleep(POLL_EVERY).await;
            }
        }
    }
}

// SPDX-License-Identifier: CC0-1.0

//! What a client READS off the wire when this service answers it: the media type the reply
//! announces, the class an error states, and the header the `awsJson` family never carries.
//!
//! RULE pinned here: **every reply this service sends — a success and every class of refusal
//! alike — announces its body in `content-type` as the JSON media type the REQUEST named this
//! endpoint's API under; an error states its class in the body's `__type` of that same JSON
//! document; and no reply carries an `x-amzn-query-error` header, because this service's only
//! protocol is `awsJson1_0`
//! (`smithy_query(query="digest")` reports the one protocol `awsJson1_0`) and that family
//! conveys the class in the body rather than in a query-compatibility header.**
//!
//! Which layer decided the reply does not change any of the three: an authorization refusal
//! the handler raised, an input-constraint refusal the validating walk raised, a modelled
//! error about stored state, a target the front door binds nothing for, and a served listing
//! are all one document class on this wire.
//!
//! The rows span what the rule generalises over: the CLASS of the reply (an identity-based
//! implicit deny, an explicit deny, a value outside a declared range, a resource this account
//! does not hold, a target no operation serves, and a success), and the media type the request
//! announces (the protocol's own `application/x-amz-json-1.0` and the plain
//! `application/json` the same endpoint also serves its API under).
//!
//! That this endpoint serves its API under BOTH of those media types, and announces its reply
//! in the one the request named it under, is not this suite's rule to establish: it belongs to
//! `crate::endpoint_front_door`, which owns it and pins it across every media type a client can
//! announce, over the layer that applies it (`service::front_door::announced`). What the rows
//! here add is that a REFUSAL is announced by that same rule — the axis the issue this suite
//! answers was filed about — so each media type appears beside the refusal classes rather than
//! on its own.
//!
//! Driven over a REAL HTTP connection to the served endpoint rather than through the
//! in-process dispatch, so what every row asserts is the bytes a client receives: the status
//! line, the header block as it was written, and the body. HTTP field names are
//! case-insensitive, so a header is looked up with its case folded
//! ([`Reply::header`]) — which is how any client reads one.
//!
//! The emulator is DynamoDB beside IAM and STS on ONE shared engine, exactly as `core/router`
//! assembles the three: the role lives in IAM's store and its session credential is minted by
//! STS. Request signatures are not verified here — what a row drives is the AUTHORIZATION of a
//! known principal, which this service reads from the access key id the request's credential
//! scope names, so the rows name a session's key id without computing its signature.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account every generated emulator presents.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is the account's own credential.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The session name every `AssumeRole` here asks for.
const SESSION: &str = "probe";

/// The policy-language version every document here states.
const POLICY_VERSION: &str = "2012-10-17";

/// The media type the `awsJson1_0` protocol this service speaks publishes.
const PROTOCOL_MEDIA_TYPE: &str = "application/x-amz-json-1.0";

/// The other JSON media type the same endpoint serves its API under.
const PLAIN_JSON_MEDIA_TYPE: &str = "application/json";

/// The header the awsQuery-compatibility shim of ANOTHER service's wire carries its legacy
/// code in, and which this service's plain `awsJson1_0` wire never carries.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// How long one exchange may take to be answered before the row gives up. Generous: it bounds
/// a hang, and nothing here waits for the clock.
const REPLY_DEADLINE: Duration = Duration::from_secs(60);

/// Which credential a row's request is signed with.
#[derive(Debug, Clone, Copy)]
enum Caller {
    /// The account's own published credential, which the account's resources admit.
    Account,
    /// A role session the account grants nothing at all — IAM's implicit deny.
    Granted,
    /// A role session whose policy allows the action and then denies it explicitly.
    ExplicitlyDenied,
}

/// One row: the request a client sends, and what the reply must state about itself.
struct Row {
    case: &'static str,
    /// The media type the request announces the API under.
    announced: &'static str,
    target: &'static str,
    body: Value,
    caller: Caller,
    status: u16,
    /// The media type the reply must announce.
    media_type: &'static str,
    /// The local name of the class the reply states in `__type`, or `None` for a success,
    /// which states no class at all.
    class: Option<&'static str>,
    /// The member a SERVED reply's document must carry, so a row that expects a success
    /// asserts the document it got rather than only the absence of a class.
    member: Option<&'static str>,
}

/// One reply as it arrived: the status, the header block, and the body.
#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    /// The value of the `name` field, with its case folded — an HTTP field name is
    /// case-insensitive, so this is how a client reads one.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The local name of the class the body states in `__type`, or `None` when the body
    /// states none. The member carries a fully qualified shape id on this wire, and a client
    /// that classifies an error reads the name after the namespace.
    fn class(&self) -> Option<String> {
        let document: Value = serde_json::from_str(&self.body).ok()?;
        let stated = document.get("__type")?.as_str()?;
        Some(
            stated
                .rsplit(['#', '/'])
                .next()
                .unwrap_or(stated)
                .to_string(),
        )
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is `access_key_id`.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
    )
}

/// One request/response exchange over a fresh connection to the served endpoint.
///
/// The request is written as the bytes of an HTTP/1.1 message and the reply is read back as
/// bytes, so nothing about the header block is normalised by a client library before a row
/// asserts on it. `Connection: close` lets the whole reply be read to the end of the stream.
fn exchange(case: &str, port: u16, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut message = format!("POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    for (name, value) in headers {
        message.push_str(&format!("{name}: {value}\r\n"));
    }
    message.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));

    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .unwrap_or_else(|error| panic!("case {case:?}: nothing accepts a connection: {error}"));
    stream
        .set_read_timeout(Some(REPLY_DEADLINE))
        .unwrap_or_else(|error| panic!("case {case:?}: the read deadline is refused: {error}"));
    stream
        .write_all(message.as_bytes())
        .unwrap_or_else(|error| panic!("case {case:?}: the request is not sent: {error}"));
    stream
        .flush()
        .unwrap_or_else(|error| panic!("case {case:?}: the request is not flushed: {error}"));
    let mut answered = Vec::new();
    stream
        .read_to_end(&mut answered)
        .unwrap_or_else(|error| panic!("case {case:?}: the reply is not read: {error}"));
    let text = String::from_utf8_lossy(&answered).to_string();

    let (head, body) = text.split_once("\r\n\r\n").unwrap_or_else(|| {
        panic!("case {case:?}: the reply must carry a header block, got {text:?}")
    });
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: {status_line:?} states no status"));
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect();
    Reply {
        status,
        headers,
        body: body.to_string(),
    }
}

/// One `application/x-www-form-urlencoded` call against the service `signing_name` names,
/// signed with the account's own credential — the wire IAM and STS speak.
fn query(case: &str, port: u16, signing_name: &str, body: &str) -> Reply {
    exchange(
        case,
        port,
        &[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("Authorization", &authorization(AMBIENT_KEY, signing_name)),
        ],
        body,
    )
}

/// Percent-encode one form component through the maintained encoder rather than by hand.
fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The text of the first `<name>…</name>` element of an XML body.
fn element(case: &str, body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let (_, rest) = body
        .split_once(&open)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must carry <{name}>, got {body}"));
    let (text, _) = rest
        .split_once(&close)
        .unwrap_or_else(|| panic!("case {case:?}: <{name}> must be closed, got {body}"));
    text.to_string()
}

/// A trust policy letting the account assume the role.
fn trust_policy() -> String {
    json!({
        "Version": POLICY_VERSION,
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// Create `role`, embed `document` in it when there is one, assume it, and answer the access
/// key id of the session STS minted.
fn role_session(case: &str, port: u16, role: &str, document: Option<&str>) -> String {
    let created = query(
        case,
        port,
        "iam",
        &format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={}&AssumeRolePolicyDocument={}",
            form_encode(role),
            form_encode(&trust_policy())
        ),
    );
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateRole({role}) must answer 200, got {created:?}"
    );
    let role_arn = element(case, &created.body, "Arn");

    if let Some(document) = document {
        let embedded = query(
            case,
            port,
            "iam",
            &format!(
                "Action=PutRolePolicy&Version=2010-05-08&RoleName={}&PolicyName=probe&\
                 PolicyDocument={}",
                form_encode(role),
                form_encode(document)
            ),
        );
        assert_eq!(
            embedded.status, 200,
            "case {case:?}: PutRolePolicy({role}) must answer 200, got {embedded:?}"
        );
    }

    let assumed = query(
        case,
        port,
        "sts",
        &format!(
            "Action=AssumeRole&Version=2011-06-15&RoleArn={}&RoleSessionName={SESSION}",
            form_encode(&role_arn)
        ),
    );
    assert_eq!(
        assumed.status, 200,
        "case {case:?}: AssumeRole({role_arn}) must answer 200, got {assumed:?}"
    );
    element(case, &assumed.body, "AccessKeyId")
}

/// The emulator the rows drive — DynamoDB, IAM and STS over ONE engine — served on a
/// loopback port of its own, whose number is answered.
async fn served_port() -> u16 {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Unsigned requests: the `Authorization` header still names the Region and the access key
    // id, which is what the service reads the caller's identity from.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    let (endpoint, _served) = emulator_server::serve_on_ephemeral_port(router).await;
    endpoint
        .rsplit(':')
        .next()
        .and_then(|port| port.parse().ok())
        .unwrap_or_else(|| panic!("{endpoint} names no port"))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_reply_announces_the_media_type_its_request_named_the_api_under() {
    let port = served_port().await;
    tokio::task::spawn_blocking(move || drive(port))
        .await
        .expect("the exchanges run to completion");
}

/// Every row, driven over the wire of the endpoint served on `port`.
fn drive(port: u16) {
    let granted = role_session("the setup", port, "ddb-media-none", None);
    let explicitly_denied = role_session(
        "the setup",
        port,
        "ddb-media-denied",
        Some(
            &json!({
                "Version": POLICY_VERSION,
                "Statement": [
                    {"Effect": "Allow", "Action": "dynamodb:*", "Resource": "*"},
                    {"Effect": "Deny", "Action": "dynamodb:ListTables", "Resource": "*"},
                ],
            })
            .to_string(),
        ),
    );

    let rows = [
        Row {
            case: "an identity-based implicit deny of a listing",
            announced: PROTOCOL_MEDIA_TYPE,
            target: "DynamoDB_20120810.ListTables",
            body: json!({"Limit": 1}),
            caller: Caller::Granted,
            status: 400,
            media_type: PROTOCOL_MEDIA_TYPE,
            class: Some("AccessDeniedException"),
            member: None,
        },
        Row {
            case: "the same implicit deny, asked under the plain JSON media type",
            announced: PLAIN_JSON_MEDIA_TYPE,
            target: "DynamoDB_20120810.ListTables",
            body: json!({"Limit": 1}),
            caller: Caller::Granted,
            status: 400,
            media_type: PLAIN_JSON_MEDIA_TYPE,
            class: Some("AccessDeniedException"),
            member: None,
        },
        Row {
            case: "an explicit deny over a wildcard allow",
            announced: PROTOCOL_MEDIA_TYPE,
            target: "DynamoDB_20120810.ListTables",
            body: json!({}),
            caller: Caller::ExplicitlyDenied,
            status: 400,
            media_type: PROTOCOL_MEDIA_TYPE,
            class: Some("AccessDeniedException"),
            member: None,
        },
        Row {
            // `ListTablesInputLimit` is `@range` 1..100, so 0 is outside it.
            case: "a page size outside the range the model declares",
            announced: PROTOCOL_MEDIA_TYPE,
            target: "DynamoDB_20120810.ListTables",
            body: json!({"Limit": 0}),
            caller: Caller::Account,
            status: 400,
            media_type: PROTOCOL_MEDIA_TYPE,
            class: Some("ValidationException"),
            member: None,
        },
        Row {
            // `DescribeTable` lists `ResourceNotFoundException`, whose status defaults to 400.
            case: "a table this account does not hold",
            announced: PROTOCOL_MEDIA_TYPE,
            target: "DynamoDB_20120810.DescribeTable",
            body: json!({"TableName": "no-such-table"}),
            caller: Caller::Account,
            status: 400,
            media_type: PROTOCOL_MEDIA_TYPE,
            class: Some("ResourceNotFoundException"),
            member: None,
        },
        Row {
            case: "a target this endpoint binds no operation for",
            announced: PROTOCOL_MEDIA_TYPE,
            target: "DynamoDB_20120810.ListNothingAtAll",
            body: json!({}),
            caller: Caller::Account,
            status: 400,
            media_type: PROTOCOL_MEDIA_TYPE,
            class: Some("UnknownOperationException"),
            member: None,
        },
        Row {
            case: "the same unbound target, asked under the plain JSON media type",
            announced: PLAIN_JSON_MEDIA_TYPE,
            target: "DynamoDB_20120810.ListNothingAtAll",
            body: json!({}),
            caller: Caller::Account,
            status: 400,
            media_type: PLAIN_JSON_MEDIA_TYPE,
            class: Some("UnknownOperationException"),
            member: None,
        },
        Row {
            case: "a served listing",
            announced: PROTOCOL_MEDIA_TYPE,
            target: "DynamoDB_20120810.ListTables",
            body: json!({}),
            caller: Caller::Account,
            status: 200,
            media_type: PROTOCOL_MEDIA_TYPE,
            class: None,
            // `ListTablesOutput$TableNames` — the member a served listing answers with.
            member: Some("TableNames"),
        },
        Row {
            case: "a served listing, asked under the plain JSON media type",
            announced: PLAIN_JSON_MEDIA_TYPE,
            target: "DynamoDB_20120810.ListTables",
            body: json!({}),
            caller: Caller::Account,
            status: 200,
            media_type: PLAIN_JSON_MEDIA_TYPE,
            class: None,
            member: Some("TableNames"),
        },
    ];

    for row in &rows {
        let case = row.case;
        let key = match row.caller {
            Caller::Account => AMBIENT_KEY,
            Caller::Granted => granted.as_str(),
            Caller::ExplicitlyDenied => explicitly_denied.as_str(),
        };
        let reply = exchange(
            case,
            port,
            &[
                ("Content-Type", row.announced),
                ("X-Amz-Target", row.target),
                ("Authorization", &authorization(key, "dynamodb")),
            ],
            &row.body.to_string(),
        );

        assert_eq!(
            reply.status, row.status,
            "case {case:?}: expected HTTP {}, got {reply:?}",
            row.status
        );
        let announced = reply.header("content-type");
        assert_eq!(
            announced,
            Some(row.media_type),
            "case {case:?}: the reply must announce {:?}, got {announced:?} in {reply:?}",
            row.media_type
        );
        let query_error = reply.header(QUERY_ERROR_HEADER);
        assert!(
            query_error.is_none(),
            "case {case:?}: this wire carries no {QUERY_ERROR_HEADER}, got {query_error:?}"
        );
        let stated = reply.class();
        assert_eq!(
            stated.as_deref(),
            row.class,
            "case {case:?}: expected the class {:?} in the body's __type, got {reply:?}",
            row.class
        );
        if let Some(member) = row.member {
            let document: Value = serde_json::from_str(&reply.body).unwrap_or(Value::Null);
            let served = document.get(member);
            assert!(
                served.is_some_and(Value::is_array),
                "case {case:?}: a served reply must carry {member} as a list, got {reply:?}"
            );
        }
    }
}

// SPDX-License-Identifier: CC0-1.0

//! Two Application Load Balancer attributes that decide a request on its way to the target,
//! end to end: `routing.http.desync_mitigation_mode` and
//! `routing.http.xff_header_processing.mode`.
//!
//! The rules under test:
//!
//! 1. **A load balancer judges the FRAMING of every request it reads — the headers that say
//!    where the body ends — and refuses, with its own `400` and before any rule of its
//!    listener is evaluated or any target is reached, exactly the framings its
//!    `routing.http.desync_mitigation_mode` refuses: `monitor` refuses none, `defensive`
//!    refuses a framing two recipients can read two different messages out of, and `strictest`
//!    refuses every framing that is not written as HTTP/1.1 states it. A compliant request is
//!    served under every mode.**
//! 2. **Before a request reaches a target, a load balancer treats its `X-Forwarded-For` as its
//!    `routing.http.xff_header_processing.mode` says: `append` adds the address the request
//!    came from to the list the client supplied, `preserve` hands the client's own header on
//!    unchanged, and `remove` strips it. Nothing else about the request changes.**
//!
//! `LoadBalancerAttribute$Key` documents both: "`routing.http.desync_mitigation_mode` -
//! Determines how the load balancer handles requests that might pose a security risk to your
//! application. The possible values are monitor, defensive, and strictest. The default is
//! defensive", and "`routing.http.xff_header_processing.mode` - Enables you to modify,
//! preserve, or remove the X-Forwarded-For header in the HTTP request before the Application
//! Load Balancer sends the request to the target. The possible values are append, preserve,
//! and remove. The default is append."
//!
//! The rows span what each rule generalises over: for the first, each framing class (a body
//! framed by one length, no body at all, a length beside a transfer coding, and the same
//! length written twice) against each of the three modes, over a REAL connection to the
//! address the load balancer publishes — which is the only place a client's own framing
//! exists, because the leg that address hands the data plane re-frames the body itself; for
//! the second, each mode against each form the client's own header takes (none, one address, a
//! list, and the header sent twice).
//!
//! Every attribute value a case is driven against is written through
//! `ModifyLoadBalancerAttributes` and read back through `DescribeLoadBalancerAttributes`
//! first, so no case rests on a value a test believes it wrote. What the target saw is
//! reported by the TARGET: a Lambda function created through Lambda's own API, run by the
//! Lambda emulator mounted on this suite's engine, which echoes the headers of the event it
//! was handed — the fixture its sibling `crate::alb_drop_invalid_header_fields` drives.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::alb_data_plane::{create_load_balancer, send, settle};
use crate::alb_drop_invalid_header_fields::{emulator, lambda, package, HANDLER, ROLE, RUNTIME};
use crate::provisioning_loop::{action, attribute_sequence, call, p};

/// The two attributes under test.
const DESYNC_ATTRIBUTE: &str = "routing.http.desync_mitigation_mode";
const XFF_ATTRIBUTE: &str = "routing.http.xff_header_processing.mode";

/// The header the second rule is about.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// The address a client on the machine this emulator runs on reaches a load balancer's own
/// socket from, which is what an `append` adds to the list the client supplied.
const CONNECTION_ADDRESS: &str = "127.0.0.1";

/// How long one dial and one exchange on the published address may take. Generous on purpose:
/// everything waited for happens in this process, so a loaded host does not decide a verdict.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);

/// The status a load balancer answers a request whose framing its mode refuses.
const BAD_REQUEST: u16 = 400;

/// Everything one case is driven against: the router every call goes through, and the load
/// balancer's ARN and published address.
struct Deployed {
    router: MultiServiceRouter,
    lb_arn: String,
    dns_name: String,
}

/// A load balancer with one HTTP:80 listener forwarding to a `lambda` target group holding a
/// function that echoes the headers of the event it is handed — every resource through the
/// owning API.
async fn deployed(name: &str, function: &str) -> Deployed {
    let (router, engine) = emulator();

    let (status, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": function,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": { "ZipFile": aws_smithy_types::base64::encode(package()) },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected 201, got {status} {created}"
    );
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateFunction: no FunctionArn in {created}"))
        .to_string();
    // The activation span Lambda owes the function, placed rather than slept for.
    {
        let state = engine
            .state::<service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState>();
        let mut store = state.lock();
        let past = SystemTime::now() - Duration::from_secs(1);
        for record in store.functions.values_mut() {
            record.active_due = past;
        }
    }

    let (lb_arn, dns_name) = create_load_balancer(&router, name).await;

    let mut form = action("CreateTargetGroup");
    form.push(p("Name", &format!("{name}-tg")));
    form.push(p("TargetType", "lambda"));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateTargetGroup: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let tg_arn = reply
        .text("TargetGroupArn")
        .unwrap_or_else(|| panic!("setup CreateTargetGroup: no TargetGroupArn in {}", reply.body));

    let mut form = action("RegisterTargets");
    form.push(p("TargetGroupArn", &tg_arn));
    form.push(p("Targets.member.1.Id", &function_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup RegisterTargets: expected 200, got {} body {}",
        reply.status, reply.body
    );

    let (status, granted) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        json!({
            "StatementId": "elbv2-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "elasticloadbalancing.amazonaws.com",
            "SourceArn": tg_arn,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission: expected 201, got {status} {granted}"
    );

    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", &tg_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );

    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    Deployed {
        router,
        lb_arn,
        dns_name,
    }
}

/// What `DescribeLoadBalancerAttributes` reports one attribute at.
async fn described(deployed: &Deployed, case: &str, key: &str) -> String {
    let mut form = action("DescribeLoadBalancerAttributes");
    form.push(p("LoadBalancerArn", &deployed.lb_arn));
    let reply = call(&deployed.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected DescribeLoadBalancerAttributes to answer 200, got {} body {}",
        reply.status, reply.body
    );
    attribute_sequence(&reply.body)
        .into_iter()
        .find(|(held, _)| held == key)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} in {}", reply.body))
}

/// Write one attribute, which must be accepted, and read it back so every case is driven
/// against a value the control plane REPORTS rather than one a test believes it wrote.
async fn written(deployed: &Deployed, case: &str, key: &str, value: &str) {
    let mut form = action("ModifyLoadBalancerAttributes");
    form.push(p("LoadBalancerArn", &deployed.lb_arn));
    form.push(p("Attributes.member.1.Key", key));
    form.push(p("Attributes.member.1.Value", value));
    let reply = call(&deployed.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected ModifyLoadBalancerAttributes to answer 200, got {} body {}",
        reply.status, reply.body
    );
    let reported = described(deployed, case, key).await;
    assert_eq!(
        reported, value,
        "case {case:?}: expected {key} to be reported at {value:?}, got {reported:?}"
    );
}

/// Dial the published address and exchange one request written BYTE FOR BYTE, as a client that
/// frames its own request does. Answers the status and the whole answer as it came off the
/// connection.
///
/// Every other way this crate reaches a data plane hands it a request some HTTP client has
/// already framed; the framing a CLIENT wrote exists only on the connection it dialled, so the
/// framing rows are driven here.
///
/// Only the PORT is taken from the published authority: the core binds its listeners on
/// loopback (`EMULATOR_BIND_ADDRESS`, `127.0.0.1` by default), so the dial is to this machine.
async fn raw_exchange(case: &str, published: &str, request: &str) -> (u16, String) {
    let port = published
        .rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the published address {published:?} carries no port to dial")
        });
    let mut stream = tokio::time::timeout(
        EXCHANGE_TIMEOUT,
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .unwrap_or_else(|_| panic!("case {case:?}: {published} did not accept a connection"))
    .unwrap_or_else(|error| panic!("case {case:?}: {published} refused a connection: {error}"));
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answer = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the published address did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    let raw = String::from_utf8_lossy(&answer).to_string();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: no status line in {raw:?}"));
    (status, raw)
}

/// One framing a client can write a request with: the header lines that frame it, the body
/// bytes that follow, and which modes refuse it.
struct Framing {
    case: &'static str,
    headers: &'static str,
    body: &'static str,
    /// Written as HTTP/1.1 states it, so refused by no mode.
    compliant: bool,
    /// Readable two ways, so refused by `defensive` as well as by `strictest`.
    ambiguous: bool,
}

/// The framings each mode is driven against. Every one of them is a message this address can
/// read, so what decides the answer is the attribute and nothing else.
fn framings() -> Vec<Framing> {
    vec![
        Framing {
            case: "a body framed by one length",
            headers: "Content-Length: 2\r\n",
            body: "hi",
            compliant: true,
            ambiguous: false,
        },
        Framing {
            case: "no body at all",
            headers: "",
            body: "",
            compliant: true,
            ambiguous: false,
        },
        Framing {
            // RFC 7230 §3.3.3: the transfer coding overrides the length for one recipient and
            // not for another, which is the framing a smuggled request travels in. Both
            // readings consume the same bytes here, so what decides the answer is the mode.
            case: "a length beside a transfer coding",
            headers: "Content-Length: 5\r\nTransfer-Encoding: chunked\r\n",
            body: "0\r\n\r\n",
            compliant: false,
            ambiguous: true,
        },
        Framing {
            // RFC 7230 §3.3.2: one length written twice is a length every recipient reads the
            // same way, in a form the grammar does not admit.
            case: "the same length written twice",
            headers: "Content-Length: 2\r\nContent-Length: 2\r\n",
            body: "hi",
            compliant: false,
            ambiguous: false,
        },
    ]
}

/// RULE 1: the framings a load balancer refuses itself are exactly the ones its
/// `routing.http.desync_mitigation_mode` refuses, and a refused request reaches no target.
#[tokio::test]
async fn a_load_balancer_refuses_the_framings_its_desync_mitigation_mode_refuses() {
    let deployed = deployed("desync-alb", "alb-desync-echo").await;

    // The three modes the attribute's domain admits, with what each refuses: `monitor` refuses
    // nothing, `defensive` refuses what reads two ways, `strictest` refuses everything not
    // written as the grammar has it.
    type Mode = (&'static str, fn(&Framing) -> bool);
    let modes: [Mode; 3] = [
        ("monitor", |_| false),
        ("defensive", |framing: &Framing| framing.ambiguous),
        ("strictest", |framing: &Framing| !framing.compliant),
    ];
    for (mode, refuses) in modes {
        written(&deployed, mode, DESYNC_ATTRIBUTE, mode).await;
        for framing in framings() {
            let case = format!("{mode:?} with {:?}", framing.case);
            let request = format!(
                "POST /desync HTTP/1.1\r\nHost: {}\r\n{}Connection: close\r\n\r\n{}",
                deployed.dns_name, framing.headers, framing.body
            );
            let (status, raw) = raw_exchange(&case, &deployed.dns_name, &request).await;
            let expected = if refuses(&framing) { BAD_REQUEST } else { 200 };
            assert_eq!(
                status, expected,
                "case {case}: expected HTTP {expected}, got {status} in {raw:?}"
            );
            // A refused request reaches NO target: the answer is the load balancer's own, so
            // it carries nothing of the echo the function answers with.
            let echoed = raw.contains(FORWARDED_FOR);
            assert_eq!(
                echoed,
                expected == 200,
                "case {case}: expected the target to have been reached={}, got {raw:?}",
                expected == 200
            );
        }
    }
}

/// One form the client's own `X-Forwarded-For` takes, and what each mode must hand the target
/// for it.
struct Supplied {
    case: &'static str,
    /// The header as the client sends it, which may be more than one field.
    sent: &'static [&'static str],
    /// What `append` must deliver: the list the client supplied, extended with the address the
    /// request came from.
    appended: &'static str,
    /// What `preserve` must deliver, which is the client's own header.
    preserved: Option<&'static str>,
}

/// The forms the client's own header takes, spanning what the rule generalises over: nothing
/// supplied, one address, a list of addresses, and the header sent twice — which is one list.
fn supplied() -> Vec<Supplied> {
    vec![
        Supplied {
            case: "nothing supplied",
            sent: &[],
            appended: CONNECTION_ADDRESS,
            preserved: None,
        },
        Supplied {
            case: "one supplied address",
            sent: &["203.0.113.9"],
            appended: "203.0.113.9, 127.0.0.1",
            preserved: Some("203.0.113.9"),
        },
        Supplied {
            case: "a supplied list",
            sent: &["203.0.113.9, 198.51.100.4"],
            appended: "203.0.113.9, 198.51.100.4, 127.0.0.1",
            preserved: Some("203.0.113.9, 198.51.100.4"),
        },
        Supplied {
            case: "the header sent twice",
            sent: &["203.0.113.9", "198.51.100.4"],
            appended: "203.0.113.9, 198.51.100.4, 127.0.0.1",
            // `preserve` changes nothing, so the target is handed the two fields the client
            // sent — and a `lambda` target group that does not exchange ARRAYS reports one
            // value per header name ("lambda.multi_value_headers.enabled", default false), so
            // what the echo shows for them is the last field. `append` reports one list
            // because the load balancer itself joined them into one field.
            preserved: Some("198.51.100.4"),
        },
    ]
}

/// The headers the TARGET was handed for one request, as the function echoed them.
async fn seen_by_the_target(
    deployed: &Deployed,
    case: &str,
    sent: &[&str],
) -> serde_json::Map<String, Value> {
    let headers: Vec<(&str, &str)> = sent
        .iter()
        .map(|value| (FORWARDED_FOR, *value))
        .chain(std::iter::once(("x-echo-kept", "untouched")))
        .collect();
    let answered = send(
        &deployed.router,
        "GET",
        &deployed.dns_name,
        "/forwarded",
        &headers,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the target's HTTP 200, got {} with body {:?}",
        answered.status, answered.body
    );
    let echoed: Value = serde_json::from_str(&answered.body).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the target's echo is not JSON ({error}) in {:?}",
            answered.body
        )
    });
    match echoed {
        Value::Object(headers) => headers,
        other => panic!("case {case:?}: the target's echo is no object, got {other}"),
    }
}

/// RULE 2: the `X-Forwarded-For` a target receives is the one the load balancer's
/// `routing.http.xff_header_processing.mode` gives it, and nothing else of the request changes.
#[tokio::test]
async fn a_target_receives_the_forwarded_for_header_its_load_balancers_mode_gives_it() {
    let deployed = deployed("xff-alb", "alb-xff-echo").await;

    for mode in ["append", "preserve", "remove"] {
        written(&deployed, mode, XFF_ATTRIBUTE, mode).await;
        for row in supplied() {
            let case = format!("{mode:?} with {:?}", row.case);
            let seen = seen_by_the_target(&deployed, &case, row.sent).await;
            let delivered = seen.get(FORWARDED_FOR).and_then(Value::as_str);
            let expected = match mode {
                "append" => Some(row.appended),
                "preserve" => row.preserved,
                _ => None,
            };
            assert_eq!(
                delivered, expected,
                "case {case}: expected {FORWARDED_FOR} at {expected:?}, got {delivered:?} in \
                 {seen:?}"
            );
            // Whatever the mode does to that one header, every other header of the same
            // request reaches the target untouched.
            let kept = seen.get("x-echo-kept").and_then(Value::as_str);
            assert_eq!(
                kept,
                Some("untouched"),
                "case {case}: every other header reaches the target, expected \"untouched\", \
                 got {kept:?} in {seen:?}"
            );
        }
    }
}

/// The address the client supplied is never what a load balancer in `append` mode adds: the
/// entry it writes is the address the request came FROM, so a client that claims another
/// address is not believed. Driven with nothing written at all, so it also pins the value the
/// attribute's own documentation carries it at.
#[tokio::test]
async fn an_appended_entry_is_the_address_the_request_came_from() {
    let deployed = deployed("xff-claim-alb", "alb-xff-claim-echo").await;
    let carried = described(&deployed, "the attribute as it is carried", XFF_ATTRIBUTE).await;
    assert_eq!(
        carried, "append",
        "case the attribute as it is carried: expected {XFF_ATTRIBUTE} at \"append\", got \
         {carried:?}"
    );

    // A client that claims to be a whole chain of addresses: the load balancer keeps the claim
    // and adds the address it actually received the request from, as the LAST entry.
    let claimed = "10.0.0.1, 10.0.0.2";
    let seen = seen_by_the_target(&deployed, "a claimed chain", &[claimed]).await;
    let delivered = seen
        .get(FORWARDED_FOR)
        .and_then(Value::as_str)
        .unwrap_or_default();
    let last = delivered.rsplit(',').next().map(str::trim);
    assert_eq!(
        last,
        Some(CONNECTION_ADDRESS),
        "case a claimed chain: expected the last entry to be the address the request came \
         from, got {delivered:?}"
    );
    assert!(
        delivered.starts_with(claimed),
        "case a claimed chain: expected the client's own claim to be kept ahead of it, got \
         {delivered:?}"
    );
}

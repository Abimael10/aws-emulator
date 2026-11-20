// SPDX-License-Identifier: CC0-1.0

//! One Application Load Balancer that both FORWARDS to a `lambda` target group and GATES a
//! second path on a `source-ip` condition, driven over real connections to the address it
//! published.
//!
//! The rule under test: **the only address either half of the request path has for a client is
//! the address the CONNECTION came from — the `X-Forwarded-For` a load balancer hands a
//! `lambda` target ends with that address, behind whatever list the client supplied, and a
//! `source-ip` condition is met exactly when that same address lies in one of the CIDR blocks
//! its `SourceIpConfig.Values` names, never because of an `X-Forwarded-For` the client
//! supplied.**
//!
//! Grounded in the model: `SourceIpConditionConfig$Values` is "the source IP addresses, in
//! CIDR format. … If you specify multiple addresses, the condition is satisfied if the source
//! IP address of the request matches one of the CIDR blocks. This condition is not satisfied by
//! the addresses in the X-Forwarded-For header"; `RuleCondition$SourceIpConfig` is
//! "information for a source IP condition. Specify only when `Field` is `source-ip`";
//! `LoadBalancer$DNSName` is "the public DNS name of the load balancer", the address a client
//! then dials; `TargetGroup$TargetType` gives `lambda` as "register a single Lambda function as
//! a target"; and `LoadBalancerAttribute$Key` carries the header's own switch at its documented
//! default, "`routing.http.xff_header_processing.mode` … The default is append."
//!
//! What this suite drives that its siblings do not: the two halves of that one reading, on ONE
//! deployment and over the SAME connections. `crate::alb_data_plane` judges a `source-ip`
//! condition on a real connection but in front of fixed responses alone, and
//! `crate::alb_desync_mitigation_and_forwarded_for` reads what a `lambda` target was handed
//! through a request dispatched in process — so nothing yet pins that the address a target is
//! TOLD the request came from and the address the gate in front of it JUDGES are the same one,
//! which is what makes a CIDR allow-list in front of a Lambda-backed path mean anything.
//!
//! The rows span what the rule generalises over: a block holding exactly the connecting address
//! and a block holding it with room to spare, blocks of the documentation ranges that hold it
//! nowhere (RFC 5737 reserves 192.0.2.0/24 and 198.51.100.0/24, so no client of this emulator
//! connects from either), one block of several, and — against both a matching and a
//! non-matching block — every form the client's own claim takes: none at all, an address inside
//! the block, the connecting address itself, and a chain sent as two header fields, which is
//! one list.
//!
//! Nothing is staged: the load balancer, its `lambda` target group, its listener and its three
//! rules are written through the owning APIs, the function is a real deployment package created
//! through Lambda's own API and run by the Lambda emulator mounted on this suite's engine, the
//! block each row is judged against is written through `ModifyRule` and read back through
//! `DescribeRules` first, and every request is a real HTTP/1.1 exchange on a connection dialled
//! to the published authority.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, settle};
use crate::alb_desync_mitigation_and_forwarded_for::{raw_exchange, CONNECTION_ADDRESS};
use crate::alb_drop_invalid_header_fields::{emulator, lambda, package, HANDLER, ROLE, RUNTIME};
use crate::provisioning_loop::{action, call, members, p, Reply};
use crate::target_group_attributes_and_modification::unescaped;

/// The function the priority-5 rule forwards to.
const FUNCTION: &str = "alb-source-ip-gate-echo";

/// The two paths the deployment serves, under one prefix as a client's own paths are: the one
/// the `lambda` rule forwards, and the one the two `source-ip`-gated rules are written for.
const ECHO_PATH: &str = "/gate/echo";
const GATED_PATH: &str = "/gate/src";

/// The bodies the three fixed responses of the deployment answer with: the gated rule's, the
/// fall-through behind it, and the listener's own default for a path no rule matches.
const BY_SOURCE_IP: &str = "BY-SOURCE-IP";
const FELL_THROUGH: &str = "FELL-THROUGH";
const DEFAULT_RULE: &str = "DEFAULT-RULE";

/// The header the forwarded half of the rule is about, as the target's echo keys it.
const FORWARDED_FOR: &str = "x-forwarded-for";

/// How the entries of that header are separated, as the header's own grammar has it.
const ENTRY_SEPARATOR: &str = ", ";

/// Everything one case is driven against.
struct Deployed {
    router: MultiServiceRouter,
    /// The address the control plane published, which every exchange below DIALS.
    published: String,
    listener_arn: String,
    /// The ARN of the gated rule, which each row re-points through `ModifyRule`.
    rule_arn: String,
}

/// The fields of the `path-pattern` condition one rule carries.
fn path_pattern(pattern: &str) -> Vec<(String, String)> {
    vec![
        p("Conditions.member.1.Field", "path-pattern"),
        p("Conditions.member.1.Values.member.1", pattern),
    ]
}

/// The fields of a `fixed-response` action answering `body`.
fn fixed_response(body: &str) -> Vec<(String, String)> {
    vec![
        p("Actions.member.1.Type", "fixed-response"),
        p("Actions.member.1.FixedResponseConfig.StatusCode", "200"),
        p(
            "Actions.member.1.FixedResponseConfig.ContentType",
            "text/plain",
        ),
        p("Actions.member.1.FixedResponseConfig.MessageBody", body),
    ]
}

/// The gated rule's conditions: the path it is written for, and the `source-ip` block list
/// `values` names — the fields a `ModifyRule` of that rule repeats, so the call leaves its path
/// condition exactly as it was.
fn gated_conditions(values: &[&str]) -> Vec<(String, String)> {
    let mut fields = path_pattern(GATED_PATH);
    fields.push(p("Conditions.member.2.Field", "source-ip"));
    for (index, value) in values.iter().enumerate() {
        fields.push(p(
            &format!(
                "Conditions.member.2.SourceIpConfig.Values.member.{}",
                index + 1
            ),
            value,
        ));
    }
    fields
}

/// Create one rule at `priority` carrying `fields`, which must be accepted, and answer the ARN
/// the create minted.
async fn add_rule(
    router: &MultiServiceRouter,
    case: &str,
    listener_arn: &str,
    priority: i32,
    fields: &[(String, String)],
) -> String {
    let mut form = action("CreateRule");
    form.push(p("ListenerArn", listener_arn));
    form.push(p("Priority", &priority.to_string()));
    form.extend(fields.iter().cloned());
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateRule {case}: expected 200, got {} body {}",
        reply.status, reply.body
    );
    reply
        .text("RuleArn")
        .unwrap_or_else(|| panic!("setup CreateRule {case}: no RuleArn in {}", reply.body))
}

/// The deployment every case is driven against: a load balancer whose HTTP:80 listener answers
/// [`DEFAULT_RULE`] by default and carries three rules — a priority-5 `path-pattern` rule
/// forwarding [`ECHO_PATH`] to a `lambda` target group holding a function that echoes the
/// headers of the event it was handed, a priority-20 rule gating [`GATED_PATH`] on a
/// `source-ip` condition, and a priority-30 fall-through for the same path.
async fn deployed(name: &str, function: &str) -> Deployed {
    let (router, engine) = emulator();

    // The target: a real deployment package, created through Lambda's own API.
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
    // The activation span Lambda owes a freshly created function, placed rather than slept for.
    {
        let state = engine
            .state::<service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState>();
        let mut store = state.lock();
        let past = SystemTime::now() - Duration::from_secs(1);
        for record in store.functions.values_mut() {
            record.active_due = past;
        }
    }

    let (lb_arn, published) = create_load_balancer(&router, name).await;

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

    // The grant that admits the load balancer's invocation, through Lambda's own API.
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
    form.push(p("DefaultActions.member.1.Type", "fixed-response"));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.StatusCode",
        "404",
    ));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.ContentType",
        "text/plain",
    ));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.MessageBody",
        DEFAULT_RULE,
    ));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let listener_arn = reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body));

    let mut fields = path_pattern(ECHO_PATH);
    fields.push(p("Actions.member.1.Type", "forward"));
    fields.push(p("Actions.member.1.TargetGroupArn", &tg_arn));
    add_rule(
        &router,
        "the lambda forward at 5",
        &listener_arn,
        5,
        &fields,
    )
    .await;

    let mut fields = gated_conditions(&[&format!("{CONNECTION_ADDRESS}/32")]);
    fields.extend(fixed_response(BY_SOURCE_IP));
    let rule_arn = add_rule(&router, "the source-ip gate at 20", &listener_arn, 20, &fields).await;

    let mut fields = path_pattern(GATED_PATH);
    fields.extend(fixed_response(FELL_THROUGH));
    add_rule(
        &router,
        "the fall-through at 30",
        &listener_arn,
        30,
        &fields,
    )
    .await;

    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    Deployed {
        router,
        published,
        listener_arn,
        rule_arn,
    }
}

/// Point the gated rule's `source-ip` condition at `values`, and answer what the call replied,
/// so a case can assert either the acceptance or the refusal.
async fn point_the_gate_at(deployed: &Deployed, values: &[&str]) -> Reply {
    let mut form = action("ModifyRule");
    form.push(p("RuleArn", &deployed.rule_arn));
    form.extend(gated_conditions(values));
    call(&deployed.router, &form).await
}

/// The gated rule as `DescribeRules` reports it, which is what a case reads a stored condition
/// back out of.
async fn described_gate(deployed: &Deployed) -> String {
    let mut form = action("DescribeRules");
    form.push(p("ListenerArn", &deployed.listener_arn));
    let reply = call(&deployed.router, &form).await;
    assert_eq!(
        reply.status, 200,
        "expected DescribeRules to answer 200, got {} body {}",
        reply.status, reply.body
    );
    members(&reply.body, "Rules")
        .into_iter()
        .find(|rule| rule.contains(&deployed.rule_arn))
        .unwrap_or_else(|| panic!("no rule {} in {}", deployed.rule_arn, reply.body))
}

/// One exchange on a real connection to the published address: `GET target`, carrying one
/// `X-Forwarded-For` field per entry of `forwarded`. Answers the status and the body, which is
/// what follows the head of the answer.
async fn exchange(
    deployed: &Deployed,
    case: &str,
    target: &str,
    forwarded: &[&str],
) -> (u16, String) {
    let mut request = format!("GET {target} HTTP/1.1\r\nHost: {}\r\n", deployed.published);
    for claimed in forwarded {
        request.push_str(&format!("X-Forwarded-For: {claimed}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    let (status, raw) = raw_exchange(case, &deployed.published, &request).await;
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no body: {raw:?}"));
    (status, body)
}

/// Which of the deployment's three bodies answered one exchange: exactly one of them.
fn served(case: &str, body: &str) -> String {
    let matched: Vec<&str> = [BY_SOURCE_IP, FELL_THROUGH, DEFAULT_RULE]
        .into_iter()
        .filter(|candidate| body.contains(candidate))
        .collect();
    assert_eq!(
        matched.len(),
        1,
        "case {case:?}: expected exactly one of the deployment's bodies, got {matched:?} in \
         {body:?}"
    );
    matched[0].to_string()
}

/// The headers the TARGET was handed for one exchange on [`ECHO_PATH`], as the function echoed
/// them.
async fn seen_by_the_target(
    deployed: &Deployed,
    case: &str,
    forwarded: &[&str],
) -> serde_json::Map<String, Value> {
    let (status, body) = exchange(deployed, case, ECHO_PATH, forwarded).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the lambda target's HTTP 200 on the published address, got \
         {status} body {body:?}"
    );
    let echoed: Value = serde_json::from_str(&body).unwrap_or_else(|error| {
        panic!("case {case:?}: the target's echo is not JSON ({error}) in {body:?}")
    });
    match echoed {
        Value::Object(headers) => headers,
        other => panic!("case {case:?}: the target's echo is no object, got {other}"),
    }
}

/// One row: the blocks the gate is pointed at, what the client claims about its own address,
/// which of the two bodies the gated path must then answer with, and the whole
/// `X-Forwarded-For` list the `lambda` target must be handed for the same claim.
struct Row {
    case: &'static str,
    blocks: &'static [&'static str],
    forwarded: &'static [&'static str],
    body: &'static str,
    delivered: &'static str,
}

/// The rows. RFC 5737 reserves 192.0.2.0/24 (TEST-NET-1) and 198.51.100.0/24 (TEST-NET-2) for
/// documentation, so no client of this emulator connects from either: a gate pointed at one of
/// them holds the connecting address nowhere, however the client writes that address into a
/// header of its own.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the connecting address as a /32, with no claim of the client's own",
            blocks: &["127.0.0.1/32"],
            forwarded: &[],
            body: BY_SOURCE_IP,
            delivered: "127.0.0.1",
        },
        Row {
            case: "a block that holds the connecting address with room to spare",
            blocks: &["127.0.0.0/8"],
            forwarded: &["192.0.2.7"],
            body: BY_SOURCE_IP,
            delivered: "192.0.2.7, 127.0.0.1",
        },
        Row {
            case: "one block of several holds the connecting address",
            blocks: &["192.0.2.0/24", "198.51.100.0/24", "127.0.0.1/32"],
            forwarded: &["198.51.100.4, 192.0.2.9"],
            body: BY_SOURCE_IP,
            delivered: "198.51.100.4, 192.0.2.9, 127.0.0.1",
        },
        Row {
            case: "a documentation block, with no claim of the client's own",
            blocks: &["192.0.2.0/24"],
            forwarded: &[],
            body: FELL_THROUGH,
            delivered: "127.0.0.1",
        },
        Row {
            case: "the client claims an address inside the block it is not connecting from",
            blocks: &["192.0.2.0/24"],
            forwarded: &["192.0.2.7"],
            body: FELL_THROUGH,
            delivered: "192.0.2.7, 127.0.0.1",
        },
        Row {
            case: "the client claims the very address it connects from",
            blocks: &["192.0.2.0/24"],
            forwarded: &["127.0.0.1"],
            body: FELL_THROUGH,
            delivered: "127.0.0.1, 127.0.0.1",
        },
        Row {
            case: "the client claims a chain inside that block, as two header fields",
            blocks: &["192.0.2.0/24"],
            forwarded: &["192.0.2.7, 198.51.100.4", "192.0.2.9"],
            body: FELL_THROUGH,
            delivered: "192.0.2.7, 198.51.100.4, 192.0.2.9, 127.0.0.1",
        },
        Row {
            case: "a block of several, none of which holds the connecting address",
            blocks: &["192.0.2.0/24", "198.51.100.0/24"],
            forwarded: &[],
            body: FELL_THROUGH,
            delivered: "127.0.0.1",
        },
        Row {
            case: "the /32 restored after all of them",
            blocks: &["127.0.0.1/32"],
            forwarded: &["192.0.2.7"],
            body: BY_SOURCE_IP,
            delivered: "192.0.2.7, 127.0.0.1",
        },
    ]
}

/// RULE: the address the connection came from is the ONE address the request path has for the
/// client — the `X-Forwarded-For` the `lambda` target is handed ends with it behind whatever
/// the client supplied, and the `source-ip` gate in front of the other path is met exactly
/// while one of its blocks holds it, never because of a header the client wrote.
#[tokio::test]
async fn the_connecting_address_is_what_the_target_is_told_and_what_the_source_ip_gate_judges() {
    let deployed = deployed("source-ip-gate-alb", FUNCTION).await;

    for row in rows() {
        // The blocks every row is judged against are the ones the control plane REPORTS, so no
        // row rests on a condition this suite believes it wrote.
        let reply = point_the_gate_at(&deployed, row.blocks).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: expected ModifyRule to answer 200, got {} body {}",
            row.case, reply.status, reply.body
        );
        let described = described_gate(&deployed).await;
        for block in row.blocks {
            assert!(
                described.contains(&format!("<member>{block}</member>")),
                "case {:?}: expected DescribeRules to echo the block {block:?}, got \
                 {described:?}",
                row.case
            );
        }

        // The forwarded half: the target is TOLD the request came from the address the
        // connection came from, as the last entry of the list it is handed.
        let seen = seen_by_the_target(&deployed, row.case, row.forwarded).await;
        let delivered = seen.get(FORWARDED_FOR).and_then(Value::as_str);
        assert_eq!(
            delivered,
            Some(row.delivered),
            "case {:?}: expected the target to be handed {FORWARDED_FOR} at {:?}, got \
             {delivered:?} in {seen:?}",
            row.case,
            row.delivered
        );
        let last = row.delivered.rsplit(ENTRY_SEPARATOR).next();
        assert_eq!(
            last,
            Some(CONNECTION_ADDRESS),
            "case {:?}: the entry the load balancer itself adds is the address the connection \
             came from, got {last:?}",
            row.case
        );

        // The gated half: the same connection, judged against the blocks just read back.
        let (status, body) = exchange(&deployed, row.case, GATED_PATH, row.forwarded).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected one of the two rules of the gated path to answer 200, got \
             {status} body {body:?}",
            row.case
        );
        let answered = served(row.case, &body);
        assert_eq!(
            answered, row.body,
            "case {:?}: expected the body {:?}, got {answered:?}",
            row.case, row.body
        );
    }
}

/// The refusal a `source-ip` condition value that names no address block earns, read from the
/// service's own one statement of it so the suite and the handler cannot drift on the text.
fn invalid_block_refusal(value: &str) -> String {
    service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::invalid_cidr_block_message(value)
}

/// The values a case drives the gate at that name no address block: the all-ones address no
/// connection comes from, an address carrying no prefix length at all, and one past the highest
/// IPv4 prefix length.
const NO_BLOCK: &[&str] = &["255.255.255.255/32", "10.0.0.1", "10.0.0.0/33"];

/// RULE (the same reading, under a refused write): a `ModifyRule` naming a value that is no
/// address block is refused HTTP 400 `ValidationError` naming the value, and changes nothing —
/// the stored condition reads back exactly as it was, the gate keeps answering by the block it
/// was already carrying, and the `lambda` path keeps serving.
#[tokio::test]
async fn a_refused_block_leaves_the_gate_and_the_lambda_path_serving_as_they_were() {
    let deployed = deployed("source-ip-gate-refusal-alb", "alb-source-ip-gate-refused").await;

    for value in NO_BLOCK {
        let before = described_gate(&deployed).await;
        let reply = point_the_gate_at(&deployed, &[value]).await;
        assert_eq!(
            reply.status, 400,
            "case {value:?}: expected ModifyRule to refuse with HTTP 400, got {} body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "ValidationError",
            "case {value:?}: expected the ValidationError code, got {:?} in {}",
            reply.error_code(),
            reply.body
        );
        assert_eq!(
            unescaped(&reply.text("Message").unwrap_or_default()),
            invalid_block_refusal(value),
            "case {value:?}: expected the refusal to name the value, got {:?}",
            reply.text("Message")
        );

        // Nothing moved: the stored condition, the gate's own answer, and the lambda path.
        let after = described_gate(&deployed).await;
        assert_eq!(
            after, before,
            "case {value:?}: a refused call must leave the rule as it was, got {after:?}"
        );
        let (status, body) = exchange(&deployed, value, GATED_PATH, &[]).await;
        assert_eq!(
            status, 200,
            "case {value:?}: expected the gated path to still answer 200, got {status} body \
             {body:?}"
        );
        let answered = served(value, &body);
        assert_eq!(
            answered, BY_SOURCE_IP,
            "case {value:?}: expected the stored block to still answer, got {answered:?}"
        );
        let seen = seen_by_the_target(&deployed, value, &[]).await;
        let delivered = seen.get(FORWARDED_FOR).and_then(Value::as_str);
        assert_eq!(
            delivered,
            Some(CONNECTION_ADDRESS),
            "case {value:?}: expected the lambda path to keep serving and the target to keep \
             being told the connecting address, got {delivered:?} in {seen:?}"
        );
    }
}

/// A path no rule of the deployment matches is answered by the listener's own default actions,
/// over the same connection — so a row of the table above that was answered by the default
/// rather than by one of its two rules could not have passed unnoticed.
#[tokio::test]
async fn a_path_no_rule_matches_is_answered_by_the_listeners_default() {
    let deployed = deployed("source-ip-gate-default-alb", "alb-source-ip-gate-default").await;
    let case = "a path no rule of the listener is written for";
    let (status, body) = exchange(&deployed, case, "/gate/elsewhere", &[]).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected the listener's default status, got {status} body {body:?}"
    );
    let answered = served(case, &body);
    assert_eq!(
        answered, DEFAULT_RULE,
        "case {case:?}: expected the listener's own default body, got {answered:?}"
    );
}

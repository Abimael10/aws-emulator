// SPDX-License-Identifier: CC0-1.0

//! The RDS PROXY WIRING a RUNNING handler reads through the AWS SDK this runtime supplies, end
//! to end over the wire: Lambda and Amazon RDS on ONE engine, a `python3.x` function whose code
//! does `import boto3` and resolves the proxy it would connect through, and the SAME reads made
//! directly against that service for comparison.
//!
//! RULE: a read of a resource's own wiring that the called service models and serves is
//! DISPATCHED by the SDK a managed runtime supplies, and what the handler reads is that
//! service's own answer — every output member the direct caller gets, under the same names and
//! the same nesting, or the service's own code, sentence and status for a refused call. An
//! application that connects to its database through RDS Proxy confirms that wiring before it
//! dials — the secret each `Auth` entry names as the credential the proxy logs in with, the
//! health of each registered target, the target group behind them and the endpoints the proxy
//! answers on — so the whole proxy read surface is part of that rule, not one read of it.
//!
//! The rows differ along what the rule generalises over: WHICH proxy read the call names (the
//! proxy, its targets, its target group, its endpoints), how it is narrowed (not at all, to one
//! proxy, to a page), whether the proxy named carries an `Auth` entry at all, whether the call is
//! served or refused, and whether the operation is one this runtime's SDK binds. What a served
//! answer must carry and what a refusal must say are not spelled out here: the same call is made
//! DIRECTLY against the service and what the handler read is held against what that call
//! answered, so a runtime that answered something of its own fails here.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{
    await_function_active, call, router_on, CLOCK_BUDGET, LOOK_AGAIN,
};
// The one `awsQuery` caller and element reader this crate's suites hold Amazon RDS's own answers
// against, exported by the suite that first needed them rather than copied here.
use crate::handler_rds_catalogue_calls::{element, rds};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than copied here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// An IAM role ARN of the form `CreateDBProxy$RoleArn` takes: the role the proxy reads its
/// credentials with.
const PROXY_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/rds-proxy";

/// A subnet id of the published form (`subnet-` plus eight hexadecimal characters).
const SUBNET_ID: &str = "subnet-0a1b2c3d";

/// The secret one proxy's `Auth` entry names as the credential it logs in with
/// (`UserAuthConfig$SecretArn`). Nothing holds it: what the rows assert is that the handler read
/// the member the SERVICE reports, whatever its value.
const SECRET_ARN: &str = "arn:aws:secretsmanager:us-east-1:000000000000:secret:emu-proxy-cred";

/// The database the proxy's target group holds the registration for.
const INSTANCE: &str = "emu-proxy-wiring-instance";

/// The proxy whose `Auth` names a secret — the shape an application that authenticates with
/// Secrets Manager credentials reads.
const SECRET_PROXY: &str = "emu-proxy-wiring-secrets";

/// A second proxy, of another engine family, whose `Auth` names nothing: a proxy authenticating
/// end to end with IAM reads no secret, so its report carries no `Auth` entry to resolve.
const IAM_PROXY: &str = "emu-proxy-wiring-iam";

/// A proxy name that resolves nothing, satisfying `DBProxyName`'s published pattern.
const ABSENT_PROXY: &str = "emu-proxy-wiring-absent";

/// Lambda and Amazon RDS over ONE shared engine, as `core/router/src/lib.rs` mounts them: the
/// handler's SDK call is dispatched back into this same router, so the service it reaches is
/// the service this suite calls directly.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_rds_amazonrdsv19::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// One `awsQuery` call made directly against Amazon RDS that must be SERVED, as its body.
async fn rds_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = rds(router, action, params).await;
    assert_eq!(
        status, 200,
        "setup {case:?}: expected {action} to be served, got {status} {body}"
    );
    body
}

/// The handler every row runs.
///
/// `wiring` is the shape an application takes before it connects: it dereferences the proxy's
/// `Auth[0].SecretArn` and its target's `TargetHealth.State` with no guard at all, so a member
/// the SDK did not hand over raises and the invocation carries the function-error signal. Every
/// other row names a client method, the parameters to call it with, the output member the
/// entries arrive under, and the member paths to read off the first entry.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def _at(value, path):
    """The value one path of member names and list positions reaches, or None."""
    for step in path.split("/"):
        if isinstance(value, list):
            if not step.isdigit() or int(step) >= len(value):
                return None
            value = value[int(step)]
        elif isinstance(value, dict):
            if step not in value:
                return None
            value = value[step]
        else:
            return None
    return value


def lambda_handler(event, context):
    client = boto3.client("rds")
    if event["method"] == "wiring":
        proxy = client.describe_db_proxies(DBProxyName=event["proxy"])["DBProxies"][0]
        targets = client.describe_db_proxy_targets(DBProxyName=event["proxy"])["Targets"]
        return {
            "ok": True,
            "secret_arn": proxy["Auth"][0]["SecretArn"],
            "role_arn": proxy["RoleArn"],
            "endpoint": proxy["Endpoint"],
            "target_state": targets[0]["TargetHealth"]["State"],
            "target_type": targets[0]["Type"],
            "rds_resource_id": targets[0]["RdsResourceId"],
        }
    method = getattr(client, event["method"], None)
    if method is None:
        return {"ok": False, "no_such_method": event["method"]}
    try:
        answered = method(**event["parameters"])
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except botocore.exceptions.BotoCoreError as error:
        return {"ok": False, "not_made": str(error)}
    rows = answered.get(event["items"], [])
    return {
        "ok": True,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "members": sorted(key for key in answered if key != "ResponseMetadata"),
        "count": len(rows),
        "names": [row.get(event["name"]) for row in rows],
        "read": {path: _at(rows[0], path) for path in event["read"]} if rows else {},
    }
"#;

/// `Invoke` the function with `event`, as the payload the run produced — which must be a run
/// that did NOT fault: the function-error signal is exactly what a handler whose SDK call raised
/// carries, and it is what this suite exists to rule out.
async fn invoke(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let answered = call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: expected no function-error signal, got {:?} with {}",
        answered.header("x-amz-function-error"),
        answered.text()
    );
    serde_json::from_str(&answered.text()).unwrap_or(Value::Null)
}

/// One member of a payload as plain text, so a value the SDK handed over as a flag or a number
/// is held against the TEXT the same member carries in the service's own answer.
fn plain(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        None | Some(Value::Null) => String::new(),
        Some(other) => other.to_string(),
    }
}

/// One member path a row read off the first entry, as plain text.
fn read_at(payload: &Value, path: &str) -> String {
    plain(payload.get("read").and_then(|read| read.get(path)))
}

/// The text of an element of an `awsQuery` answer, as the characters it stands for.
fn unescaped(text: &str) -> String {
    quick_xml::escape::unescape(text)
        .unwrap_or_else(|error| panic!("the answer's text must unescape as XML: {error}"))
        .to_string()
}

/// One row of the proxy-read table: the client method a handler calls, the operation the SAME
/// read is made directly with, and the members it must have handed over.
struct ProxyReadCase {
    case: &'static str,
    /// The client method the handler calls, as the SDK names it.
    method: &'static str,
    /// The operation the direct call names — the answer this row is held against.
    action: &'static str,
    /// The output member the entries arrive under.
    items: &'static str,
    /// The member each entry is named by, read off every entry.
    name: &'static str,
    /// Member paths read off the FIRST entry, each held against the element of the same name in
    /// the service's own answer. Every one of them must resolve to something.
    read: &'static [&'static str],
}

/// The rows differ along WHICH read of the proxy's wiring the call names: every one of the four
/// the service models, each with the members an application resolves off it — the credential the
/// proxy logs in with, the health of the target it would connect to, the pool behind that target
/// and the address it dials.
const PROXY_READS: &[ProxyReadCase] = &[
    ProxyReadCase {
        case: "the proxy itself, with the secret its Auth names",
        method: "describe_db_proxies",
        action: "DescribeDBProxies",
        items: "DBProxies",
        name: "DBProxyName",
        read: &[
            "DBProxyName",
            "Status",
            "EngineFamily",
            "RoleArn",
            "Endpoint",
            "Auth/0/AuthScheme",
            "Auth/0/SecretArn",
        ],
    },
    ProxyReadCase {
        case: "the proxy's targets, with the health of each",
        method: "describe_db_proxy_targets",
        action: "DescribeDBProxyTargets",
        items: "Targets",
        name: "RdsResourceId",
        read: &[
            "RdsResourceId",
            "TargetArn",
            "Endpoint",
            "Port",
            "Type",
            "Role",
            "TargetHealth/State",
        ],
    },
    ProxyReadCase {
        case: "the target group the registrations live in",
        method: "describe_db_proxy_target_groups",
        action: "DescribeDBProxyTargetGroups",
        items: "TargetGroups",
        name: "TargetGroupName",
        read: &[
            "DBProxyName",
            "TargetGroupName",
            "TargetGroupArn",
            "IsDefault",
            "Status",
            "ConnectionPoolConfig/MaxConnectionsPercent",
        ],
    },
    ProxyReadCase {
        case: "the endpoints the proxy answers on",
        method: "describe_db_proxy_endpoints",
        action: "DescribeDBProxyEndpoints",
        items: "DBProxyEndpoints",
        name: "DBProxyEndpointName",
        read: &[
            "DBProxyEndpointName",
            "DBProxyEndpointArn",
            "DBProxyName",
            "Status",
            "Endpoint",
            "TargetRole",
            "IsDefault",
        ],
    },
];

/// `CreateDBInstance` and the wait for the status the registration needs it in.
async fn given_instance(router: &MultiServiceRouter, identifier: &str) {
    rds_ok(
        router,
        identifier,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", identifier),
            ("Engine", "mysql"),
            ("DBInstanceClass", "db.t3.micro"),
        ],
    )
    .await;
    // The move to `available` is owed by the CLOCK, so this waits for the CONDITION under a
    // generous deadline rather than for a fixed span.
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        let body = rds_ok(
            router,
            identifier,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier)],
        )
        .await;
        if element(&body, "DBInstanceStatus") == "available" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: expected {identifier} to reach available, last answer {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `CreateDBProxy` of `engine_family`, with the `Auth` entry naming `secret_arn` where one is
/// given, and the wait for the proxy status a registration needs.
async fn given_proxy(
    router: &MultiServiceRouter,
    name: &str,
    engine_family: &str,
    secret_arn: Option<&str>,
) {
    let mut params: Vec<(&str, &str)> = vec![
        ("DBProxyName", name),
        ("EngineFamily", engine_family),
        ("RoleArn", PROXY_ROLE_ARN),
        ("VpcSubnetIds.member.1", SUBNET_ID),
    ];
    if let Some(secret_arn) = secret_arn {
        params.push(("Auth.member.1.AuthScheme", "SECRETS"));
        params.push(("Auth.member.1.SecretArn", secret_arn));
        params.push(("Auth.member.1.UserName", "emu_app"));
        params.push(("Auth.member.1.IAMAuth", "DISABLED"));
    }
    rds_ok(router, name, "CreateDBProxy", &params).await;
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        let body = rds_ok(router, name, "DescribeDBProxies", &[("DBProxyName", name)]).await;
        if element(&body, "Status") == "available" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: expected proxy {name} to reach available, last answer {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `RegisterDBProxyTargets` for `identifier` on `proxy`, and the wait for the target health the
/// registration settles on.
async fn given_registered_target(router: &MultiServiceRouter, proxy: &str, identifier: &str) {
    rds_ok(
        router,
        proxy,
        "RegisterDBProxyTargets",
        &[
            ("DBProxyName", proxy),
            ("DBInstanceIdentifiers.member.1", identifier),
        ],
    )
    .await;
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        let body = rds_ok(
            router,
            proxy,
            "DescribeDBProxyTargets",
            &[("DBProxyName", proxy)],
        )
        .await;
        if element(&body, "State") == "AVAILABLE" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: expected the target for {identifier} on {proxy} to settle, last answer {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create the function this suite's rows run, and wait until it reports `Active`.
async fn given_function(router: &MultiServiceRouter, name: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(router, "the proxy-reading function", name).await;
}

#[tokio::test]
async fn a_handler_reads_its_proxys_wiring_through_the_sdk_this_runtime_supplies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = router();
    given_instance(&router, INSTANCE).await;
    given_proxy(&router, SECRET_PROXY, "MYSQL", Some(SECRET_ARN)).await;
    given_proxy(&router, IAM_PROXY, "POSTGRESQL", None).await;
    given_registered_target(&router, SECRET_PROXY, INSTANCE).await;
    given_registered_target(&router, IAM_PROXY, INSTANCE).await;
    let name = "rds-proxy-wiring-caller";
    given_function(&router, name).await;

    // 1. The read an application makes before it connects, dereferenced with no guard: the
    //    secret its proxy logs in with and the health of the target it would reach. What each
    //    must be is the service's own answer to the same two reads, made directly.
    let case = "the wiring an application resolves before it connects";
    let proxy_body = rds_ok(
        &router,
        case,
        "DescribeDBProxies",
        &[("DBProxyName", SECRET_PROXY)],
    )
    .await;
    let targets_body = rds_ok(
        &router,
        case,
        "DescribeDBProxyTargets",
        &[("DBProxyName", SECRET_PROXY)],
    )
    .await;
    let payload = invoke(
        &router,
        case,
        name,
        &json!({ "method": "wiring", "proxy": SECRET_PROXY }),
    )
    .await;
    for (member, expected) in [
        ("secret_arn", element(&proxy_body, "SecretArn")),
        ("role_arn", element(&proxy_body, "RoleArn")),
        ("endpoint", element(&proxy_body, "Endpoint")),
        ("target_state", element(&targets_body, "State")),
        ("target_type", element(&targets_body, "Type")),
        ("rds_resource_id", element(&targets_body, "RdsResourceId")),
    ] {
        assert!(
            !expected.is_empty(),
            "case {case:?}: the service's own answer must carry {member}, got {proxy_body} \
             {targets_body}"
        );
        assert_eq!(
            plain(payload.get(member)),
            expected,
            "case {case:?}: expected the handler to read {member} as the service reports it \
             ({expected:?}), got {payload}"
        );
    }

    // 2. Every read of the proxy's wiring the service models, each held member by member against
    //    the answer the SAME read gives a direct caller.
    for row in PROXY_READS {
        let body = rds_ok(
            &router,
            row.case,
            row.action,
            &[("DBProxyName", SECRET_PROXY)],
        )
        .await;
        let payload = invoke(
            &router,
            row.case,
            name,
            &json!({
                "method": row.method,
                "parameters": { "DBProxyName": SECRET_PROXY },
                "items": row.items,
                "name": row.name,
                "read": row.read,
            }),
        )
        .await;
        assert_eq!(
            payload.get("ok"),
            Some(&Value::Bool(true)),
            "case {:?}: expected the call to be served, got {payload}",
            row.case
        );
        assert_eq!(
            payload.get("status").and_then(Value::as_i64),
            Some(200),
            "case {:?}: expected the service's own 200, got {payload}",
            row.case
        );
        assert_eq!(
            payload.get("count").and_then(Value::as_i64),
            Some(1),
            "case {:?}: expected the one entry the service reports, got {payload}",
            row.case
        );
        assert!(
            payload
                .get("members")
                .and_then(Value::as_array)
                .is_some_and(|members| members.contains(&json!(row.items))),
            "case {:?}: expected the answer to carry {}, got {payload}",
            row.case,
            row.items
        );
        for path in row.read {
            let expected = element(&body, path.rsplit('/').next().unwrap_or(path));
            assert!(
                !expected.is_empty(),
                "case {:?}: the service's own answer must carry {path}, got {body}",
                row.case
            );
            let reached = read_at(&payload, path);
            assert_eq!(
                reached, expected,
                "case {:?}: expected {path} to be the {expected:?} the service reports, got \
                 {reached:?} in {payload}",
                row.case
            );
        }
    }

    // 3. A proxy whose `Auth` names NO secret — one authenticating end to end with IAM — reports
    //    no entry to resolve, while the other proxy's does: the handler reads the member the
    //    service reports for the proxy it asked about, not one shape for every proxy.
    let case = "a proxy whose Auth names no secret";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_db_proxies",
            "parameters": { "DBProxyName": IAM_PROXY },
            "items": "DBProxies",
            "name": "DBProxyName",
            "read": ["DBProxyName", "EngineFamily", "Auth/0/SecretArn"],
        }),
    )
    .await;
    let iam_body = rds_ok(
        &router,
        case,
        "DescribeDBProxies",
        &[("DBProxyName", IAM_PROXY)],
    )
    .await;
    assert_eq!(
        (
            read_at(&payload, "DBProxyName"),
            read_at(&payload, "EngineFamily"),
            read_at(&payload, "Auth/0/SecretArn"),
        ),
        (
            element(&iam_body, "DBProxyName"),
            element(&iam_body, "EngineFamily"),
            String::new(),
        ),
        "case {case:?}: expected the proxy the service reports and no secret on it, got {payload}"
    );

    // 4. The listing narrowed by NOTHING: every proxy of the Region, which is both of them, in
    //    the order the service reports them.
    let case = "every proxy of the Region";
    let listed = rds_ok(&router, case, "DescribeDBProxies", &[]).await;
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_db_proxies",
            "parameters": {},
            "items": "DBProxies",
            "name": "DBProxyName",
            "read": ["DBProxyName"],
        }),
    )
    .await;
    let mut reported: Vec<String> = payload
        .get("names")
        .and_then(Value::as_array)
        .map(|names| names.iter().map(|name| plain(Some(name))).collect())
        .unwrap_or_default();
    reported.sort();
    assert_eq!(
        reported,
        vec![IAM_PROXY.to_string(), SECRET_PROXY.to_string()],
        "case {case:?}: expected both proxies the service lists ({listed}), got {payload}"
    );

    // 5. The same read cut to a PAGE: 20 is the smallest `MaxRecords` the member publishes
    //    (`smithy_query(query="doc",shape="DescribeDBProxyTargetsRequest",member="MaxRecords")`:
    //    "Minimum 20, maximum 100"), and the page the call asked for bounds what came back — which
    //    for a group holding fewer targets than that is every target the service reports.
    let case = "a page of at most the records asked for";
    let direct = rds_ok(
        &router,
        case,
        "DescribeDBProxyTargets",
        &[("DBProxyName", SECRET_PROXY)],
    )
    .await;
    let reported = direct.matches("<RdsResourceId>").count();
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_db_proxy_targets",
            "parameters": { "DBProxyName": SECRET_PROXY, "MaxRecords": 20 },
            "items": "Targets",
            "name": "RdsResourceId",
            "read": ["TargetHealth/State"],
        }),
    )
    .await;
    let paged = payload
        .get("count")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    assert!(
        paged > 0 && paged <= 20 && paged == i64::try_from(reported).unwrap_or_default(),
        "case {case:?}: expected the {reported} target(s) the service reports, within the 20 \
         records asked for, got {paged} in {payload}"
    );

    // 6. A REFUSED read: a proxy name that resolves nothing. What it must be refused with is the
    //    service's own answer to the same call, read by making it directly.
    let case = "a proxy name that resolves nothing";
    let (status, body) = rds(
        &router,
        "DescribeDBProxyTargets",
        &[("DBProxyName", ABSENT_PROXY)],
    )
    .await;
    let expected_code = element(&body, "Code");
    // The sentence travels in an XML document, which escapes the apostrophes the service's own
    // not-found wording quotes a name with; the SDK hands a handler that sentence itself.
    let expected_message = unescaped(&element(&body, "Message"));
    assert_ne!(
        status, 200,
        "case {case:?}: the direct call must be refused, got {status} {body}"
    );
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "describe_db_proxy_targets",
            "parameters": { "DBProxyName": ABSENT_PROXY },
            "items": "Targets",
            "name": "RdsResourceId",
            "read": ["TargetHealth/State"],
        }),
    )
    .await;
    assert_eq!(
        (
            payload.get("code").and_then(Value::as_str),
            payload.get("message").and_then(Value::as_str),
            payload.get("status").and_then(Value::as_i64),
        ),
        (
            Some(expected_code.as_str()),
            Some(expected_message.as_str()),
            Some(i64::from(status)),
        ),
        "case {case:?}: expected the code, sentence and status the service answered \
         ({expected_code}, {expected_message:?}, {status}), got {payload}"
    );

    // 7. An operation of this service the runtime's SDK does not bind: the handler is told the
    //    call was NOT MADE, naming it and what is bound, rather than sent a request this seam
    //    would be guessing at.
    let case = "a proxy operation this runtime's SDK does not bind";
    let payload = invoke(
        &router,
        case,
        name,
        &json!({
            "method": "register_db_proxy_targets",
            "parameters": {
                "DBProxyName": SECRET_PROXY,
                "DBInstanceIdentifiers": [INSTANCE],
            },
            "items": "DBProxyTargets",
            "name": "RdsResourceId",
            "read": ["RdsResourceId"],
        }),
    )
    .await;
    let reported = payload
        .get("not_made")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // The name the refusal reports is the one Amazon RDS's own model spells the operation with
    // (`RegisterDBProxyTargets`), not the recasing of the method name the SDK derived it from.
    assert!(
        reported.contains("RegisterDBProxyTargets")
            && reported.contains("DescribeDBProxyTargets"),
        "case {case:?}: expected the refusal to name the call and what is bound, got {payload}"
    );
}

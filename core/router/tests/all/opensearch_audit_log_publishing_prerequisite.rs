// SPDX-License-Identifier: CC0-1.0

//! Which LOG TYPES a domain may publish depends on the configuration it runs — end to end
//! over the real `MultiServiceRouter` with Amazon OpenSearch Service and Amazon CloudWatch
//! Logs mounted on one engine, so the destination each option names is a log group that
//! service really holds and the answer asserted is what a client sees.
//!
//! The RULE pinned here: **a domain publishes `AUDIT_LOGS` only while fine-grained access
//! control is enabled on it, so a request that would leave it publishing that log type
//! without that boundary is refused `ValidationException` (HTTP 400) and changes nothing —
//! whichever of the two calls that take `LogPublishingOptions` carries it.**
//!
//! `LogType`'s `AUDIT_LOGS` is the log whose records are the "user requests for access to
//! the domain" (`smithy_query(query="shape",shape="LogType")`), and WHO may reach a domain
//! — so what a request for access is granted or refused on — is decided by fine-grained
//! access control (`AdvancedSecurityOptions$Enabled`, "true to enable fine-grained access
//! control"). A domain not reached through that boundary takes no such decision, so the log
//! type has nothing to publish and the configuration describes a delivery that can never
//! happen. The refusal is the operation's modelled `ValidationException`
//! (`smithy_query(query="operation",operation="CreateDomain,UpdateDomainConfig")`); its
//! sentence is recorded behaviour, so it is spelled out here rather than imported, and a
//! service that worded it differently fails this suite.
//!
//! The rows differ along what the rule generalises over: whether the audit option is
//! enabled at all, whether the request switches the boundary on, leaves it out or carries it
//! switched off, how many log types the map carries beside the audit one, and which log
//! types those are — each row driven through the create AND through the call that changes
//! what a domain publishes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The error every refusal below carries: the operation's modelled 400 for a configuration
/// the service will not provision.
const REFUSAL: &str = "ValidationException";

/// The sentence the service refuses an audit option without that boundary with. Spelled
/// here rather than imported from the service, so a wrong sentence in the service fails
/// this suite.
const AUDIT_NEEDS_ADVANCED_SECURITY: &str =
    "audit log publishing cannot be enabled as you do not have advanced security options \
     configured.";

/// The service principal Amazon OpenSearch Service delivers a domain's logs under, which is
/// what each destination group's resource policy has to admit.
const DELIVERY_PRINCIPAL: &str = "es.amazonaws.com";

/// The two calls that delivery is made of, each needing its own grant.
const DELIVERY_ACTIONS: [&str; 2] = ["logs:CreateLogStream", "logs:PutLogEvents"];

/// The log group every row publishes to, and a second one for the rows carrying two log
/// types.
const AUDIT_GROUP: &str = "/aws/opensearch/domains/audit-rule";
const OTHER_GROUP: &str = "/aws/opensearch/domains/other-rule";

/// How long a case waits for a condition the clock owes a domain — its create or its change
/// settling — before it gives up. Generous on purpose: the span is the service's, and what
/// the case waits for is the CONDITION, so a loaded host delays a row rather than breaking
/// it.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// One look inside [`SETTLE_DEADLINE`].
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Amazon OpenSearch Service and Amazon CloudWatch Logs over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_opensearch_amazonopensearchservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
/// routes the request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `restJson1` call to the `es` endpoint, as its status, its `x-amzn-errortype` and its
/// parsed body.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Option<String>, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://es.{REGION}.amazonaws.com{path}"))
        .header("host", format!("es.{REGION}.amazonaws.com"))
        .header("authorization", authorization("es"));
    let payload = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Bytes::from(body.to_string())
        }
        None => Bytes::new(),
    };
    let request = builder.body(payload).expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, error, parsed)
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`) that must answer
/// 200.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("logs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch Logs {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// A log group CloudWatch Logs holds, whose own resource policy lets this service deliver
/// into it — so what a row proves is the audit rule rather than the destination rules around
/// it. Answers the group's ARN, read back off the group rather than built here.
async fn given_granted_group(router: &MultiServiceRouter, name: &str) -> String {
    let case = "granting a destination group";
    logs(router, case, "CreateLogGroup", &json!({ "logGroupName": name })).await;
    let described = logs(
        router,
        case,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": name }),
    )
    .await;
    let arn = described["logGroups"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the group's arn, got {described}"))
        .to_string();
    // The grant is written over the group AND everything under it, which is the resource a
    // `logs:CreateLogStream` is taken on.
    let under_the_group = match arn.strip_suffix(":*") {
        Some(group) => format!("{group}:*"),
        None => format!("{arn}:*"),
    };
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": DELIVERY_PRINCIPAL },
            "Action": DELIVERY_ACTIONS.to_vec(),
            "Resource": under_the_group,
        }],
    });
    logs(
        router,
        case,
        "PutResourcePolicy",
        &json!({
            "policyName": format!("{name}-opensearch"),
            "policyDocument": document.to_string(),
            "resourceArn": arn,
        }),
    )
    .await;
    arn.strip_suffix(":*").unwrap_or(&arn).to_string()
}

/// The containers a domain reached through fine-grained access control is created with: the
/// boundary itself, its master user, and the three prerequisites the service requires of it
/// (encryption at rest, node-to-node encryption, an HTTPS-only endpoint).
fn advanced_security() -> Vec<(&'static str, Value)> {
    vec![
        ("EncryptionAtRestOptions", json!({ "Enabled": true })),
        ("NodeToNodeEncryptionOptions", json!({ "Enabled": true })),
        ("DomainEndpointOptions", json!({ "EnforceHTTPS": true })),
        (
            "AdvancedSecurityOptions",
            json!({
                "Enabled": true,
                "InternalUserDatabaseEnabled": false,
                "MasterUserOptions": {
                    "MasterUserARN":
                        format!("arn:aws:iam::{}:user/audit-master", ::engine::emulator_account_id()),
                },
            }),
        ),
    ]
}

/// A `CreateDomain` body for `name`: one small cluster on one release, plus whatever the row
/// adds.
fn create_body(name: &str) -> Value {
    json!({
        "DomainName": name,
        "EngineVersion": "OpenSearch_2.11",
        "ClusterConfig": { "InstanceType": "r6g.large.search", "InstanceCount": 1 },
        "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 20 },
    })
}

/// What `DescribeDomain` reports `name` publishing, and whether it reports the domain at
/// all.
async fn published(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (status, _, body) = call(
        router,
        "GET",
        &format!("/2021-01-01/opensearch/domain/{name}"),
        None,
    )
    .await;
    (status == 200).then(|| {
        body["DomainStatus"]
            .get("LogPublishingOptions")
            .cloned()
            .unwrap_or(Value::Null)
    })
}

/// Wait until `name` reports nothing in flight — the condition a client polls for, and what
/// a further configuration change needs — looking again rather than sleeping for the span.
async fn settled(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _, body) = call(
            router,
            "GET",
            &format!("/2021-01-01/opensearch/domain/{name}"),
            None,
        )
        .await;
        let domain = body["DomainStatus"].clone();
        if status == 200
            && domain["Created"].as_bool() == Some(true)
            && domain["Processing"].as_bool() == Some(false)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {name:?}: the domain never reported itself settled within \
             {SETTLE_DEADLINE:?}, last read {status} with {domain}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the table: the `LogPublishingOptions` map a request carries, whether it also
/// asks for fine-grained access control, and whether the rule refuses it.
struct Row {
    case: &'static str,
    options: Value,
    /// The `AdvancedSecurityOptions` the request carries, as the row's own JSON — `None`
    /// where it carries none at all.
    security: Option<Value>,
    refused: bool,
}

/// The table. `audit` and `other` are the ARNs of the two granted groups, so every row's
/// destinations exist and admit the delivery.
fn rows(audit: &str, other: &str) -> Vec<Row> {
    let security = advanced_security()
        .into_iter()
        .find(|(option, _)| *option == "AdvancedSecurityOptions")
        .map(|(_, value)| value)
        .expect("the advanced-security container");
    vec![
        Row {
            case: "AUDIT_LOGS enabled with no advanced security container at all",
            options: json!({
                "AUDIT_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": audit },
            }),
            security: None,
            refused: true,
        },
        Row {
            case: "AUDIT_LOGS enabled with the boundary switched off",
            options: json!({
                "AUDIT_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": audit },
            }),
            security: Some(json!({ "Enabled": false })),
            refused: true,
        },
        Row {
            case: "AUDIT_LOGS enabled beside another log type, no advanced security",
            options: json!({
                "AUDIT_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": audit },
                "ES_APPLICATION_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": other },
            }),
            security: None,
            refused: true,
        },
        Row {
            case: "AUDIT_LOGS enabled with the boundary switched on",
            options: json!({
                "AUDIT_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": audit },
            }),
            security: Some(security.clone()),
            refused: false,
        },
        Row {
            case: "AUDIT_LOGS enabled beside two other log types, boundary switched on",
            options: json!({
                "AUDIT_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": audit },
                "ES_APPLICATION_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": other },
                "SEARCH_SLOW_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": other },
            }),
            security: Some(security),
            refused: false,
        },
        Row {
            // A DISABLED option publishes nothing, so it needs no boundary: it is stored as
            // written.
            case: "AUDIT_LOGS disabled, no advanced security",
            options: json!({
                "AUDIT_LOGS": { "Enabled": false, "CloudWatchLogsLogGroupArn": audit },
            }),
            security: None,
            refused: false,
        },
        Row {
            case: "every log type but the audit one, no advanced security",
            options: json!({
                "ES_APPLICATION_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": other },
                "INDEX_SLOW_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": other },
                "SEARCH_SLOW_LOGS": { "Enabled": true, "CloudWatchLogsLogGroupArn": audit },
            }),
            security: None,
            refused: false,
        },
        Row {
            case: "no log publishing at all, no advanced security",
            options: json!({}),
            security: None,
            refused: false,
        },
    ]
}

/// The refusal a row expects, asserted on the answer `answer` to `case`.
fn assert_refused(answer: &(u16, Option<String>, Value), case: &str) {
    let (status, error, body) = answer;
    assert_eq!(
        *status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body}"
    );
    assert_eq!(
        error.as_deref(),
        Some(REFUSAL),
        "case {case:?}: expected the error type {REFUSAL}, got {error:?} with {body}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(AUDIT_NEEDS_ADVANCED_SECURITY),
        "case {case:?}: expected the prerequisite sentence, got {body}"
    );
}

#[tokio::test]
async fn audit_log_publishing_needs_fine_grained_access_control_at_create() {
    let router = router();
    let audit = given_granted_group(&router, AUDIT_GROUP).await;
    let other = given_granted_group(&router, OTHER_GROUP).await;

    for (index, row) in rows(&audit, &other).into_iter().enumerate() {
        let Row {
            case,
            options,
            security,
            refused,
        } = row;
        let name = format!("audit-create-{index}");
        let mut body = create_body(&name);
        body["LogPublishingOptions"] = options.clone();
        if let Some(security) = &security {
            // A row that switches the boundary ON carries the three prerequisites that
            // boundary itself has, so what the row proves is the audit rule.
            if security["Enabled"].as_bool() == Some(true) {
                for (option, value) in advanced_security() {
                    body[option] = value;
                }
            }
            body["AdvancedSecurityOptions"] = security.clone();
        }

        let answer = call(&router, "POST", "/2021-01-01/opensearch/domain", Some(&body)).await;
        if refused {
            assert_refused(&answer, case);
            // Nothing is stored: the domain the refused create named does not exist.
            assert_eq!(
                published(&router, &name).await,
                None,
                "case {case:?}: a refused create must leave no domain behind"
            );
            continue;
        }
        let (status, _, created) = &answer;
        assert_eq!(
            *status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {created}"
        );
        let expected = if options.as_object().is_some_and(|types| !types.is_empty()) {
            options.clone()
        } else {
            Value::Null
        };
        assert_eq!(
            published(&router, &name).await,
            Some(expected.clone()),
            "case {case:?}: expected the created domain to publish {expected}"
        );
    }
}

#[tokio::test]
async fn audit_log_publishing_needs_fine_grained_access_control_at_update() {
    let router = router();
    let audit = given_granted_group(&router, AUDIT_GROUP).await;
    let other = given_granted_group(&router, OTHER_GROUP).await;

    // Two settled domains, differing in the ONE thing the rule reads: one reached through
    // fine-grained access control, one not. The change is judged against the configuration
    // the domain KEEPS, so the same map is accepted on the one and refused on the other.
    let plain = "audit-update-plain";
    let guarded = "audit-update-guarded";
    for (name, secured) in [(plain, false), (guarded, true)] {
        let mut body = create_body(name);
        if secured {
            for (option, value) in advanced_security() {
                body[option] = value;
            }
        }
        let (status, _, created) =
            call(&router, "POST", "/2021-01-01/opensearch/domain", Some(&body)).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected the create to be accepted, got {status} with {created}"
        );
        settled(&router, name).await;
    }

    for (index, row) in rows(&audit, &other).into_iter().enumerate() {
        let Row {
            case,
            options,
            security,
            refused,
        } = row;
        // The rows that ask for the boundary are driven against the domain that has it, and
        // the rest against the domain that does not — which is what the merged
        // configuration of each change leaves behind.
        let secured = security
            .as_ref()
            .is_some_and(|security| security["Enabled"].as_bool() == Some(true));
        let name = if secured { guarded } else { plain };
        let before = published(&router, name).await;
        let answer = call(
            &router,
            "POST",
            &format!("/2021-01-01/opensearch/domain/{name}/config"),
            Some(&json!({ "LogPublishingOptions": options.clone() })),
        )
        .await;
        let case = format!("{case} (change {index} of {name})");
        if refused {
            assert_refused(&answer, &case);
            // A refused change leaves the domain publishing what it published before.
            assert_eq!(
                published(&router, name).await,
                before,
                "case {case:?}: a refused change must leave the domain publishing {before:?}"
            );
            continue;
        }
        let (status, _, changed) = &answer;
        assert_eq!(
            *status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {changed}"
        );
        // An accepted change lands before the next row starts another: the condition is
        // polled, never slept for.
        settled(&router, name).await;
        let expected = if options.as_object().is_some_and(|types| !types.is_empty()) {
            options.clone()
        } else {
            Value::Null
        };
        assert_eq!(
            published(&router, name).await,
            Some(expected.clone()),
            "case {case:?}: expected the changed domain to publish {expected}"
        );
    }
}

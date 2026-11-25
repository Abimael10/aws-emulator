// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed Amazon OpenSearch Service domain with fine-grained access
//! control and log publishing, driven end to end: the deploy, the signed REST data plane the
//! deployed domain serves, the security plugin's own audit configuration on it, and the audit
//! records a document request leaves in the log group the `AUDIT_LOGS` option names.
//!
//! RULE: **a domain an accepted create leaves behind becomes a usable cluster — whichever
//! surface made the create — so the owning service reports it `Processing=false` with the
//! `Endpoint` it published, that endpoint serves the signed REST data plane including the
//! security plugin's audit-configuration resource, and every decision the cluster takes about
//! an audited request is delivered to the log group the `AUDIT_LOGS` option names and to NO
//! other group the domain publishes to.** `CreateDomain` takes `AdvancedSecurityOptions` and
//! `LogPublishingOptions`, a map of `LogType`
//! (`smithy_query(query="shape",shape="LogType,LogPublishingOption")`) to
//! `LogPublishingOption {CloudWatchLogsLogGroupArn, Enabled}`, and `DescribeDomain` reports
//! `DomainStatus {Processing, Endpoint, LogPublishingOptions}`; the data plane and the
//! plugin's audit API are recorded behaviour.
//!
//! The rows differ along what the rule generalises over: how many log types the deployed
//! domain publishes (all four of `LogType`'s values, each to its own group, and `AUDIT_LOGS`
//! alone), whether the template NAMES the domain or leaves the name to CloudFormation, which
//! of the two spellings of the plugin's audit resource the setup step addresses (the current
//! `_plugins` prefix and the legacy `_opendistro` one), and the index and document ids the
//! audited requests address.
//!
//! Three crates have to agree for that to hold — CloudFormation plans the estate and makes
//! the calls, Amazon CloudWatch Logs holds the groups and the grant, and Amazon OpenSearch
//! Service judges the create, serves the data plane and delivers the records — so the suite
//! lives here rather than in any one service's crate.
//!
//! Every wait is for a CONDITION and never for a span: the stack reaching a terminal status
//! and the records reaching the group are looked at again until they hold or a generous
//! deadline passes, so nothing here is decided by how fast or how loaded the host is.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region every resource below
/// belongs to.
///
/// `pub(crate)` with the declaration below it, because the sibling suite that drives this same
/// lifecycle on the emulator a client STARTS
/// ([`crate::cloudformation_deployed_audited_domain_on_a_started_emulator`]) deploys the same
/// estate and reads the same records: what the template declares and what an audit record says
/// are stated HERE, once, so the two suites cannot disagree about the lifecycle they are both
/// about.
pub(crate) const REGION: &str = "us-east-1";

/// The account this emulator serves, which the declared grant names its groups under.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `StackStatus` a completed deployment reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How often and how long the suite looks again for a condition it is waiting on. The
/// deadline only has to make the wait TERMINATE: the spans the services behind a stack choose
/// take the time they take, and a loaded host is slower without the rule being any different.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// The service principal a domain's log delivery is made under, which the declared
/// account-scoped policy grants.
const DELIVERY_PRINCIPAL: &str = "es.amazonaws.com";

/// The two actions delivery needs on the groups it writes to.
const DELIVERY_ACTIONS: [&str; 2] = ["logs:CreateLogStream", "logs:PutLogEvents"];

/// `LogType`'s whole value set (`smithy_query(query="shape",shape="LogType")`), each paired
/// with the logical id of the log group a template declares for it.
pub(crate) const LOG_TYPES: [(&str, &str); 4] = [
    ("AUDIT_LOGS", "AuditLogGroup"),
    ("ES_APPLICATION_LOGS", "AppLogGroup"),
    ("SEARCH_SLOW_LOGS", "SlowSearchLogGroup"),
    ("INDEX_SLOW_LOGS", "SlowIndexLogGroup"),
];

/// The log type whose group the plugin's audit records go to.
pub(crate) const AUDIT_LOG_TYPE: &str = "AUDIT_LOGS";

/// The logical ids of the domain and of the account-scoped grant it depends on.
const DOMAIN_ID: &str = "SearchDomain";
const POLICY_ID: &str = "LogDeliveryPolicy";

/// The template parameters a deployment passes the grant's name and the domain's master user
/// in.
pub(crate) const POLICY_NAME_PARAMETER: &str = "PolicyName";
pub(crate) const MASTER_USER_PARAMETER: &str = "MasterUserArn";

/// How long the declared groups keep their records, a value `RetentionInDays`' own set
/// admits.
const RETENTION_IN_DAYS: u64 = 3653;

/// The engine release every row deploys, one the owning service's catalogue offers, with an
/// instance type and a volume that release takes — so the only thing a row can fail on is the
/// lifecycle under test.
const ENGINE_VERSION: &str = "OpenSearch_2.11";
const INSTANCE_TYPE: &str = "t3.small.search";
const VOLUME_TYPE: &str = "gp3";
const VOLUME_SIZE: u64 = 10;

/// The two spellings of the security plugin's audit-configuration resource: the prefix its
/// API carries today, and the one the Open Distro-era tooling still addresses.
const CURRENT_AUDIT_PATH: &str = "/_plugins/_security/api/audit";
pub(crate) const LEGACY_AUDIT_PATH: &str = "/_opendistro/_security/api/audit";

/// The audit categories a served document request leaves a record of, one per layer.
pub(crate) const CATEGORY_AUTHENTICATED: &str = "AUTHENTICATED";
pub(crate) const CATEGORY_GRANTED_PRIVILEGES: &str = "GRANTED_PRIVILEGES";

/// The transport request the cluster carries a document read in, which a transport-layer
/// record names.
pub(crate) const GET_REQUEST: &str = "GetRequest";

/// The two streams Amazon OpenSearch Service's own create-time probe of a granted destination
/// opens in each group, whose events belong to the CREATE rather than to the work a row does.
pub(crate) const PROBE_STREAMS: [&str; 2] = ["cust-test-log-stream", "es-test-log-stream"];

/// The emulator as it is ASSEMBLED, on one engine: CloudFormation reaches the owning services
/// through the in-process callback, and the domain this suite deploys is the domain its signed
/// requests then address.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One Amazon CloudWatch Logs call over its awsJson1_1 wire.
async fn logs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization("logs"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One Amazon OpenSearch Service control-plane call over its `restJson1` wire.
async fn opensearch(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("es.{REGION}.amazonaws.com"))
        .header("authorization", authorization("es"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// `DescribeDomain`'s `DomainStatus` for `name`.
async fn described(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let (status, body) = opensearch(
        router,
        "GET",
        &format!("/2021-01-01/opensearch/domain/{name}"),
    )
    .await;
    (status, body["DomainStatus"].clone())
}

/// One signed request to the ADDRESS the domain published, dialled the way a client dials it.
async fn at_endpoint(
    router: &MultiServiceRouter,
    endpoint: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |document| Bytes::from(document.to_string()));
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{endpoint}{path}"))
        .header("host", endpoint.to_string())
        .header("authorization", authorization("es"))
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The ARN of the identity this suite deploys and signs with, as `sts:GetCallerIdentity`
/// reports it — the principal each row's template names as the domain's master user, which is
/// what the plugin's audit API admits.
async fn caller_arn(router: &MultiServiceRouter) -> String {
    let (status, body) = query(router, "sts", "2011-06-15", "GetCallerIdentity", &[]).await;
    let arn = opt_element(&body, "Arn").unwrap_or_default();
    assert!(
        status == 200 && arn.starts_with("arn:"),
        "setup: GetCallerIdentity must report the signing identity, got {status} with {body}"
    );
    arn
}

/// The `StackStatus` the stack reports now, with the answer it was read from.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStacks {stack:?}: expected HTTP 200, got {status} with {body}"
    );
    (opt_element(&body, "StackStatus").unwrap_or_default(), body)
}

/// Every resource failure the stack recorded, so a case can say WHY it did not complete.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&body, "ResourceStatusReason")
}

/// Wait until the stack settles in a status that is not still in progress, and answer that
/// status with the `DescribeStacks` answer it was read from.
async fn settled_stack(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut last = stack_status(router, stack).await;
    while last.0.ends_with("_IN_PROGRESS") && Instant::now() < giving_up_at {
        tokio::time::sleep(POLL_INTERVAL).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// The log group name a deployment of `stack` declares for `logical_id`.
///
/// `pub(crate)` for the started-emulator sibling, which deploys the same estate.
pub(crate) fn group_name(stack: &str, logical_id: &str) -> String {
    format!("/aws/opensearch/{stack}/{}", logical_id.to_ascii_lowercase())
}

/// The ARN `AWS::Logs::LogGroup`'s `Arn` attribute publishes for a group this account holds:
/// the group's own ARN with the wildcard its log streams are named by after it, which is the
/// form each template below names its destination in.
fn group_attribute_arn(group: &str) -> String {
    format!("arn:aws:logs:{REGION}:{ACCOUNT}:log-group:{group}:*")
}

/// Every record `group` holds, each parsed back from the line it was delivered as, LESS the
/// events of the two streams the create-time probe opens — those are the CREATE's, not the
/// work a row did.
async fn delivered(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<Value> {
    let (status, parsed) = logs(
        router,
        "FilterLogEvents",
        &json!({ "logGroupName": group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from FilterLogEvents({group:?}), got {status} with \
         {parsed}"
    );
    parsed["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter(|event| {
                    !PROBE_STREAMS.contains(&event["logStreamName"].as_str().unwrap_or_default())
                })
                .map(|event| {
                    let message = event["message"].as_str().unwrap_or_default();
                    serde_json::from_str(message).unwrap_or_else(|_| json!(message))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until `group` holds at least `wanted` records of the work a row did, and answer every
/// record it then holds.
async fn delivered_at_least(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<Value> {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut records = delivered(router, case, group).await;
    while records.len() < wanted && Instant::now() < giving_up_at {
        tokio::time::sleep(POLL_INTERVAL).await;
        records = delivered(router, case, group).await;
    }
    records
}

/// One row: the estate it deploys, which log types the domain publishes, and the audited work
/// it then does on the deployed domain.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The domain name the template declares, or `None` for a declaration that leaves the
    /// name to CloudFormation. Inside `DomainName`'s own `^[a-z][a-z0-9\-]+$` and 3..28
    /// characters where it is declared.
    domain: Option<&'static str>,
    /// The log types the deployed domain publishes, each to a group of its own.
    published: &'static [(&'static str, &'static str)],
    /// The spelling of the plugin's audit resource this row's setup step addresses.
    audit_path: &'static str,
    /// The index the audited requests address, inside `IndexName`'s own 1..255.
    index: &'static str,
    /// The document that index is seeded with, and an id it does not hold.
    document: &'static str,
    absent: &'static str,
}

/// The table: one row per way a deployed audited domain differs from another in what it
/// publishes, how it is named, which spelling of the plugin's resource configures it, and
/// what the audited requests address.
fn rows() -> Vec<Row> {
    vec![
        // The recorded shape: all four log types, each to the stack's own group, a domain
        // CloudFormation names, and the legacy spelling of the audit resource.
        Row {
            case: "all four log types, a generated domain name, the legacy audit resource",
            stack: "osds-e2e-fourlogs-estate-stack",
            domain: None,
            published: &LOG_TYPES,
            audit_path: LEGACY_AUDIT_PATH,
            index: "requests-2026.01",
            document: "seeded",
            absent: "unseeded",
        },
        // The audit log ALONE, on a domain the template names, configured through the
        // resource's current spelling: the records go to the one group there is.
        Row {
            case: "the audit log alone, a declared domain name, the current audit resource",
            stack: "osds-e2e-auditonly",
            domain: Some("osds-e2e-auditonly"),
            published: &[LOG_TYPES[0]],
            audit_path: CURRENT_AUDIT_PATH,
            index: "a",
            document: "0",
            absent: "nothing-here",
        },
    ]
}

/// The template a deployment of `stack` carries: a log group per published log type, one
/// account-scoped grant over them, and the fine-grained-access-control domain that publishes
/// to them — named by the template where `domain` says so, and left to CloudFormation to
/// generate where it does not.
///
/// `pub(crate)` for the started-emulator sibling: ONE statement of what the estate under test
/// is, so the two suites cannot deploy different things.
pub(crate) fn template(
    stack: &str,
    domain: Option<&str>,
    published: &[(&str, &str)],
) -> String {
    let mut resources = Map::new();
    for (_, logical_id) in published {
        resources.insert(
            (*logical_id).to_string(),
            json!({
                "Type": "AWS::Logs::LogGroup",
                "Properties": {
                    "LogGroupName": group_name(stack, logical_id),
                    "RetentionInDays": RETENTION_IN_DAYS,
                },
            }),
        );
    }
    let groups: Vec<String> = published
        .iter()
        .map(|(_, logical_id)| (*logical_id).to_string())
        .collect();
    let resource_list = groups
        .iter()
        .map(|logical_id| format!("\\\"${{{logical_id}.Arn}}\\\""))
        .collect::<Vec<String>>()
        .join(",");
    let actions = DELIVERY_ACTIONS
        .iter()
        .map(|action| format!("\\\"{action}\\\""))
        .collect::<Vec<String>>()
        .join(",");
    let document = format!(
        "{{\\\"Version\\\":\\\"2012-10-17\\\",\\\"Statement\\\":[{{\
         \\\"Effect\\\":\\\"Allow\\\",\
         \\\"Principal\\\":{{\\\"Service\\\":\\\"{DELIVERY_PRINCIPAL}\\\"}},\
         \\\"Action\\\":[{actions}],\
         \\\"Resource\\\":[{resource_list}]}}]}}"
    );
    let document: Value = serde_json::from_str(&format!("\"{document}\""))
        .expect("the declared policy document is one JSON string");
    resources.insert(
        POLICY_ID.to_string(),
        json!({
            "Type": "AWS::Logs::ResourcePolicy",
            "DependsOn": groups,
            "Properties": {
                "PolicyName": { "Ref": POLICY_NAME_PARAMETER },
                "PolicyDocument": { "Fn::Sub": document },
            },
        }),
    );

    let mut options = Map::new();
    for (log_type, logical_id) in published {
        options.insert(
            (*log_type).to_string(),
            json!({
                "Enabled": true,
                "CloudWatchLogsLogGroupArn": { "Fn::GetAtt": [logical_id, "Arn"] },
            }),
        );
    }
    let mut properties = json!({
        "EngineVersion": ENGINE_VERSION,
        "ClusterConfig": { "InstanceType": INSTANCE_TYPE, "InstanceCount": 1 },
        "EBSOptions": {
            "EBSEnabled": true,
            "VolumeType": VOLUME_TYPE,
            "VolumeSize": VOLUME_SIZE,
        },
        "EncryptionAtRestOptions": { "Enabled": true },
        "NodeToNodeEncryptionOptions": { "Enabled": true },
        "DomainEndpointOptions": { "EnforceHTTPS": true },
        "AdvancedSecurityOptions": {
            "Enabled": true,
            "InternalUserDatabaseEnabled": false,
            "MasterUserOptions": {
                "MasterUserARN": { "Ref": MASTER_USER_PARAMETER },
            },
        },
        "AccessPolicies": {
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {
                    "AWS": {
                        "Fn::Sub": "arn:${AWS::Partition}:iam::${AWS::AccountId}:root",
                    },
                },
                "Action": "es:*",
                "Resource": "*",
            }],
        },
        "LogPublishingOptions": Value::Object(options),
    });
    if let Some(name) = domain {
        properties["DomainName"] = json!(name);
    }
    resources.insert(
        DOMAIN_ID.to_string(),
        json!({
            "Type": "AWS::OpenSearchService::Domain",
            "DependsOn": [POLICY_ID],
            "Properties": properties,
        }),
    );

    let mut outputs = json!({
        "DomainName": { "Value": { "Ref": DOMAIN_ID } },
        "DomainEndpoint": { "Value": { "Fn::GetAtt": [DOMAIN_ID, "DomainEndpoint"] } },
    });
    for (_, logical_id) in published {
        outputs[format!("{logical_id}Name")] = json!({ "Value": { "Ref": logical_id } });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "an audited search domain and the log groups it publishes to",
        "Parameters": {
            POLICY_NAME_PARAMETER: { "Type": "String" },
            MASTER_USER_PARAMETER: { "Type": "String" },
        },
        "Resources": Value::Object(resources),
        "Outputs": outputs,
    })
    .to_string()
}

/// Deploy one row's estate and answer the outputs the completed deployment published.
async fn deploy(
    router: &MultiServiceRouter,
    row: &Row,
    master_user: &str,
) -> BTreeMap<String, String> {
    let case = row.case;
    let policy_name = format!("{}-delivery", row.stack);
    let body = template(row.stack, row.domain, row.published);
    let (status, answer) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &body),
            ("Parameters.member.1.ParameterKey", POLICY_NAME_PARAMETER),
            ("Parameters.member.1.ParameterValue", &policy_name),
            ("Parameters.member.2.ParameterKey", MASTER_USER_PARAMETER),
            ("Parameters.member.2.ParameterValue", master_user),
        ],
    )
    .await;
    assert!(
        status == 200 && answer.contains("StackId"),
        "case {case:?}: expected CreateStack to be accepted, got {status} with {answer}"
    );

    let (reported, settled) = settled_stack(router, row.stack).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: expected the declared estate to deploy, got {reported} with the \
         resource failures {:?}",
        failure_reasons(router, row.stack).await
    );
    let outputs = outputs_of(&settled);
    for (_, logical_id) in row.published {
        let key = format!("{logical_id}Name");
        assert_eq!(
            outputs.get(&key).map(String::as_str),
            Some(group_name(row.stack, logical_id).as_str()),
            "case {case:?}: expected the stack to publish the {logical_id} group's name, got \
             {outputs:?}"
        );
    }
    for key in ["DomainName", "DomainEndpoint"] {
        assert!(
            outputs.get(key).is_some_and(|value| !value.is_empty()),
            "case {case:?}: expected the stack to publish a non-empty {key}, got {outputs:?}"
        );
    }
    outputs
}

/// The audit configuration the plugin reports on the domain at `endpoint`, read through
/// `path`.
async fn audit_configuration(
    router: &MultiServiceRouter,
    case: &str,
    endpoint: &str,
    path: &str,
) -> Value {
    let (status, body) = at_endpoint(router, endpoint, "GET", path, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the audit configuration to be readable at {path}, got {status} \
         with {body}"
    );
    body["config"].clone()
}

/// The patch a client's setup step writes: auditing on, both layers on, no category excluded.
///
/// `pub(crate)` for the started-emulator sibling, whose setup step is the same one.
pub(crate) fn enabling_patch() -> Value {
    json!([
        { "op": "replace", "path": "/config/enabled", "value": true },
        { "op": "replace", "path": "/config/audit/enable_rest", "value": true },
        { "op": "replace", "path": "/config/audit/enable_transport", "value": true },
        { "op": "replace", "path": "/config/audit/disabled_rest_categories", "value": [] },
        { "op": "replace", "path": "/config/audit/disabled_transport_categories", "value": [] },
    ])
}

/// Whether the configuration reports auditing enabled on both layers with no category
/// excluded — what the patch above asks for, and what a second run must read back.
pub(crate) fn is_enabled(config: &Value) -> bool {
    config["enabled"] == json!(true)
        && config["audit"]["enable_rest"] == json!(true)
        && config["audit"]["enable_transport"] == json!(true)
        && config["audit"]["disabled_rest_categories"] == json!([])
        && config["audit"]["disabled_transport_categories"] == json!([])
}

/// The field names a record carries, so a case can say that none of them reports the ANSWER
/// the recorded request got.
fn field_names(record: &Value) -> BTreeSet<String> {
    record
        .as_object()
        .map(|members| members.keys().cloned().collect())
        .unwrap_or_default()
}

/// Every field of `record` whose NAME would report the answer the request got — the status it
/// was answered with, the response, the code — which an audit record of a decision carries
/// none of.
///
/// `pub(crate)` for the started-emulator sibling, which reads the same records.
pub(crate) fn fields_reporting_the_answer(record: &Value) -> Vec<String> {
    field_names(record)
        .into_iter()
        .filter(|field| {
            ["status", "response", "code"]
                .iter()
                .any(|answered| field.contains(answered))
        })
        .collect()
}

/// Every record of `records` at `layer`.
pub(crate) fn at_layer(records: &[Value], layer: &str) -> Vec<Value> {
    records
        .iter()
        .filter(|record| record["audit_request_layer"] == json!(layer))
        .cloned()
        .collect()
}

#[tokio::test]
async fn a_deployed_audited_domain_serves_its_data_plane_and_audits_a_document_read() {
    let router = build_router();
    let master_user = caller_arn(&router).await;

    for row in rows() {
        let case = row.case;
        let outputs = deploy(&router, &row, &master_user).await;
        let domain = outputs
            .get("DomainName")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the deployment published no domain name"));
        let endpoint = outputs.get("DomainEndpoint").cloned().unwrap_or_default();

        // The owning service holds a SETTLED domain at the address the stack published: a
        // completed deployment is one whose domain a client can use.
        let (status, status_body) = described(&router, &domain).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the owning service to report the deployed domain, got \
             {status} with {status_body}"
        );
        assert_eq!(
            (
                status_body["Created"].as_bool(),
                status_body["Processing"].as_bool(),
                status_body["Endpoint"].as_str(),
            ),
            (Some(true), Some(false), Some(endpoint.as_str())),
            "case {case:?}: expected a created, settled domain published at the stack's own \
             endpoint output {endpoint:?}, got {status_body}"
        );
        for (log_type, logical_id) in row.published {
            let published = status_body["LogPublishingOptions"][log_type].clone();
            assert_eq!(
                (
                    published["Enabled"].as_bool(),
                    published["CloudWatchLogsLogGroupArn"].as_str(),
                ),
                (
                    Some(true),
                    Some(group_attribute_arn(&group_name(row.stack, logical_id)).as_str()),
                ),
                "case {case:?}: expected the domain to publish {log_type} to the group the \
                 stack made, got {published}"
            );
        }

        // The published address serves the cluster's own banner for the domain the stack
        // created.
        let (status, banner) = at_endpoint(&router, &endpoint, "GET", "/", None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a signed GET / on {endpoint:?} to be served, got {status} \
             with {banner}"
        );
        assert_eq!(
            banner["name"].as_str(),
            Some(domain.as_str()),
            "case {case:?}: expected the banner to name the deployed domain, got {banner}"
        );

        // The plugin's audit configuration is readable, starts switched off, and is written by
        // the patch a client's setup step sends — idempotently, so a second run reads it back
        // already enabled.
        let before = audit_configuration(&router, case, &endpoint, row.audit_path).await;
        assert!(
            !is_enabled(&before),
            "case {case:?}: expected auditing to start switched off, got {before}"
        );
        for run in ["the first run", "a second, identical run"] {
            let (status, answer) = at_endpoint(
                &router,
                &endpoint,
                "PATCH",
                row.audit_path,
                Some(enabling_patch()),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} ({run}): expected the enabling patch to be taken, got {status} \
                 with {answer}"
            );
            let reread = audit_configuration(&router, case, &endpoint, row.audit_path).await;
            assert!(
                is_enabled(&reread),
                "case {case:?} ({run}): expected auditing enabled on both layers with no \
                 category excluded, got {reread}"
            );
        }

        // An index the cluster does not hold is created, and the identical repeat is refused
        // as an index that already exists.
        let index_path = format!("/{}", row.index);
        let (status, created) = at_endpoint(
            &router,
            &endpoint,
            "PUT",
            &index_path,
            Some(json!({ "mappings": { "properties": { "title": { "type": "text" } } } })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PUT {index_path} to create the index, got {status} with \
             {created}"
        );
        let (status, refused) =
            at_endpoint(&router, &endpoint, "PUT", &index_path, Some(json!({}))).await;
        assert_eq!(
            (status, refused["error"]["type"].as_str()),
            (400, Some("resource_already_exists_exception")),
            "case {case:?}: expected the repeat of PUT {index_path} to be refused, got {status} \
             with {refused}"
        );

        // The document the audited reads address.
        let document_path = format!("/{}/_doc/{}", row.index, row.document);
        let (status, written) = at_endpoint(
            &router,
            &endpoint,
            "PUT",
            &format!("{document_path}?refresh=true"),
            Some(json!({ "title": "an audited document" })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected the seed document to be created, got {status} with {written}"
        );

        // From here on the records the group holds are the two reads' own.
        let audit_group = group_name(row.stack, LOG_TYPES[0].1);
        let baseline = delivered(&router, case, &audit_group).await.len();

        let (status, found) = at_endpoint(&router, &endpoint, "GET", &document_path, None).await;
        assert_eq!(
            (status, found["found"].as_bool()),
            (200, Some(true)),
            "case {case:?}: expected the seeded document to be read back, got {status} with \
             {found}"
        );
        let absent_path = format!("/{}/_doc/{}", row.index, row.absent);
        let (status, missed) = at_endpoint(&router, &endpoint, "GET", &absent_path, None).await;
        assert_eq!(
            (status, missed["found"].as_bool()),
            (404, Some(false)),
            "case {case:?}: expected a read of an id the index does not hold to miss, got \
             {status} with {missed}"
        );

        // Each read left ONE record per layer in the group the AUDIT_LOGS option names: who
        // the caller is, at the REST layer, and what it may do, at the transport layer.
        let records = delivered_at_least(&router, case, &audit_group, baseline + 4).await;
        let reads: Vec<Value> = records.into_iter().skip(baseline).collect();
        assert_eq!(
            reads.len(),
            4,
            "case {case:?}: expected one REST and one transport record per read, got {reads:#?}"
        );
        let rest_records = at_layer(&reads, "REST");
        let transport_records = at_layer(&reads, "TRANSPORT");
        assert_eq!(
            (rest_records.len(), transport_records.len()),
            (2, 2),
            "case {case:?}: expected two records at each layer, got {reads:#?}"
        );
        for (read, path) in [("the served read", &document_path), ("the missed read", &absent_path)]
        {
            let of_read: Vec<&Value> = rest_records
                .iter()
                .filter(|record| record["audit_rest_request_path"] == json!(path))
                .collect();
            assert_eq!(
                of_read.len(),
                1,
                "case {case:?}: expected exactly one REST record of {read} ({path}), got \
                 {rest_records:#?}"
            );
            assert_eq!(
                (
                    of_read[0]["audit_category"].as_str(),
                    of_read[0]["audit_rest_request_method"].as_str(),
                    of_read[0]["audit_request_effective_user"].as_str(),
                ),
                (
                    Some(CATEGORY_AUTHENTICATED),
                    Some("GET"),
                    Some(master_user.as_str()),
                ),
                "case {case:?}: expected the REST record of {read} to state the admitted \
                 caller, got {:#?}",
                of_read[0]
            );
        }
        for record in &transport_records {
            assert_eq!(
                (
                    record["audit_category"].as_str(),
                    record["audit_transport_request_type"].as_str(),
                    record["audit_trace_resolved_indices"].clone(),
                ),
                (
                    Some(CATEGORY_GRANTED_PRIVILEGES),
                    Some(GET_REQUEST),
                    json!([row.index]),
                ),
                "case {case:?}: expected the transport record to state the granted read, got \
                 {record:#?}"
            );
        }

        // The record of the served read and the record of the missed one are the same record
        // outside the time they were taken and the id the request addressed: an audit record
        // reports the DECISION the cluster took, never the answer the caller got.
        let volatile = ["@timestamp", "audit_rest_request_path"];
        let normalised = |record: &Value| {
            let mut copy = record.clone();
            let members = copy.as_object_mut().expect("a record is an object");
            for field in volatile {
                members.remove(field);
            }
            copy
        };
        assert_eq!(
            normalised(&rest_records[0]),
            normalised(&rest_records[1]),
            "case {case:?}: expected the two reads' REST records to differ only in the time and \
             the id addressed, got {rest_records:#?}"
        );
        assert_eq!(
            normalised(&transport_records[0]),
            normalised(&transport_records[1]),
            "case {case:?}: expected the two reads' transport records to differ only in the \
             time, got {transport_records:#?}"
        );
        for record in &reads {
            let leaking = fields_reporting_the_answer(record);
            assert!(
                leaking.is_empty(),
                "case {case:?}: an audit record must carry no field reporting the answer the \
                 request got, got {leaking:?} in {record:#?}"
            );
        }

        // ...and no OTHER group the domain publishes to mentions either document: the audit
        // log is the only log of a request for access to the domain.
        for (log_type, logical_id) in row.published.iter().filter(|(log_type, _)| {
            *log_type != AUDIT_LOG_TYPE
        }) {
            let group = group_name(row.stack, logical_id);
            let held = delivered(&router, case, &group).await;
            let mentioning: Vec<&Value> = held
                .iter()
                .filter(|record| {
                    let text = record.to_string();
                    text.contains(row.document) || text.contains(row.absent)
                })
                .collect();
            assert!(
                mentioning.is_empty(),
                "case {case:?}: expected the {log_type} group {group:?} to mention neither \
                 document, got {mentioning:#?}"
            );
        }
    }
}

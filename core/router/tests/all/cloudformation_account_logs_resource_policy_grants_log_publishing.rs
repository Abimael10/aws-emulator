// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that grants Amazon OpenSearch Service log delivery with an
//! ACCOUNT-scoped `AWS::Logs::ResourcePolicy` and then declares the domain that publishes to
//! the granted groups — end to end over the real `MultiServiceRouter` with CloudFormation,
//! Amazon OpenSearch Service and Amazon CloudWatch Logs on one engine.
//!
//! The RULE pinned here: **a resource type this emulator supports is only real if the
//! service that OWNS it created it, so an `AWS::Logs::ResourcePolicy` is PUT in Amazon
//! CloudWatch Logs — `PutResourcePolicy` with no `resourceArn`, which is that service's
//! account-scoped policy — before the resources that depend on the grant are created.** The
//! policies in force for a log group are the one attached to it AND the account-scoped ones
//! covering it, so a domain whose `LogPublishingOptions` name those groups is accepted on
//! that grant alone and the stack completes; a stack that declares the grant nowhere, or one
//! whose policy grants less than delivery needs, has its domain refused by the owning
//! service and rolls back.
//!
//! The rows differ along what the rule generalises over: the FORM the granted resource is
//! written in (the log group's ARN as `Fn::GetAtt … Arn` reports it, which ends in the
//! wildcard the group's streams are named by, and the plain group ARN composed with
//! `Fn::Sub`), which of the two actions delivery needs the policy grants, how many log types
//! the domain publishes, and both directions of the rule — a stack carrying the grant and
//! one carrying none.
//!
//! Three crates have to agree for that to hold — CloudFormation plans and makes the calls,
//! CloudWatch Logs decides which policies are in force, and Amazon OpenSearch Service judges
//! the domain against them — so the suite lives here.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`), and the stack-status polling every suite that deploys a stack
// shares (`tests/all/cloudformation_firehose_stream_stabilisation.rs`).
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};

/// The Region every request here is signed for — the Region the shared stack poller signs
/// for too, so one estate is read by one Region.
const REGION: &str = "us-east-1";

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// CloudWatch Logs' `X-Amz-Target` prefix, from its model's service shape, and the signing
/// name its credential scope carries.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// The service principal a domain's logs are delivered under, which the declared policy
/// grants.
const DELIVERY_PRINCIPAL: &str = "es.amazonaws.com";

/// The parameter the templates take the policy's name from, which is what that policy is
/// identified by in the account.
const POLICY_NAME_PARAMETER: &str = "PolicyName";

/// The two streams the service's create-time probe of a granted destination is made of, and
/// the one event each carries — the proof, inside the groups the stack made, that the grant
/// the stack declared really admits the delivery.
const CALLER_PROBE_STREAM: &str = "cust-test-log-stream";
const SERVICE_PROBE_STREAM: &str = "es-test-log-stream";
const PROBE_MESSAGE: &str = "Amazon OpenSearch Service created a test log event";

/// The four log types a domain publishes, each to a log group of its own: `LogType`'s whole
/// value set (`smithy_query(query="shape",shape="LogType")`), paired with the logical id of
/// the group the template declares for it.
const LOG_TYPES: [(&str, &str); 4] = [
    ("AUDIT_LOGS", "AuditLogGroup"),
    ("ES_APPLICATION_LOGS", "AppLogGroup"),
    ("INDEX_SLOW_LOGS", "SlowIndexLogGroup"),
    ("SEARCH_SLOW_LOGS", "SlowSearchLogGroup"),
];

/// The logical id of the declared resource policy, and of the domain that depends on it.
const POLICY: &str = "LogDeliveryPolicy";
const DOMAIN: &str = "OpenSearchDomain";

/// The three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
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

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one CloudWatch Logs operation over its awsJson1_1 wire.
async fn logs(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{LOGS_SIGNING_NAME}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(LOGS_SIGNING_NAME))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Call one Amazon OpenSearch Service operation over its `restJson1` wire.
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

/// Every resource failure the stack recorded, so a case can say WHY a stack did not
/// complete.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&body, "ResourceStatusReason")
}

/// The account-scoped resource policy CloudWatch Logs holds under `name`, as
/// `DescribeResourcePolicies` reports it — `None` when that service holds none.
async fn account_policy(router: &MultiServiceRouter, case: &str, name: &str) -> Option<Value> {
    let (status, parsed) = logs(router, "DescribeResourcePolicies", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeResourcePolicies, got {status} with \
         {parsed}"
    );
    parsed
        .get("resourcePolicies")
        .and_then(Value::as_array)
        .and_then(|policies| {
            policies
                .iter()
                .find(|policy| policy.get("policyName").and_then(Value::as_str) == Some(name))
                .cloned()
        })
}

/// The stream names CloudWatch Logs holds in `group`, sorted so a case compares a SET.
async fn stream_names(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, parsed) = logs(
        router,
        "DescribeLogStreams",
        &json!({"logGroupName": group}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeLogStreams for {group:?}, got {status} \
         with {parsed}"
    );
    let mut names: Vec<String> = parsed
        .get("logStreams")
        .and_then(Value::as_array)
        .map(|streams| {
            streams
                .iter()
                .filter_map(|stream| {
                    stream
                        .get("logStreamName")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The messages of the events `group`/`stream` holds.
async fn stream_messages(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    stream: &str,
) -> Vec<String> {
    let (status, parsed) = logs(
        router,
        "GetLogEvents",
        &json!({"logGroupName": group, "logStreamName": stream}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetLogEvents for {group:?}/{stream:?}, got \
         {status} with {parsed}"
    );
    parsed
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| {
                    event
                        .get("message")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// How a row's declared policy names the log groups it grants, which is the one thing the
/// granted rows differ in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GrantedResource {
    /// `Fn::GetAtt <group>.Arn` — the ARN the type publishes for a log group, which ends in
    /// the wildcard that names the group's log streams.
    GroupAttributeArn,
    /// The plain group ARN composed with `Fn::Sub` from the account, the Region and the
    /// group's own name, which is the other form a template writes it in.
    ComposedGroupArn,
}

/// One row: the policy the stack declares (or declares not at all), and the status the rule
/// says the stack settles in.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The resource the declared policy grants on — `None` for a stack that declares no
    /// policy resource at all.
    granted: Option<GrantedResource>,
    /// The actions the declared policy grants. Delivery needs both; a policy carrying one
    /// lets no record arrive.
    actions: &'static [&'static str],
    expected_status: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        // The recorded shape: an account-scoped policy over the ARNs the groups publish, and
        // a domain publishing all four log types to them.
        Row {
            case: "an account-scoped policy over the ARNs the groups publish",
            stack: "osds-granted-attribute-arn",
            granted: Some(GrantedResource::GroupAttributeArn),
            actions: &["logs:PutLogEvents", "logs:CreateLogStream"],
            expected_status: CREATE_COMPLETE,
        },
        // The same grant written over the PLAIN group ARN: the group and its streams are the
        // two spellings of one destination, so either admits the delivery.
        Row {
            case: "an account-scoped policy over the plain group ARNs",
            stack: "osds-granted-composed-arn",
            granted: Some(GrantedResource::ComposedGroupArn),
            actions: &["logs:CreateLogStream", "logs:PutLogEvents"],
            expected_status: CREATE_COMPLETE,
        },
        // No grant anywhere: the domain is refused by the owning service and the stack rolls
        // back, which is what a missing provider for the policy type looks like.
        Row {
            case: "no resource policy declared at all",
            stack: "osds-ungranted",
            granted: None,
            actions: &[],
            expected_status: ROLLBACK_COMPLETE,
        },
        // A grant carrying only one of the two actions: delivery needs both, so the domain
        // is refused on it.
        Row {
            case: "a policy granting only logs:PutLogEvents",
            stack: "osds-half-granted",
            granted: Some(GrantedResource::GroupAttributeArn),
            actions: &["logs:PutLogEvents"],
            expected_status: ROLLBACK_COMPLETE,
        },
    ]
}

/// The log group name the template declares for `logical_id` under `stack`.
fn group_name(stack: &str, logical_id: &str) -> String {
    format!("/aws/opensearch/{stack}/{}", logical_id.to_ascii_lowercase())
}

/// The template a row deploys: four log groups, the row's resource policy over them, and the
/// domain that publishes one log type to each — declared `DependsOn` the policy, so the grant
/// is in force before the domain is created.
fn template(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    for (_, logical_id) in LOG_TYPES {
        resources.insert(
            logical_id.to_string(),
            json!({
                "Type": "AWS::Logs::LogGroup",
                "Properties": {
                    "LogGroupName": group_name(row.stack, logical_id),
                    "RetentionInDays": 3653,
                },
            }),
        );
    }
    let mut depends_on: Vec<String> = LOG_TYPES
        .iter()
        .map(|(_, logical_id)| (*logical_id).to_string())
        .collect();
    if let Some(granted) = row.granted {
        // Every grant is written as the JSON string the type's `PolicyDocument` is, with the
        // group ARNs substituted by the template itself.
        let resource_list = LOG_TYPES
            .iter()
            .map(|(_, logical_id)| match granted {
                GrantedResource::GroupAttributeArn => format!("\\\"${{{logical_id}.Arn}}\\\""),
                GrantedResource::ComposedGroupArn => format!(
                    "\\\"arn:${{AWS::Partition}}:logs:${{AWS::Region}}:${{AWS::AccountId}}:\
                     log-group:${{{logical_id}}}\\\""
                ),
            })
            .collect::<Vec<String>>()
            .join(",");
        let actions = row
            .actions
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
            .expect("the policy document is one JSON string");
        resources.insert(
            POLICY.to_string(),
            json!({
                "Type": "AWS::Logs::ResourcePolicy",
                "DependsOn": depends_on.clone(),
                "Properties": {
                    "PolicyName": {"Ref": POLICY_NAME_PARAMETER},
                    "PolicyDocument": {"Fn::Sub": document},
                },
            }),
        );
        depends_on = vec![POLICY.to_string()];
    }
    let mut options = serde_json::Map::new();
    for (log_type, logical_id) in LOG_TYPES {
        options.insert(
            log_type.to_string(),
            json!({
                "Enabled": true,
                "CloudWatchLogsLogGroupArn": {"Fn::GetAtt": [logical_id, "Arn"]},
            }),
        );
    }
    resources.insert(
        DOMAIN.to_string(),
        json!({
            "Type": "AWS::OpenSearchService::Domain",
            "DependsOn": depends_on,
            "Properties": {
                "EngineVersion": "OpenSearch_2.11",
                "ClusterConfig": {"InstanceType": "t3.small.search", "InstanceCount": 1},
                "EBSOptions": {"EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10},
                "EncryptionAtRestOptions": {"Enabled": true},
                "NodeToNodeEncryptionOptions": {"Enabled": true},
                "DomainEndpointOptions": {"EnforceHTTPS": true},
                "AdvancedSecurityOptions": {
                    "Enabled": true,
                    "InternalUserDatabaseEnabled": false,
                    "MasterUserOptions": {
                        "MasterUserARN": {
                            "Fn::Sub": "arn:${AWS::Partition}:iam::${AWS::AccountId}:user/search-master",
                        },
                    },
                },
                "AccessPolicies": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": {"AWS": {"Fn::Sub": "arn:${AWS::Partition}:iam::${AWS::AccountId}:root"}},
                        "Action": "es:*",
                        "Resource": "*",
                    }],
                },
                "LogPublishingOptions": options,
            },
        }),
    );
    let mut outputs = json!({
        "OpenSearchDomainName": {"Value": {"Ref": DOMAIN}},
        "OpenSearchDomainEndpoint": {"Value": {"Fn::GetAtt": [DOMAIN, "DomainEndpoint"]}},
        "OpenSearchDomainArn": {"Value": {"Fn::GetAtt": [DOMAIN, "Arn"]}},
    });
    for (_, logical_id) in LOG_TYPES {
        outputs[format!("{logical_id}Name")] = json!({"Value": {"Ref": logical_id}});
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a granted search domain and the groups it publishes to",
        "Parameters": {POLICY_NAME_PARAMETER: {"Type": "String"}},
        "Resources": Value::Object(resources),
        "Outputs": outputs,
    })
    .to_string()
}

#[tokio::test]
async fn an_account_scoped_resource_policy_grants_the_declared_domains_log_publishing() {
    let router = build_router();
    for row in rows() {
        let case = row.case;
        let policy_name = format!("{}-delivery", row.stack);
        let body = template(&row);

        let (status, answer) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &body),
                ("Parameters.member.1.ParameterKey", POLICY_NAME_PARAMETER),
                ("Parameters.member.1.ParameterValue", &policy_name),
            ],
        )
        .await;
        assert!(
            status == 200 && answer.contains("StackId"),
            "case {case:?}: expected CreateStack to be accepted, got {status} with {answer}"
        );
        // The id the create answered, which is what addresses the stack once its deletion has
        // completed: "Deleted stacks: You must specify the unique stack ID"
        // (`DescribeStacksInput$StackName`).
        let stack_id = crate::query_wire::opt_element(&answer, "StackId").unwrap_or_default();
        let (reported, settled) =
            wait_for_stack_status(&router, row.stack, row.expected_status).await;
        assert_eq!(
            reported, row.expected_status,
            "case {case:?}: expected the stack to settle in {:?}, got {settled} with the \
             resource failures {:?}",
            row.expected_status,
            failure_reasons(&router, &stack_id).await
        );

        let outputs = outputs_of(&settled);
        if row.expected_status != CREATE_COMPLETE {
            // A rolled-back stack publishes nothing, and the domain it declared does not
            // exist: the grant the template wrote was not one delivery could be made on.
            assert!(
                outputs.is_empty(),
                "case {case:?}: expected a rolled-back stack to publish no outputs, got \
                 {outputs:?}"
            );
            continue;
        }

        // The policy is really PUT: CloudWatch Logs reports it under the name the template
        // declared, with the document and the account scope.
        let held = account_policy(&router, case, &policy_name)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected CloudWatch Logs to hold the account-scoped policy \
                     {policy_name:?}"
                )
            });
        assert_eq!(
            held.get("policyScope").and_then(Value::as_str),
            Some("ACCOUNT"),
            "case {case:?}: expected the declared policy to be account-scoped, got {held}"
        );
        let document = held
            .get("policyDocument")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            document.contains(DELIVERY_PRINCIPAL)
                && row.actions.iter().all(|action| document.contains(action)),
            "case {case:?}: expected the stored document to grant {DELIVERY_PRINCIPAL} the \
             actions {:?}, got {document:?}",
            row.actions
        );

        // The stack publishes the domain, its endpoint and its ARN, and the four group names.
        for key in [
            "OpenSearchDomainName",
            "OpenSearchDomainEndpoint",
            "OpenSearchDomainArn",
        ] {
            assert!(
                outputs.get(key).is_some_and(|value| !value.is_empty()),
                "case {case:?}: expected the stack to publish a non-empty {key}, got {outputs:?}"
            );
        }
        let domain_name = outputs
            .get("OpenSearchDomainName")
            .cloned()
            .unwrap_or_default();

        // The domain is really there, publishing to the groups the stack made.
        let (status, described) = opensearch(
            &router,
            "GET",
            &format!("/2021-01-01/opensearch/domain/{domain_name}"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the owning service to report the domain, got {status} with \
             {described}"
        );
        for (log_type, logical_id) in LOG_TYPES {
            let group = group_name(row.stack, logical_id);
            assert_eq!(
                outputs.get(&format!("{logical_id}Name")).map(String::as_str),
                Some(group.as_str()),
                "case {case:?}: expected the stack to publish the {log_type} group's name, got \
                 {outputs:?}"
            );
            let published = described["DomainStatus"]["LogPublishingOptions"][log_type].clone();
            assert_eq!(
                published["Enabled"].as_bool(),
                Some(true),
                "case {case:?}: expected the domain to publish {log_type}, got {published}"
            );
            // ...and each granted group holds the service's create-time probe: both test
            // streams, with one test event each, which is what says the grant admits the
            // delivery.
            let streams = stream_names(&router, case, &group).await;
            assert_eq!(
                streams,
                vec![
                    CALLER_PROBE_STREAM.to_string(),
                    SERVICE_PROBE_STREAM.to_string()
                ],
                "case {case:?}: expected the {log_type} group {group:?} to hold both probe \
                 streams, got {streams:?}"
            );
            for stream in [CALLER_PROBE_STREAM, SERVICE_PROBE_STREAM] {
                let messages = stream_messages(&router, case, &group, stream).await;
                assert_eq!(
                    messages,
                    vec![PROBE_MESSAGE.to_string()],
                    "case {case:?}: expected {group:?}/{stream:?} to hold one test event, got \
                     {messages:?}"
                );
            }
        }

        // Tearing the stack down takes the policy off the account again: the resource ceases
        // to exist where it was really made.
        let (status, answer) =
            cloudformation(&router, "DeleteStack", &[("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {answer}"
        );
        let (reported, torn_down) =
            wait_for_stack_status(&router, &stack_id, DELETE_COMPLETE).await;
        assert_eq!(
            reported, DELETE_COMPLETE,
            "case {case:?}: expected the stack to be deleted, got {torn_down} with the resource \
             failures {:?}",
            failure_reasons(&router, &stack_id).await
        );
        assert_eq!(
            account_policy(&router, case, &policy_name).await,
            None,
            "case {case:?}: expected the deleted stack's policy to be gone from the account"
        );
    }
}

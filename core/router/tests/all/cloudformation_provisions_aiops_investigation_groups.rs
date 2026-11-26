// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::AIOps::InvestigationGroup` deploys: the
//! investigation group is created in Amazon CloudWatch investigations' own control plane, the
//! stack publishes the ARN that service minted, re-declaring the group's configuration changes
//! the group that service holds, deleting the stack takes it away — and a declaration that
//! service REFUSES fails the resource with its own refusal and rolls the stack back, on the
//! create and on a later update alike.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what the stack
//! publishes is what that service answered, and what that service refuses is a FAILED RESOURCE
//! rather than a template-format error.** Four crates have to agree for that to hold
//! (CloudFormation submits the template and makes the calls, CloudWatch investigations serves
//! them, IAM holds the role a group gathers its data as, and KMS holds the customer managed key
//! it encrypts that data with), so the suite lives here: a crate-local test can only ever see
//! one side of it.
//!
//! The rows of the deploy table differ along what the rule generalises over: which of the
//! type's properties the declaration states (the role alone; the retention, the CloudTrail flag
//! and the tag key boundaries; an encryption configuration naming a customer managed key the
//! same stack creates; the chat channels and the cross-account configurations; a resource
//! policy; the type's own `Tags`), how many values a collection property carries, which
//! published return value the output reads (`Ref`, `Fn::GetAtt Arn`, `Fn::GetAtt CreatedAt`,
//! `Fn::GetAtt CreatedBy`) and the template spelling (JSON with long-form intrinsics, YAML with
//! the short-form tags).
//!
//! The refusal rows differ along the CLASS of declaration that service refuses: a role ARN
//! naming no role of this account, a role this account's IAM holds without a trust policy
//! admitting the investigations service, an encryption configuration whose two members disagree
//! in either direction, and a cross-account source role belonging to the investigation account
//! itself. Each is the owning service's own refusal, reported on the resource that failed —
//! never the `Template format error: Unrecognized resource types` a type CloudFormation does
//! not know is refused with, which no row of any table here may ever see.
//!
//! Nothing here waits a fixed time: a stack's transition is clock-owed, so every wait polls the
//! condition until it holds or a generous deadline passes ([`poll_until`], [`settled_stack`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery percent-encoder and the XML readers every awsQuery suite in this crate shares,
// written once in `all/query_wire.rs` so no suite re-derives the encoding or the reading its
// assertions depend on.
use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stack and the group it
/// deploys belong to.
const REGION: &str = "us-east-1";

/// How long a clock-owed transition is waited for before the wait is a failure: generously more
/// than the spans a stack owes, so a loaded host still reaches the condition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);

/// How often a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Amazon CloudWatch investigations' signing name, which is also its ARN namespace
/// (`sigv4Name: aiops`, `awsService.arnNamespace: aiops`).
const AIOPS: &str = "aiops";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The resource type under test, and the logical ids every template below declares its
/// resources under.
const INVESTIGATION_GROUP_TYPE: &str = "AWS::AIOps::InvestigationGroup";
const GROUP: &str = "Investigations";
const ROLE: &str = "InvestigationRole";
const KEY: &str = "InvestigationKey";

/// The service principal the role an investigation group gathers its data as has to admit,
/// which is this service's own (`aiops.amazonaws.com`).
const INVESTIGATIONS_PRINCIPAL: &str = service_com_amazonaws_aiops_aiops::state::SERVICE_PRINCIPAL;

/// The sentence that service refuses a write naming a role it could not assume with, read from
/// its own crate rather than restated here so the suite and the service cannot drift apart.
fn role_not_assumable() -> &'static str {
    service_com_amazonaws_aiops_aiops::state::ROLE_NOT_ASSUMABLE
}

/// The ARN prefix an investigation group of this Region carries (the modelled
/// `InvestigationGroupArn` pattern
/// `^arn:…:aiops:[a-zA-Z0-9-]*:[0-9]{12}:investigation-group\/[A-Za-z0-9]{16}$`).
fn group_arn_prefix() -> String {
    format!(
        "arn:aws:aiops:{REGION}:{}:investigation-group/",
        engine::emulator_account_id()
    )
}

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: CloudFormation, the CloudWatch investigations control
/// plane the stack calls, IAM — because that service resolves the role a group gathers its data
/// as against this account's IAM records before it stores the group — and KMS, because it
/// resolves a customer managed key the same way.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_aiops_aiops::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
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

/// Call one CloudWatch investigations operation over its own restJson1 wire, on the method and
/// URI its `@http` binding names.
async fn aiops(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|document| document.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("aiops.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(AIOPS))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `GetInvestigationGroup` for one identifier. The ARN is written into the single path segment
/// the `@httpLabel` binds, escaped the way an AWS SDK escapes one — its `:` and `/` included —
/// so it addresses one group rather than another path.
async fn get_group(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    let uri = format!("/investigationGroups/{}", form_encode(identifier));
    aiops(router, "GET", &uri, None).await
}

/// `GetInvestigationGroupPolicy` for one identifier: the group's resource policy, which that
/// service answers its own not-found for when the group carries none.
async fn get_group_policy(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    let uri = format!("/investigationGroups/{}/policy", form_encode(identifier));
    aiops(router, "GET", &uri, None).await
}

/// The ARNs `ListInvestigationGroups` reports for this account and Region.
async fn listed_groups(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, answered) = aiops(router, "GET", "/investigationGroups", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListInvestigationGroups, got {status} with \
         {answered}"
    );
    answered
        .get("investigationGroups")
        .and_then(Value::as_array)
        .map(|listed| {
            listed
                .iter()
                .filter_map(|summary| summary.get("arn").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The tags that service itself holds for one resource, read through its own
/// `ListTagsForResource` — which answers them as a MAP.
async fn held_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> BTreeMap<String, String> {
    let uri = format!("/tags/{}", form_encode(arn));
    let (status, answered) = aiops(router, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} with {answered}"
    );
    answered
        .get("tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look again until `condition` holds or [`SETTLE_DEADLINE`] passes, panicking with `case` and
/// whatever the last look saw.
///
/// Every wait in this suite goes through here: what is being waited for is a CONDITION the
/// clock makes true, never an elapsed time, so nothing in this file sleeps for a span and then
/// asserts.
async fn poll_until<F, Fut>(case: &str, waiting_for: &str, mut look: F) -> String
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = (bool, String)>,
{
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (held, seen) = look().await;
        if held {
            return seen;
        }
        if Instant::now() >= deadline {
            panic!("case {case:?}: timed out waiting for {waiting_for}; last saw {seen}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether a stack status is one a deployment STOPS in, rather than one it is passing through:
/// CloudFormation's own statuses end in `_IN_PROGRESS` while work is outstanding.
fn is_terminal(stack_status: &str) -> bool {
    stack_status.ends_with("_COMPLETE") || stack_status.ends_with("_FAILED")
}

/// Poll `DescribeStacks` until the stack rests in a terminal status, and answer that body.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    poll_until(case, "the stack to rest in a terminal status", || async {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        if status != 200 {
            return (false, format!("HTTP {status} from DescribeStacks: {body}"));
        }
        let settled = opt_element(&body, "StackStatus").is_some_and(|it| is_terminal(&it));
        (settled, body)
    })
    .await
}

/// Create a stack, acknowledging the IAM resources every template here declares, then wait for
/// it to settle and answer the `DescribeStacks` body.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", template),
            // "If you have IAM resources with custom names, you must specify
            // CAPABILITY_NAMED_IAM", which acknowledges both forms.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an \
         AWS::AIOps::InvestigationGroup is a type a template may declare — got {status} with \
         body {body}"
    );
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected the type to be recognised, got {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// One output the stack must report, as the text it carries.
fn published(case: &str, body: &str, key: &str) -> String {
    outputs_of(body)
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {body}"))
        .clone()
}

/// The IAM role a group gathers its investigation data as, as a resource of the template: that
/// service resolves it against this account's IAM before it stores the group, so the role has
/// to be one IAM really holds with a trust policy admitting `principal`.
fn investigation_role(principal: &str) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": {"Service": principal},
                    "Action": "sts:AssumeRole",
                }],
            },
        },
    })
}

/// The resource policy a declaration may state for the group: the document that lets CloudWatch
/// alarms start investigations in it, which is what `PutInvestigationGroupPolicy` exists for.
fn investigation_group_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "aiops.alarms.cloudwatch.amazonaws.com"},
            "Action": ["aiops:CreateInvestigation", "aiops:CreateInvestigationEvent"],
            "Resource": "*",
        }],
    })
}

/// What one row expects of the group its stack declares.
struct Declared {
    /// The output key whose value is the ARN that service minted.
    arn_output: &'static str,
    /// The output keys whose values must be the same ARN, where the row reads more than one.
    same_arn_outputs: Vec<&'static str>,
    /// The output key whose value is a read-only value that service reports, with the member it
    /// reports it under — where the row reads one.
    reported_output: Option<(&'static str, &'static str)>,
    /// The `name` the group must carry.
    name: &'static str,
    /// The `retentionInDays` it must report.
    retention_in_days: i64,
    /// The `isCloudTrailEventHistoryEnabled` it must report.
    cloud_trail_enabled: bool,
    /// The `tagKeyBoundaries` it must report, in declaration order.
    tag_key_boundaries: Vec<&'static str>,
    /// The `crossAccountConfigurations` source roles it must report, in declaration order.
    cross_account_roles: Vec<String>,
    /// The chat channels it must report, as `(topic, configurations)` pairs.
    channels: Vec<(String, Vec<String>)>,
    /// The `encryptionConfiguration.type` it must report.
    encryption_type: &'static str,
    /// Whether the encryption configuration must name a customer managed key — the one the same
    /// stack created, whose ARN is published as `KeyArn`.
    names_the_stack_key: bool,
    /// Whether the group must carry the declared resource policy.
    has_policy: bool,
    /// The tags the group must end up carrying.
    tags: BTreeMap<String, String>,
}

/// One row: a template and what the group it declares must be.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    declared: Declared,
}

/// The source role of another account a cross-account configuration may name: that service
/// refuses one belonging to the investigation account itself.
fn other_account_role(name: &str) -> String {
    format!("arn:aws:iam::111122223333:role/{name}")
}

/// An Amazon SNS topic ARN of this account, as a chat channel keys one by.
fn topic_arn(name: &str) -> String {
    format!(
        "arn:aws:sns:{REGION}:{}:{name}",
        engine::emulator_account_id()
    )
}

/// A chat application configuration ARN, as the array a channel's topic maps to holds one.
fn chat_configuration_arn(name: &str) -> String {
    format!(
        "arn:aws:chatbot::{}:chat-configuration/slack-channel/{name}",
        engine::emulator_account_id()
    )
}

fn rows() -> Vec<Row> {
    let cross_account = other_account_role("Investigator");
    let second_cross_account = other_account_role("SecondInvestigator");
    let topic = topic_arn("investigation-notifications");
    let configuration = chat_configuration_arn("ops");
    let second_configuration = chat_configuration_arn("oncall");
    vec![
        // The reported case: the two properties the type requires and nothing else, with both
        // published spellings of the identity read back, in JSON with long-form intrinsics.
        Row {
            case: "the required properties alone, both spellings of the identity read back",
            stack_name: "aiops-group-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    ROLE: investigation_role(INVESTIGATIONS_PRINCIPAL),
                    GROUP: {
                        "Type": INVESTIGATION_GROUP_TYPE,
                        "Properties": {
                            "Name": "retail-ops",
                            "RoleArn": {"Fn::GetAtt": [ROLE, "Arn"]},
                        }
                    }
                },
                "Outputs": {
                    "GroupRef": {"Value": {"Ref": GROUP}},
                    "GroupArn": {"Value": {"Fn::GetAtt": [GROUP, "Arn"]}},
                    "CreatedBy": {"Value": {"Fn::GetAtt": [GROUP, "CreatedBy"]}},
                    "RoleArn": {"Value": {"Fn::GetAtt": [ROLE, "Arn"]}},
                }
            })
            .to_string(),
            declared: Declared {
                arn_output: "GroupRef",
                same_arn_outputs: vec!["GroupArn"],
                reported_output: Some(("CreatedBy", "createdBy")),
                name: "retail-ops",
                // Neither property is declared, so each carries the value its own member
                // documents for an omission: "If you omit this parameter, the default of 90
                // days is used", and "The default is true".
                retention_in_days: 90,
                cloud_trail_enabled: true,
                tag_key_boundaries: Vec::new(),
                cross_account_roles: Vec::new(),
                channels: Vec::new(),
                encryption_type: "AWS_OWNED_KEY",
                names_the_stack_key: false,
                has_policy: false,
                tags: BTreeMap::new(),
            },
        },
        // The retention, the CloudTrail flag and three tag key boundaries stated, with the
        // type's own `Tags`, read back through `Fn::GetAtt Arn` and `Fn::GetAtt CreatedAt`.
        Row {
            case: "retention, the CloudTrail flag, three tag key boundaries and tags",
            stack_name: "aiops-group-configured",
            template: json!({
                "Resources": {
                    ROLE: investigation_role(INVESTIGATIONS_PRINCIPAL),
                    GROUP: {
                        "Type": INVESTIGATION_GROUP_TYPE,
                        "Properties": {
                            "Name": "payments (tier-1)",
                            "RoleArn": {"Fn::GetAtt": [ROLE, "Arn"]},
                            "RetentionInDays": 7,
                            "IsCloudTrailEventHistoryEnabled": false,
                            "TagKeyBoundaries": ["application", "service", "component"],
                            "Tags": [
                                {"Key": "team", "Value": "platform"},
                                {"Key": "tier", "Value": "1"},
                            ],
                        }
                    }
                },
                "Outputs": {
                    "GroupArn": {"Value": {"Fn::GetAtt": [GROUP, "Arn"]}},
                    "CreatedAt": {"Value": {"Fn::GetAtt": [GROUP, "CreatedAt"]}},
                    "RoleArn": {"Value": {"Fn::GetAtt": [ROLE, "Arn"]}},
                }
            })
            .to_string(),
            declared: Declared {
                arn_output: "GroupArn",
                same_arn_outputs: Vec::new(),
                reported_output: Some(("CreatedAt", "createdAt")),
                name: "payments (tier-1)",
                retention_in_days: 7,
                cloud_trail_enabled: false,
                tag_key_boundaries: vec!["application", "service", "component"],
                cross_account_roles: Vec::new(),
                channels: Vec::new(),
                encryption_type: "AWS_OWNED_KEY",
                names_the_stack_key: false,
                has_policy: false,
                tags: [
                    ("team".to_string(), "platform".to_string()),
                    ("tier".to_string(), "1".to_string()),
                ]
                .into_iter()
                .collect(),
            },
        },
        // An encryption configuration naming a CUSTOMER MANAGED key the same stack creates —
        // which that service resolves against KMS before it stores the group — beside a
        // resource policy, which is a call of its own in that service.
        Row {
            case: "a customer managed key the stack creates, and a resource policy",
            stack_name: "aiops-group-encrypted",
            template: json!({
                "Resources": {
                    ROLE: investigation_role(INVESTIGATIONS_PRINCIPAL),
                    KEY: {"Type": "AWS::KMS::Key", "Properties": {}},
                    GROUP: {
                        "Type": INVESTIGATION_GROUP_TYPE,
                        "Properties": {
                            "Name": "fraud-detection",
                            "RoleArn": {"Fn::GetAtt": [ROLE, "Arn"]},
                            "RetentionInDays": 30,
                            "EncryptionConfiguration": {
                                "EncryptionConfigurationType": "CUSTOMER_MANAGED_KMS_KEY",
                                "KmsKeyId": {"Fn::GetAtt": [KEY, "Arn"]},
                            },
                            "InvestigationGroupPolicy": investigation_group_policy(),
                        }
                    }
                },
                "Outputs": {
                    "GroupRef": {"Value": {"Ref": GROUP}},
                    "KeyArn": {"Value": {"Fn::GetAtt": [KEY, "Arn"]}},
                    "RoleArn": {"Value": {"Fn::GetAtt": [ROLE, "Arn"]}},
                }
            })
            .to_string(),
            declared: Declared {
                arn_output: "GroupRef",
                same_arn_outputs: Vec::new(),
                reported_output: None,
                name: "fraud-detection",
                retention_in_days: 30,
                cloud_trail_enabled: true,
                tag_key_boundaries: Vec::new(),
                cross_account_roles: Vec::new(),
                channels: Vec::new(),
                encryption_type: "CUSTOMER_MANAGED_KMS_KEY",
                names_the_stack_key: true,
                has_policy: true,
                tags: BTreeMap::new(),
            },
        },
        // TWO cross-account source roles and one chat channel carrying TWO configurations —
        // the two collection properties the type and the owning API spell differently.
        Row {
            case: "two cross-account source roles and a channel with two configurations",
            stack_name: "aiops-group-collections",
            template: json!({
                "Resources": {
                    ROLE: investigation_role(INVESTIGATIONS_PRINCIPAL),
                    GROUP: {
                        "Type": INVESTIGATION_GROUP_TYPE,
                        "Properties": {
                            "Name": "search-platform",
                            "RoleArn": {"Fn::GetAtt": [ROLE, "Arn"]},
                            "CrossAccountConfigurations": [
                                {"SourceRoleArn": cross_account.clone()},
                                {"SourceRoleArn": second_cross_account.clone()},
                            ],
                            "ChatbotNotificationChannels": [{
                                "SNSTopicArn": topic.clone(),
                                "ChatConfigurationArns": [
                                    configuration.clone(),
                                    second_configuration.clone(),
                                ],
                            }],
                        }
                    }
                },
                "Outputs": {
                    "GroupRef": {"Value": {"Ref": GROUP}},
                    "RoleArn": {"Value": {"Fn::GetAtt": [ROLE, "Arn"]}},
                }
            })
            .to_string(),
            declared: Declared {
                arn_output: "GroupRef",
                same_arn_outputs: Vec::new(),
                reported_output: None,
                name: "search-platform",
                retention_in_days: 90,
                cloud_trail_enabled: true,
                tag_key_boundaries: Vec::new(),
                cross_account_roles: vec![cross_account.clone(), second_cross_account.clone()],
                channels: vec![(
                    topic.clone(),
                    vec![configuration.clone(), second_configuration.clone()],
                )],
                encryption_type: "AWS_OWNED_KEY",
                names_the_stack_key: false,
                has_policy: false,
                tags: BTreeMap::new(),
            },
        },
        // The same type in YAML with the short-form intrinsic, one tag key boundary, one
        // cross-account source role, and the group's identity read back through `Fn::GetAtt`.
        Row {
            case: "a YAML declaration with the short-form intrinsic",
            stack_name: "aiops-group-yaml",
            template: format!(
                "\
Resources:
  {ROLE}:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: '2012-10-17'
        Statement:
          - Effect: Allow
            Principal:
              Service: {INVESTIGATIONS_PRINCIPAL}
            Action: sts:AssumeRole
  {GROUP}:
    Type: {INVESTIGATION_GROUP_TYPE}
    Properties:
      Name: logistics
      RoleArn: !GetAtt {ROLE}.Arn
      RetentionInDays: 14
      TagKeyBoundaries:
        - workload
      CrossAccountConfigurations:
        - SourceRoleArn: {cross_account}
      Tags:
        - Key: owner
          Value: sre
Outputs:
  GroupArn:
    Value: !GetAtt {GROUP}.Arn
  RoleArn:
    Value: !GetAtt {ROLE}.Arn
"
            ),
            declared: Declared {
                arn_output: "GroupArn",
                same_arn_outputs: Vec::new(),
                reported_output: None,
                name: "logistics",
                retention_in_days: 14,
                cloud_trail_enabled: true,
                tag_key_boundaries: vec!["workload"],
                cross_account_roles: vec![cross_account.clone()],
                channels: Vec::new(),
                encryption_type: "AWS_OWNED_KEY",
                names_the_stack_key: false,
                has_policy: false,
                tags: [("owner".to_string(), "sre".to_string())]
                    .into_iter()
                    .collect(),
            },
        },
    ]
}

/// The source roles one reported group carries, in the order that service reports them.
fn reported_cross_account_roles(reported: &Value) -> Vec<String> {
    reported
        .get("crossAccountConfigurations")
        .and_then(Value::as_array)
        .map(|listed| {
            listed
                .iter()
                .filter_map(|entry| entry.get("sourceRoleArn").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The chat channels one reported group carries, as `(topic, configurations)` pairs: that
/// service reports them as the MAP its own member is, keyed by the Amazon SNS topic.
fn reported_channels(reported: &Value) -> Vec<(String, Vec<String>)> {
    reported
        .get("chatbotNotificationChannel")
        .and_then(Value::as_object)
        .map(|channels| {
            channels
                .iter()
                .map(|(topic, configurations)| {
                    (
                        topic.clone(),
                        configurations
                            .as_array()
                            .map(|listed| {
                                listed
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A stack that completes has really created its declared investigation group in Amazon
/// CloudWatch investigations, under the identity that service minted, with every property the
/// declaration stated in force there.
#[tokio::test]
async fn a_stack_creates_its_declared_investigation_group_in_the_aiops_control_plane() {
    for row in rows() {
        // One router per row: an account holds one investigation group per Region, so each row
        // deploys into an emulator of its own rather than into a Region another row filled.
        let router = build_router();
        let case = row.case;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {body}"
        );

        // The stack publishes the ARN that service minted, which is the type's primary
        // identifier and the handle every investigation-group API takes.
        let arn = published(case, &body, row.declared.arn_output);
        assert!(
            arn.starts_with(&group_arn_prefix()),
            "case {case:?}: expected an investigation group ARN of this account and Region, got \
             {arn:?}"
        );
        for other in &row.declared.same_arn_outputs {
            assert_eq!(
                published(case, &body, other),
                arn,
                "case {case:?}: expected Ref and Fn::GetAtt Arn to publish one ARN"
            );
        }

        // ...and that service really holds the group under it, configured as declared.
        let (status, reported) = get_group(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetInvestigationGroup to resolve {arn:?}, got {status} with \
             {reported}"
        );
        assert_eq!(
            reported.get("arn").and_then(Value::as_str),
            Some(arn.as_str()),
            "case {case:?}: expected the group to report the published ARN, got {reported}"
        );
        assert_eq!(
            reported.get("name").and_then(Value::as_str),
            Some(row.declared.name),
            "case {case:?}: expected the declared name, got {reported}"
        );
        // The role the group gathers its data as is the one the stack created, resolved in
        // this account's IAM by that service before it stored the group — every row publishes
        // it so the two sides are compared rather than assumed.
        assert_eq!(
            reported.get("roleArn").and_then(Value::as_str),
            Some(published(case, &body, "RoleArn").as_str()),
            "case {case:?}: expected the role the declaration named, got {reported}"
        );
        assert_eq!(
            reported.get("retentionInDays").and_then(Value::as_i64),
            Some(row.declared.retention_in_days),
            "case {case:?}: expected the retention the declaration implies, got {reported}"
        );
        assert_eq!(
            reported
                .get("isCloudTrailEventHistoryEnabled")
                .and_then(Value::as_bool),
            Some(row.declared.cloud_trail_enabled),
            "case {case:?}: expected the CloudTrail flag the declaration implies, got {reported}"
        );
        let boundaries: Vec<String> = reported
            .get("tagKeyBoundaries")
            .and_then(Value::as_array)
            .map(|listed| {
                listed
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            boundaries, row.declared.tag_key_boundaries,
            "case {case:?}: expected the declared tag key boundaries, got {reported}"
        );
        assert_eq!(
            reported_cross_account_roles(&reported),
            row.declared.cross_account_roles,
            "case {case:?}: expected the declared cross-account source roles, got {reported}"
        );
        assert_eq!(
            reported_channels(&reported),
            row.declared.channels,
            "case {case:?}: expected the declared chat channels as the map the member is, got \
             {reported}"
        );
        assert_eq!(
            reported
                .pointer("/encryptionConfiguration/type")
                .and_then(Value::as_str),
            Some(row.declared.encryption_type),
            "case {case:?}: expected the encryption type the declaration implies, got {reported}"
        );
        if row.declared.names_the_stack_key {
            assert_eq!(
                reported
                    .pointer("/encryptionConfiguration/kmsKeyId")
                    .and_then(Value::as_str),
                Some(published(case, &body, "KeyArn").as_str()),
                "case {case:?}: expected the group to encrypt with the key the stack created, \
                 got {reported}"
            );
        }

        // The resource policy, where the declaration states one, is really attached — which is
        // a resource of its own in that service.
        let (status, policy) = get_group_policy(&router, &arn).await;
        if row.declared.has_policy {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the declared resource policy to be attached, got \
                 {status} with {policy}"
            );
            let attached: Value = policy
                .get("policy")
                .and_then(Value::as_str)
                .map(|document| serde_json::from_str(document).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            assert_eq!(
                attached,
                investigation_group_policy(),
                "case {case:?}: expected the declared document, got {policy}"
            );
        } else {
            assert_eq!(
                status, 404,
                "case {case:?}: expected no resource policy for a declaration stating none, got \
                 {status} with {policy}"
            );
        }

        assert_eq!(
            held_tags(&router, case, &arn).await,
            row.declared.tags,
            "case {case:?}: expected the group to carry the declared tags, read through that \
             service's own tag plane"
        );

        // ...and that service LISTS it, as the one group this account and Region holds.
        let listed = listed_groups(&router, case).await;
        assert_eq!(
            listed,
            vec![arn.clone()],
            "case {case:?}: expected ListInvestigationGroups to report the published ARN alone, \
             got {listed:?}"
        );

        // A read-only value the row reads back is the one that service reported, never a value
        // CloudFormation invented for it.
        if let Some((output, member)) = row.declared.reported_output {
            let answered = published(case, &body, output);
            let value = reported
                .get(member)
                .unwrap_or_else(|| panic!("case {case:?}: no {member} in {reported}"));
            let expected = value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string());
            assert_eq!(
                answered, expected,
                "case {case:?}: expected the published {output} to be the {member} that service \
                 reported"
            );
        }
    }
}

/// A re-declared configuration is applied to the group that service holds rather than replacing
/// it: the identity it minted survives every change the update API can make.
#[tokio::test]
async fn a_stack_update_reconfigures_the_investigation_group_that_service_holds() {
    let router = build_router();
    let stack_name = "aiops-group-reconfigured";
    let other_role = other_account_role("Investigator");
    let topic = topic_arn("investigation-notifications");
    let configuration = chat_configuration_arn("ops");

    // The group's own properties change while its name and retention — the two the create
    // alone fixes — stay as declared, so every row is an in-place change.
    let template = |properties: Value| {
        json!({
            "Resources": {
                ROLE: investigation_role(INVESTIGATIONS_PRINCIPAL),
                "SecondRole": investigation_role(INVESTIGATIONS_PRINCIPAL),
                GROUP: {"Type": INVESTIGATION_GROUP_TYPE, "Properties": properties},
            },
            "Outputs": {"GroupRef": {"Value": {"Ref": GROUP}}}
        })
        .to_string()
    };
    let base = json!({
        "Name": "reconfigured",
        "RoleArn": {"Fn::GetAtt": [ROLE, "Arn"]},
        "RetentionInDays": 30,
    });

    let created = create_and_settle(
        &router,
        "the first deployment",
        stack_name,
        &template(base.clone()),
    )
    .await;
    assert_eq!(
        opt_element(&created, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "expected the create to complete, got {created}"
    );
    let arn = published("the first deployment", &created, "GroupRef");

    // Each row re-declares the group and states what that service must then report about it:
    // the rows differ along WHICH member of the update the change touches.
    struct Change {
        case: &'static str,
        properties: Value,
        role_output: &'static str,
        cloud_trail_enabled: bool,
        tag_key_boundaries: Vec<&'static str>,
        cross_account_roles: Vec<String>,
        channels: Vec<(String, Vec<String>)>,
        has_policy: bool,
        tags: BTreeMap<String, String>,
    }

    let changes = vec![
        Change {
            case: "another role of the same stack, the CloudTrail flag turned off",
            properties: json!({
                "Name": "reconfigured",
                "RoleArn": {"Fn::GetAtt": ["SecondRole", "Arn"]},
                "RetentionInDays": 30,
                "IsCloudTrailEventHistoryEnabled": false,
            }),
            role_output: "SecondRole",
            cloud_trail_enabled: false,
            tag_key_boundaries: Vec::new(),
            cross_account_roles: Vec::new(),
            channels: Vec::new(),
            has_policy: false,
            tags: BTreeMap::new(),
        },
        Change {
            case: "tag key boundaries, a cross-account role, a chat channel and tags added",
            properties: json!({
                "Name": "reconfigured",
                "RoleArn": {"Fn::GetAtt": ["SecondRole", "Arn"]},
                "RetentionInDays": 30,
                "IsCloudTrailEventHistoryEnabled": false,
                "TagKeyBoundaries": ["application"],
                "CrossAccountConfigurations": [{"SourceRoleArn": other_role.clone()}],
                "ChatbotNotificationChannels": [{
                    "SNSTopicArn": topic.clone(),
                    "ChatConfigurationArns": [configuration.clone()],
                }],
                "InvestigationGroupPolicy": investigation_group_policy(),
                "Tags": [{"Key": "team", "Value": "sre"}],
            }),
            role_output: "SecondRole",
            cloud_trail_enabled: false,
            tag_key_boundaries: vec!["application"],
            cross_account_roles: vec![other_role.clone()],
            channels: vec![(topic.clone(), vec![configuration.clone()])],
            has_policy: true,
            tags: [("team".to_string(), "sre".to_string())]
                .into_iter()
                .collect(),
        },
        Change {
            case: "the resource policy and the tags taken away again",
            properties: json!({
                "Name": "reconfigured",
                "RoleArn": {"Fn::GetAtt": [ROLE, "Arn"]},
                "RetentionInDays": 30,
                "IsCloudTrailEventHistoryEnabled": true,
                "TagKeyBoundaries": ["application", "service"],
                "CrossAccountConfigurations": [{"SourceRoleArn": other_role.clone()}],
                "ChatbotNotificationChannels": [{
                    "SNSTopicArn": topic.clone(),
                    "ChatConfigurationArns": [configuration.clone()],
                }],
            }),
            role_output: ROLE,
            cloud_trail_enabled: true,
            tag_key_boundaries: vec!["application", "service"],
            cross_account_roles: vec![other_role.clone()],
            channels: vec![(topic.clone(), vec![configuration.clone()])],
            has_policy: false,
            tags: BTreeMap::new(),
        },
        Change {
            // Every collection property dropped from the declaration: what the group is to
            // carry is what the template now states, so a value a deployment no longer declares
            // is taken away rather than left in force.
            case: "every collection property and the flag dropped from the declaration",
            properties: json!({
                "Name": "reconfigured",
                "RoleArn": {"Fn::GetAtt": [ROLE, "Arn"]},
                "RetentionInDays": 30,
            }),
            role_output: ROLE,
            // The flag carries the value its own member documents for an omission.
            cloud_trail_enabled: true,
            tag_key_boundaries: Vec::new(),
            cross_account_roles: Vec::new(),
            channels: Vec::new(),
            has_policy: false,
            tags: BTreeMap::new(),
        },
    ];

    for change in changes {
        let case = change.case;
        let mut document: Value =
            serde_json::from_str(&template(change.properties.clone())).expect("the template parses");
        document["Outputs"]["RoleArn"] =
            json!({"Value": {"Fn::GetAtt": [change.role_output, "Arn"]}});
        let (status, body) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", stack_name),
                ("TemplateBody", &document.to_string()),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
        );
        let described = settled_stack(&router, case, stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, got {described}"
        );
        assert_eq!(
            outputs_of(&described).get("GroupRef"),
            Some(&arn),
            "case {case:?}: expected the group to keep the identity that service minted, got \
             {described}"
        );

        // The change reached the group THAT service holds, not just the stack's record.
        let (status, reported) = get_group(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected that service to still hold {arn:?}, got {status} with \
             {reported}"
        );
        assert_eq!(
            reported.get("roleArn").and_then(Value::as_str),
            Some(published(case, &described, "RoleArn").as_str()),
            "case {case:?}: expected the re-declared role, got {reported}"
        );
        assert_eq!(
            reported
                .get("isCloudTrailEventHistoryEnabled")
                .and_then(Value::as_bool),
            Some(change.cloud_trail_enabled),
            "case {case:?}: expected the re-declared CloudTrail flag, got {reported}"
        );
        let boundaries: Vec<String> = reported
            .get("tagKeyBoundaries")
            .and_then(Value::as_array)
            .map(|listed| {
                listed
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            boundaries, change.tag_key_boundaries,
            "case {case:?}: expected the re-declared tag key boundaries, got {reported}"
        );
        assert_eq!(
            reported_cross_account_roles(&reported),
            change.cross_account_roles,
            "case {case:?}: expected the re-declared cross-account source roles, got {reported}"
        );
        assert_eq!(
            reported_channels(&reported),
            change.channels,
            "case {case:?}: expected the re-declared chat channels, got {reported}"
        );
        let (status, policy) = get_group_policy(&router, &arn).await;
        assert_eq!(
            status == 200,
            change.has_policy,
            "case {case:?}: expected the resource policy to be {}, got {status} with {policy}",
            if change.has_policy {
                "attached"
            } else {
                "absent"
            }
        );
        assert_eq!(
            held_tags(&router, case, &arn).await,
            change.tags,
            "case {case:?}: expected the re-declared tag set"
        );
        let listed = listed_groups(&router, case).await;
        assert_eq!(
            listed,
            vec![arn.clone()],
            "case {case:?}: expected the one group to be changed rather than a second one \
             created, got {listed:?}"
        );
    }
}

/// The recorded scenario this suite was filed for: a group round-trips through a stack, and a
/// later update to an INVALID `RoleArn` is refused with the owning service's own refusal as the
/// resource's failure reason — leaving the group with the role it already had.
#[tokio::test]
async fn an_update_to_an_invalid_role_arn_is_refused_with_the_services_own_refusal() {
    // The rows differ along HOW the re-declared role fails to be a role that service could
    // assume: an ARN naming no role of this account, a role name this account's IAM holds no
    // role under, and an ARN that is not an IAM ARN at all.
    let rows: [(&str, &str, String); 3] = [
        (
            "an IAM role ARN this account holds no role under",
            "aiops-group-missing-role",
            format!(
                "arn:aws:iam::{}:role/no-such-investigation-role",
                engine::emulator_account_id()
            ),
        ),
        (
            "a role ARN of another account",
            "aiops-group-foreign-role",
            other_account_role("ForeignInvestigator"),
        ),
        (
            "an ARN that names no IAM role at all",
            "aiops-group-not-a-role",
            format!(
                "arn:aws:sqs:{REGION}:{}:a-queue-not-a-role",
                engine::emulator_account_id()
            ),
        ),
    ];

    for (case, stack_name, invalid_role_arn) in rows {
        let router = build_router();
        let template = |role: Value| {
            json!({
                "Resources": {
                    ROLE: investigation_role(INVESTIGATIONS_PRINCIPAL),
                    GROUP: {
                        "Type": INVESTIGATION_GROUP_TYPE,
                        "Properties": {
                            "Name": "refused-update",
                            "RoleArn": role,
                            "RetentionInDays": 30,
                        }
                    }
                },
                "Outputs": {
                    "GroupRef": {"Value": {"Ref": GROUP}},
                    "RoleArn": {"Value": {"Fn::GetAtt": [ROLE, "Arn"]}},
                }
            })
            .to_string()
        };

        // The round trip first: the group is created and the stack publishes what that service
        // minted.
        let created =
            create_and_settle(&router, case, stack_name, &template(json!({"Fn::GetAtt": [ROLE, "Arn"]})))
                .await;
        assert_eq!(
            opt_element(&created, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {created}"
        );
        let arn = published(case, &created, "GroupRef");
        let role_arn = published(case, &created, "RoleArn");

        // ...then the update to the invalid role, which that service refuses.
        let (status, body) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", stack_name),
                ("TemplateBody", &template(json!(invalid_role_arn))),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
        );
        let described = settled_stack(&router, case, stack_name).await;
        let settled = opt_element(&described, "StackStatus").unwrap_or_default();
        assert!(
            settled.starts_with("UPDATE_ROLLBACK"),
            "case {case:?}: expected the refused update to be rolled back, got {described}"
        );

        // The failure reason is the OWNING service's own refusal, on the resource that failed,
        // and never a refusal of the template.
        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = elements(&events, "ResourceStatusReason");
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains(role_not_assumable())),
            "case {case:?}: expected a failure reason carrying {:?}, got {reasons:?}",
            role_not_assumable()
        );

        // And the group still gathers its data as the role it already had: a refused change
        // wrote nothing.
        let (status, reported) = get_group(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected that service to still hold {arn:?}, got {status} with \
             {reported}"
        );
        assert_eq!(
            reported.get("roleArn").and_then(Value::as_str),
            Some(role_arn.as_str()),
            "case {case:?}: expected the group to keep the role the refused update did not \
             replace, got {reported}"
        );
    }
}

/// One refusal row: a declaration the owning service refuses, and what the failed resource must
/// report.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    /// The service principal the declared role's trust policy admits.
    role_principal: &'static str,
    /// The group's declared properties, beside the `Name` and the `RoleArn` the template adds.
    properties: Value,
    /// Whether the declaration takes its role from the stack's own role resource; a row that
    /// states `RoleArn` itself sets this false.
    role_from_the_stack: bool,
    /// The FIXED text of the sentence that service words this class of refusal with, which the
    /// resource's failure reason has to carry.
    sentence: String,
}

fn refused_rows() -> Vec<Refused> {
    let own_account = engine::emulator_account_id();
    vec![
        // A role this account's IAM holds, whose trust policy admits another service, is a role
        // the investigations service could not act as.
        Refused {
            case: "a role whose trust policy does not admit the investigations service",
            stack_name: "aiops-group-untrusted-role",
            role_principal: "ec2.amazonaws.com",
            properties: json!({}),
            role_from_the_stack: true,
            sentence: role_not_assumable().to_string(),
        },
        // An encryption configuration selecting a customer managed key while naming none names
        // nothing to encrypt with.
        Refused {
            case: "a customer managed key selected with no key named",
            stack_name: "aiops-group-no-key",
            role_principal: INVESTIGATIONS_PRINCIPAL,
            properties: json!({
                "EncryptionConfiguration": {
                    "EncryptionConfigurationType": "CUSTOMER_MANAGED_KMS_KEY",
                },
            }),
            role_from_the_stack: true,
            sentence: service_com_amazonaws_aiops_aiops::state::KMS_KEY_ID_REQUIRED.clone(),
        },
        // And the other direction of the same agreement: the Amazon Web Services owned key
        // selected while a customer managed key is named asks for two keys at once.
        Refused {
            case: "the AWS owned key selected while a customer managed key is named",
            stack_name: "aiops-group-two-keys",
            role_principal: INVESTIGATIONS_PRINCIPAL,
            properties: json!({
                "EncryptionConfiguration": {
                    "EncryptionConfigurationType": "AWS_OWNED_KEY",
                    "KmsKeyId": format!("arn:aws:kms:{REGION}:{own_account}:key/abcd1234"),
                },
            }),
            role_from_the_stack: true,
            sentence: service_com_amazonaws_aiops_aiops::state::KMS_KEY_ID_NOT_ACCEPTED.clone(),
        },
        // A cross-account source role belonging to the investigation account itself is no
        // cross-account access at all.
        Refused {
            case: "a cross-account source role of the investigation account itself",
            stack_name: "aiops-group-own-account-role",
            role_principal: INVESTIGATIONS_PRINCIPAL,
            properties: json!({
                "CrossAccountConfigurations": [{
                    "SourceRoleArn": format!("arn:aws:iam::{own_account}:role/Investigator"),
                }],
            }),
            role_from_the_stack: true,
            sentence: service_com_amazonaws_aiops_aiops::state::source_role_arn_in_investigation_account(
                own_account,
                &format!("arn:aws:iam::{own_account}:role/Investigator"),
            ),
        },
    ]
}

/// A declaration the owning service refuses fails the RESOURCE — with that service's own
/// sentence — and rolls the stack back, creating nothing.
#[tokio::test]
async fn a_declaration_the_aiops_control_plane_refuses_rolls_the_stack_back() {
    for row in refused_rows() {
        let router = build_router();
        let case = row.case;
        let mut properties = row.properties.clone();
        properties["Name"] = json!("refused-group");
        if row.role_from_the_stack {
            properties["RoleArn"] = json!({"Fn::GetAtt": [ROLE, "Arn"]});
        }
        let template = json!({
            "Resources": {
                ROLE: investigation_role(row.role_principal),
                GROUP: {"Type": INVESTIGATION_GROUP_TYPE, "Properties": properties},
            },
            "Outputs": {"GroupRef": {"Value": {"Ref": GROUP}}}
        })
        .to_string();

        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed create to be rolled back rather than publish an \
             ARN that service never minted, got {body}"
        );
        assert!(
            outputs_of(&body).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {body}"
        );

        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack_name)]).await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = elements(&events, "ResourceStatusReason");
        assert!(
            reasons.iter().any(|reason| reason.contains(&row.sentence)),
            "case {case:?}: expected a failure reason carrying {:?}, got {reasons:?}",
            row.sentence
        );

        // And nothing was created: that service holds no investigation group at all.
        let listed = listed_groups(&router, case).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected that service to hold no investigation group, got {listed:?}"
        );
    }
}

/// Deleting the stack takes the investigation group away from the service that held it, so the
/// Region is free for another group.
#[tokio::test]
async fn deleting_the_stack_deletes_the_investigation_group_it_created() {
    let router = build_router();
    let case = "a group deleted with its stack";
    let stack_name = "aiops-group-deleted";
    let template = json!({
        "Resources": {
            ROLE: investigation_role(INVESTIGATIONS_PRINCIPAL),
            GROUP: {
                "Type": INVESTIGATION_GROUP_TYPE,
                "Properties": {
                    "Name": "deleted-with-its-stack",
                    "RoleArn": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "InvestigationGroupPolicy": investigation_group_policy(),
                }
            }
        },
        "Outputs": {"GroupRef": {"Value": {"Ref": GROUP}}}
    })
    .to_string();

    let body = create_and_settle(&router, case, stack_name, &template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {body}"
    );
    let arn = published(case, &body, "GroupRef");
    assert_eq!(
        listed_groups(&router, case).await,
        vec![arn.clone()],
        "case {case:?}: expected that service to hold the group the stack created"
    );

    let (status, deleted) =
        cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );

    // The group is gone from the service that held it — read both ways: its own listing no
    // longer reports it, and a read of the ARN the stack published answers that service's own
    // not-found.
    let last = poll_until(case, "that service to stop holding the group", || async {
        let listed = listed_groups(&router, case).await;
        (listed.is_empty(), format!("{listed:?}"))
    })
    .await;
    assert_eq!(
        last, "[]",
        "case {case:?}: expected the group to be gone from that service, got {last}"
    );
    let (status, reported) = get_group(&router, &arn).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected that service to answer its own not-found for the deleted group, \
         got {status} with {reported}"
    );
}

// SPDX-License-Identifier: CC0-1.0

//! The Amazon Data Lifecycle Manager lifecycle policy a stack declares, end to end: the policy
//! exists in that service itself, under the identity it minted, a changed declaration reaches it,
//! deleting the stack takes it away — and a declaration that service REFUSES fails the resource
//! with its own refusal and rolls the stack back.
//!
//! RULE: a resource type AWS publishes is one a template may declare, and a declaration of it is
//! DEPLOYED by calling the service that OWNS the type — so the handle the stack publishes (`Ref`)
//! resolves in that service, the values the type publishes are the ones that service reported, and
//! what that service refuses is a FAILED RESOURCE rather than a template-format error.
//!
//! The rows of the deploy table differ along what the rule generalises over: which KIND of policy
//! the declaration describes (a custom EBS snapshot policy, a custom EBS-backed AMI policy, a
//! default policy configured from the request body), the resource types it targets, the form each
//! nested rule of its schedule states (a count-based retention beside an interval-based one, an
//! interval creation rule beside a cron one), the activation state it asks for, whether it carries
//! the type's own `Tags`, which published return value the output reads (`Ref`, `Fn::GetAtt Arn`)
//! and the template spelling (JSON with long-form intrinsics, YAML with the short-form tags).
//!
//! The refusal rows differ along the CLASS of declaration that service refuses: an execution role
//! the caller may not pass, for two kinds of principal, and a schedule whose retention states no
//! form at all. Each is the owning service's own refusal, quoted on the resource that failed —
//! never the `Template format error: Unrecognized resource types` a type CloudFormation does not
//! know is refused with, which no row of either table may ever see.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs` builds —
//! the CloudFormation calls over its awsQuery wire, the Amazon Data Lifecycle Manager calls over
//! that service's own `restJson1` wire — so what is asserted is what a client that deploys the
//! template and then calls that service observes.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, quoted_and_class, router, settled_stack_status,
    stack_event_reasons, REGION, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::{describe_stack, form_encode, output_value};

/// The resource type under test, and the type of the role the policy runs under — declared in the
/// same stack, which is why every deploy here acknowledges `CAPABILITY_IAM`.
const POLICY: &str = "AWS::DLM::LifecyclePolicy";
const ROLE: &str = "AWS::IAM::Role";
const CAPABILITY_IAM: &str = "CAPABILITY_IAM";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with, which
/// no row of either table may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The error code Amazon Data Lifecycle Manager answers for a policy it does not hold, and the
/// status that error carries (the modelled 404 of `GetLifecyclePolicy`).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// The interval a count-based `RetainRule` reads back with: that service materializes the member
/// the request stated no value for, and every read of such a policy reports it
/// (`RetainRule$Interval`, `@range(min: 0)`).
const UNSET_RULE_INTERVAL: i64 = 0;

/// The policy types a custom policy may be of, and the resource types each manages
/// (`smithy_query(query="shape", shape="PolicyDetails",
/// model_path="services/service-com-amazonaws-dlm-dlm-20180112/smithy-model/\
/// com-amazonaws-dlm-dlm-20180112.json")`: `PolicyType` is one of
/// `EBS_SNAPSHOT_MANAGEMENT | IMAGE_MANAGEMENT | EVENT_BASED_POLICY`, and `ResourceTypes` one of
/// `VOLUME | INSTANCE`).
const EBS_SNAPSHOT_MANAGEMENT: &str = "EBS_SNAPSHOT_MANAGEMENT";
const IMAGE_MANAGEMENT: &str = "IMAGE_MANAGEMENT";
const VOLUME: &str = "VOLUME";
const INSTANCE: &str = "INSTANCE";

/// The trust policy a role Amazon Data Lifecycle Manager assumes carries: that service's own
/// principal, which is its model's `cloudTrailEventSource` (`dlm.amazonaws.com`).
fn assume_role_policy_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "dlm.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// One Amazon Data Lifecycle Manager call over the `restJson1` wire its model declares, on the
/// method and URI that operation's own `@http` binding names, signed for `dlm` — that model's own
/// `sigv4Name` — in the Region the stack's resources live in.
async fn dlm(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://dlm.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("dlm.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/dlm/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        );
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed Amazon Data Lifecycle Manager request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// That service's own read of one policy, by the id it minted: the status it answered and the
/// whole body, whose `Policy` member carries the record.
async fn get_policy(router: &MultiServiceRouter, policy_id: &str) -> (u16, Value) {
    dlm(router, "GET", &format!("/policies/{policy_id}"), None).await
}

/// Every policy that service lists for one resource type, narrowed by the target tag the row's
/// policy selects on — the listing a client of that service uses to find a policy it did not
/// record the id of.
async fn list_policies(
    router: &MultiServiceRouter,
    resource_type: &str,
    target_tag: (&str, &str),
) -> Vec<Value> {
    let filter = form_encode(&format!("{}={}", target_tag.0, target_tag.1));
    let uri = format!("/policies?resourceTypes={resource_type}&targetTags={filter}");
    let (_, answered) = dlm(router, "GET", &uri, None).await;
    answered
        .get("Policies")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The tags that service holds on one policy, by the policy's own ARN — the `@httpLabel` of
/// `GET /tags/{ResourceArn}`, escaped as a single path segment the way an SDK sends a non-greedy
/// label.
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let (status, answered) = dlm(router, "GET", &format!("/tags/{}", form_encode(arn)), None).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListTagsForResource for {arn}, got {status} with {answered}"
    );
    answered
        .get("Tags")
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

/// Submit `template` as `stack_name`, carrying the stack's own tag and the IAM acknowledgement the
/// declared role needs, and answer the `CreateStack` body: a type AWS publishes is ADMITTED, so
/// this never sees the template-format refusal.
async fn submitted(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = create_stack(
        router,
        stack_name,
        template,
        &[
            member("Capabilities.member.1", CAPABILITY_IAM),
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — a {POLICY} is a type a template may \
         declare — got {status} with body {body}"
    );
    body
}

/// Deploy `template` as `stack_name` and answer the `DescribeStacks` body of the COMPLETED stack,
/// quoting the stack's own failure reasons when the create did not complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    submitted(router, case, stack_name, template).await;
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    let (status, described) = describe_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
    );
    described
}

/// Whether `policy_id` has the form a `PolicyId` publishes (`^policy-[a-f0-9]+$`).
fn is_a_policy_id(policy_id: &str) -> bool {
    policy_id
        .strip_prefix("policy-")
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
}

/// Whether `arn` has the form a `PolicyArn` publishes
/// (`^arn:aws(-[a-z]{1,3}){0,2}:dlm:[A-Za-z0-9_/.-]{0,63}:\d+:policy/[0-9A-Za-z_-]{1,128}$`).
fn is_a_policy_arn(arn: &str, policy_id: &str) -> bool {
    arn.starts_with("arn:")
        && arn.contains(&format!(":dlm:{REGION}:"))
        && arn.ends_with(&format!(":policy/{policy_id}"))
}

/// Read one policy until that service reports it, or answer the last read once the deadline
/// passes: the resource's creation is a stack transition the CLOCK makes due, so what is waited
/// for is the CONDITION rather than a span of wall clock.
async fn reported_policy(router: &MultiServiceRouter, case: &str, policy_id: &str) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = get_policy(router, policy_id).await;
        if status == 200 || std::time::Instant::now() >= deadline {
            assert_eq!(
                status, 200,
                "case {case:?}: expected Amazon Data Lifecycle Manager to report the policy the \
                 stack created under {policy_id:?}, got {status} with {body}"
            );
            return body.get("Policy").cloned().unwrap_or(Value::Null);
        }
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// What one row expects of the policy the stack declares.
struct Expected {
    /// The configuration the policy must report back, member by member, as the declaration stated
    /// it (plus whatever that service materializes beside it).
    reported: Value,
    /// Whether the policy is a default policy, which that service reports of every policy.
    default_policy: bool,
    /// The resource type and target tag the policy is listed under, for a custom policy that
    /// selects by tag.
    listed_under: Option<(&'static str, (&'static str, &'static str))>,
    /// The type's own `Tags` the declaration states, which the policy must carry beside the
    /// stack's own propagated tag.
    tags: &'static [(&'static str, &'static str)],
}

/// One row: a template, and what the policy it declares must report.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    expected: Expected,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own stack: a custom EBS snapshot policy with an interval creation rule and a
        // COUNT-based retention, every return value read back, in JSON with long-form intrinsics.
        Row {
            case: "a custom EBS snapshot policy with a count-based retain rule",
            stack_name: "dlm-policy-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Role": {
                        "Type": ROLE,
                        "Properties": {
                            "AssumeRolePolicyDocument": assume_role_policy_document(),
                        }
                    },
                    "Policy": {
                        "Type": POLICY,
                        "Properties": {
                            "Description": "dlm round trip snapshots",
                            "State": "ENABLED",
                            "ExecutionRoleArn": {"Fn::GetAtt": ["Role", "Arn"]},
                            "PolicyDetails": {
                                "PolicyType": EBS_SNAPSHOT_MANAGEMENT,
                                "ResourceTypes": [VOLUME],
                                "TargetTags": [{"Key": "dlm-round-trip", "Value": "sim2real"}],
                                "Schedules": [{
                                    "Name": "sim2real-sched",
                                    "CreateRule": {
                                        "Interval": 24,
                                        "IntervalUnit": "HOURS",
                                        "Times": ["03:00"],
                                    },
                                    "RetainRule": {"Count": 1},
                                }],
                            },
                            "Tags": [{"Key": "sim2real-dlm", "Value": "round-trip"}],
                        }
                    }
                },
                "Outputs": {
                    "PolicyRef": {"Value": {"Ref": "Policy"}},
                    "PolicyArn": {"Value": {"Fn::GetAtt": ["Policy", "Arn"]}},
                    "RoleArn": {"Value": {"Fn::GetAtt": ["Role", "Arn"]}}
                }
            })
            .to_string(),
            expected: Expected {
                reported: json!({
                    "Description": "dlm round trip snapshots",
                    "State": "ENABLED",
                    "PolicyDetails": {
                        "PolicyType": EBS_SNAPSHOT_MANAGEMENT,
                        "ResourceTypes": [VOLUME],
                        "TargetTags": [{"Key": "dlm-round-trip", "Value": "sim2real"}],
                        "Schedules": [{
                            "Name": "sim2real-sched",
                            "CreateRule": {
                                "Interval": 24,
                                "IntervalUnit": "HOURS",
                                "Times": ["03:00"],
                            },
                            "RetainRule": {"Count": 1, "Interval": UNSET_RULE_INTERVAL},
                        }],
                    },
                }),
                default_policy: false,
                listed_under: Some((VOLUME, ("dlm-round-trip", "sim2real"))),
                tags: &[("sim2real-dlm", "round-trip")],
            },
        },
        // The OTHER custom policy type, over the other resource type, with a CRON creation rule and
        // an INTERVAL-based retention, disabled at creation, no `Tags` of its own, in YAML with the
        // short-form intrinsics.
        Row {
            case: "a custom EBS-backed AMI policy with a cron create rule, disabled, YAML",
            stack_name: "dlm-policy-images",
            template: "\
Resources:
  Role:
    Type: AWS::IAM::Role
    Properties:
      AssumeRolePolicyDocument:
        Version: '2012-10-17'
        Statement:
          - Effect: Allow
            Principal:
              Service: dlm.amazonaws.com
            Action: sts:AssumeRole
  Policy:
    Type: AWS::DLM::LifecyclePolicy
    Properties:
      Description: dlm images nightly
      State: DISABLED
      ExecutionRoleArn: !GetAtt Role.Arn
      PolicyDetails:
        PolicyType: IMAGE_MANAGEMENT
        ResourceTypes:
          - INSTANCE
        TargetTags:
          - Key: dlm-images
            Value: sim2real
        Schedules:
          - Name: sim2real-images
            CreateRule:
              CronExpression: cron(0 1 * * ? *)
            RetainRule:
              Interval: 2
              IntervalUnit: DAYS
Outputs:
  PolicyRef:
    Value: !Ref Policy
  PolicyArn:
    Value: !GetAtt Policy.Arn
  RoleArn:
    Value: !GetAtt Role.Arn
"
            .to_string(),
            expected: Expected {
                reported: json!({
                    "Description": "dlm images nightly",
                    "State": "DISABLED",
                    "PolicyDetails": {
                        "PolicyType": IMAGE_MANAGEMENT,
                        "ResourceTypes": [INSTANCE],
                        "TargetTags": [{"Key": "dlm-images", "Value": "sim2real"}],
                        "Schedules": [{
                            "Name": "sim2real-images",
                            "CreateRule": {"CronExpression": "cron(0 1 * * ? *)"},
                            "RetainRule": {"Interval": 2, "IntervalUnit": "DAYS"},
                        }],
                    },
                }),
                default_policy: false,
                listed_under: Some((INSTANCE, ("dlm-images", "sim2real"))),
                tags: &[],
            },
        },
        // A DEFAULT policy, configured from the request body rather than from a `PolicyDetails`
        // structure: the `[Default policies only]` properties, which are the create's own
        // top-level members, including the two that are NUMBERS on the wire.
        Row {
            case: "a default policy for EBS snapshots, configured in the request body",
            stack_name: "dlm-policy-default",
            template: json!({
                "Resources": {
                    "Role": {
                        "Type": ROLE,
                        "Properties": {
                            "AssumeRolePolicyDocument": assume_role_policy_document(),
                        }
                    },
                    "Policy": {
                        "Type": POLICY,
                        "Properties": {
                            "Description": "dlm default volumes",
                            "State": "ENABLED",
                            "ExecutionRoleArn": {"Fn::GetAtt": ["Role", "Arn"]},
                            "DefaultPolicy": VOLUME,
                            "CreateInterval": 2,
                            "RetainInterval": 7,
                            "CopyTags": true,
                            "ExtendDeletion": false,
                            "Tags": [{"Key": "sim2real-dlm", "Value": "default"}],
                        }
                    }
                },
                "Outputs": {
                    "PolicyRef": {"Value": {"Ref": "Policy"}},
                    "PolicyArn": {"Value": {"Fn::GetAtt": ["Policy", "Arn"]}},
                    "RoleArn": {"Value": {"Fn::GetAtt": ["Role", "Arn"]}}
                }
            })
            .to_string(),
            expected: Expected {
                reported: json!({
                    "Description": "dlm default volumes",
                    "State": "ENABLED",
                }),
                default_policy: true,
                listed_under: None,
                tags: &[("sim2real-dlm", "default")],
            },
        },
    ]
}

/// Every member of `expected` is reported by `reported`, with the same value: no member of the
/// declaration is dropped or rewritten on the way to the owning service. Objects are compared
/// member by member — so a value that service materializes beside what was declared is allowed —
/// lists element by element, and scalars by equality.
fn reports(expected: &Value, reported: &Value, path: &str) -> Result<(), String> {
    match expected {
        Value::Object(members) => {
            let Some(found) = reported.as_object() else {
                return Err(format!("{path} must be an object, got {reported}"));
            };
            for (name, value) in members {
                let Some(held) = found.get(name) else {
                    return Err(format!("{path}/{name} was declared as {value} but is absent"));
                };
                reports(value, held, &format!("{path}/{name}"))?;
            }
            Ok(())
        }
        Value::Array(elements) => {
            let Some(found) = reported.as_array() else {
                return Err(format!("{path} must be a list, got {reported}"));
            };
            if found.len() != elements.len() {
                return Err(format!(
                    "{path} must carry {} elements, got {reported}",
                    elements.len()
                ));
            }
            for (index, value) in elements.iter().enumerate() {
                reports(value, &found[index], &format!("{path}/{index}"))?;
            }
            Ok(())
        }
        scalar if scalar == reported => Ok(()),
        scalar => Err(format!("{path} must be {scalar}, got {reported}")),
    }
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_lifecycle_policy_dlm_minted() {
    for row in rows() {
        let router = router();
        let case = row.case;
        let described = deployed(&router, case, row.stack_name, &row.template).await;

        let published = output_value(&described, "PolicyRef")
            .unwrap_or_else(|| panic!("case {case:?}: no PolicyRef output in {described}"));
        assert!(
            is_a_policy_id(&published),
            "case {case:?}: expected Ref to publish a policy id of the modelled form, got \
             {published:?}"
        );
        let published_arn = output_value(&described, "PolicyArn")
            .unwrap_or_else(|| panic!("case {case:?}: no PolicyArn output in {described}"));
        assert!(
            is_a_policy_arn(&published_arn, &published),
            "case {case:?}: expected Fn::GetAtt Arn to publish the ARN of the published policy, \
             got {published_arn:?} for {published:?}"
        );
        let role_arn = output_value(&described, "RoleArn")
            .unwrap_or_else(|| panic!("case {case:?}: no RoleArn output in {described}"));

        // THE RULE: the policy the stack claims to have created is a policy that service holds,
        // under the identity the stack published, configured as the template declared.
        let policy = reported_policy(&router, case, &published).await;
        assert_eq!(
            (
                policy.get("PolicyId").and_then(Value::as_str),
                policy.get("PolicyArn").and_then(Value::as_str),
                policy.get("ExecutionRoleArn").and_then(Value::as_str),
                policy.get("DefaultPolicy").and_then(Value::as_bool),
            ),
            (
                Some(published.as_str()),
                Some(published_arn.as_str()),
                Some(role_arn.as_str()),
                Some(row.expected.default_policy),
            ),
            "case {case:?}: expected the policy to report the published identity and the declared \
             role, got {policy}"
        );
        if let Err(difference) = reports(&row.expected.reported, &policy, "Policy") {
            panic!("case {case:?}: {difference}");
        }

        // ...and the listing that service serves for the policy's own scope resolves the same
        // identity, which is how a client finds a policy it holds no id for.
        if let Some((resource_type, target_tag)) = row.expected.listed_under {
            let listed = list_policies(&router, resource_type, target_tag).await;
            let ids: Vec<&str> = listed
                .iter()
                .filter_map(|summary| summary.get("PolicyId").and_then(Value::as_str))
                .collect();
            assert_eq!(
                ids,
                vec![published.as_str()],
                "case {case:?}: expected the listing for {resource_type}/{target_tag:?} to \
                 resolve exactly the published policy, got {listed:?}"
            );
        }

        // The tags the policy carries are the declaration's own beside the stack's, and none of
        // the records CloudFormation keeps in the namespace AWS reserves for itself.
        let held = list_tags(&router, &published_arn).await;
        let mut expected: BTreeMap<String, String> = row
            .expected
            .tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        expected.insert(STACK_TAG_KEY.to_string(), row.stack_name.to_string());
        assert_eq!(
            held, expected,
            "case {case:?}: expected the declared tags beside the stack's own, got {held:?}"
        );
    }
}

#[tokio::test]
async fn a_declared_interval_reaches_the_policy_in_either_spelling() {
    // A template states a value either in JSON's own spelling or as the TEXT a parameter reference
    // resolves to — `Ref` on a parameter answers that parameter's value, which a stack operation
    // carries as a string whatever the parameter's declared type — so both spellings have to reach
    // the owning service as the kind `CreateInterval`/`RetainInterval` are carried in.
    // (case, the stack, the creation frequency, the retention period, how it is stated)
    let rows: [(&str, &str, i64, i64, bool); 3] = [
        (
            "the documented defaults' neighbourhood, as JSON numbers",
            "dlm-interval-json",
            1,
            7,
            false,
        ),
        (
            "the published maxima, as parameter text",
            "dlm-interval-maxima",
            7,
            14,
            true,
        ),
        (
            "the published minima, as parameter text",
            "dlm-interval-minima",
            1,
            2,
            true,
        ),
    ];

    for (case, stack_name, create_interval, retain_interval, as_parameters) in rows {
        let router = router();
        let (intervals, parameters, extra) = if as_parameters {
            (
                json!({
                    "CreateInterval": {"Ref": "CreateDays"},
                    "RetainInterval": {"Ref": "RetainDays"},
                }),
                json!({
                    "CreateDays": {"Type": "Number"},
                    "RetainDays": {"Type": "Number"},
                }),
                vec![
                    member("Parameters.member.1.ParameterKey", "CreateDays"),
                    member(
                        "Parameters.member.1.ParameterValue",
                        &create_interval.to_string(),
                    ),
                    member("Parameters.member.2.ParameterKey", "RetainDays"),
                    member(
                        "Parameters.member.2.ParameterValue",
                        &retain_interval.to_string(),
                    ),
                ],
            )
        } else {
            (
                json!({
                    "CreateInterval": create_interval,
                    "RetainInterval": retain_interval,
                }),
                json!({}),
                Vec::new(),
            )
        };
        let mut properties = json!({
            "Description": "dlm interval spelling",
            "State": "ENABLED",
            "ExecutionRoleArn": {"Fn::GetAtt": ["Role", "Arn"]},
            "DefaultPolicy": VOLUME,
        });
        for (name, value) in intervals.as_object().expect("the intervals are an object") {
            properties[name] = value.clone();
        }
        let mut template = json!({
            "Resources": {
                "Role": {
                    "Type": ROLE,
                    "Properties": {
                        "AssumeRolePolicyDocument": assume_role_policy_document(),
                    }
                },
                "Policy": {"Type": POLICY, "Properties": properties}
            },
            "Outputs": {"PolicyRef": {"Value": {"Ref": "Policy"}}}
        });
        if as_parameters {
            template["Parameters"] = parameters;
        }

        let mut members = vec![member("Capabilities.member.1", CAPABILITY_IAM)];
        members.extend(extra);
        let (status, created) =
            create_stack(&router, stack_name, &template.to_string(), &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
        );
        let reached = settled_stack_status(case, &router, REGION, stack_name).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {reached} with failure reasons \
             {:?}",
            stack_event_reasons(&router, REGION, stack_name).await
        );
        let (_, described) = describe_stack(&router, stack_name).await;
        let published = output_value(&described, "PolicyRef")
            .unwrap_or_else(|| panic!("case {case:?}: no PolicyRef output in {described}"));
        let policy = reported_policy(&router, case, &published).await;
        assert_eq!(
            (
                policy
                    .pointer("/PolicyDetails/CreateInterval")
                    .and_then(Value::as_i64),
                policy
                    .pointer("/PolicyDetails/RetainInterval")
                    .and_then(Value::as_i64),
            ),
            (Some(create_interval), Some(retain_interval)),
            "case {case:?}: expected the declared intervals to reach the policy as numbers, got \
             {policy}"
        );
    }
}

#[tokio::test]
async fn a_changed_declaration_is_applied_to_the_policy_the_stack_has() {
    // `UpdateLifecyclePolicy` "updates the specified lifecycle policy", taking its description, its
    // activation state, the role it runs under and its configuration — so a deployment that states
    // one of them differently is applied to the policy the stack already has, under the identity
    // that service minted, rather than being dropped or replaced.
    let router = router();
    let case = "an update that disables the policy and re-times its schedule";
    let stack_name = "dlm-policy-updated";
    let template_of = |description: &str, state: &str, interval: i64| {
        json!({
            "Resources": {
                "Role": {
                    "Type": ROLE,
                    "Properties": {
                        "AssumeRolePolicyDocument": assume_role_policy_document(),
                    }
                },
                "Policy": {
                    "Type": POLICY,
                    "Properties": {
                        "Description": description,
                        "State": state,
                        "ExecutionRoleArn": {"Fn::GetAtt": ["Role", "Arn"]},
                        "PolicyDetails": {
                            "PolicyType": EBS_SNAPSHOT_MANAGEMENT,
                            "ResourceTypes": [VOLUME],
                            "TargetTags": [{"Key": "dlm-updated", "Value": "sim2real"}],
                            "Schedules": [{
                                "Name": "sim2real-updated",
                                "CreateRule": {
                                    "Interval": interval,
                                    "IntervalUnit": "HOURS",
                                    "Times": ["03:00"],
                                },
                                "RetainRule": {"Count": 2},
                            }],
                        },
                    }
                }
            },
            "Outputs": {"PolicyRef": {"Value": {"Ref": "Policy"}}}
        })
        .to_string()
    };

    let described = deployed(
        &router,
        case,
        stack_name,
        &template_of("before the update", "ENABLED", 24),
    )
    .await;
    let published = output_value(&described, "PolicyRef")
        .unwrap_or_else(|| panic!("case {case:?}: no PolicyRef output in {described}"));
    reported_policy(&router, case, &published).await;

    let (status, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member(
                "TemplateBody",
                &template_of("after the update", "DISABLED", 12),
            ),
            member("Capabilities.member.1", CAPABILITY_IAM),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {updated}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );
    let (_, described) = describe_stack(&router, stack_name).await;
    assert_eq!(
        output_value(&described, "PolicyRef"),
        Some(published.clone()),
        "case {case:?}: expected the policy to keep the identity that service minted, got \
         {described}"
    );

    // The change reached the policy THAT service holds, not just the stack's record — and it is the
    // one policy rather than a second one.
    let policy = reported_policy(&router, case, &published).await;
    assert_eq!(
        (
            policy.get("Description").and_then(Value::as_str),
            policy.get("State").and_then(Value::as_str),
            policy
                .pointer("/PolicyDetails/Schedules/0/CreateRule/Interval")
                .and_then(Value::as_i64),
        ),
        (Some("after the update"), Some("DISABLED"), Some(12)),
        "case {case:?}: expected the re-declared description, state and schedule, got {policy}"
    );
    let listed = list_policies(&router, VOLUME, ("dlm-updated", "sim2real")).await;
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected the one policy to be changed rather than a second one created, \
         got {listed:?}"
    );
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_lifecycle_policies_it_created() {
    let router = router();
    let case = "two policies deleted with their stack";
    let stack_name = "dlm-policy-deleted";
    let declared = |name: &str, target: &str| {
        json!({
            "Type": POLICY,
            "Properties": {
                "Description": name,
                "State": "ENABLED",
                "ExecutionRoleArn": {"Fn::GetAtt": ["Role", "Arn"]},
                "PolicyDetails": {
                    "PolicyType": EBS_SNAPSHOT_MANAGEMENT,
                    "ResourceTypes": [VOLUME],
                    "TargetTags": [{"Key": target, "Value": "sim2real"}],
                    "Schedules": [{
                        "Name": name,
                        "CreateRule": {
                            "Interval": 24,
                            "IntervalUnit": "HOURS",
                            "Times": ["03:00"],
                        },
                        "RetainRule": {"Count": 1},
                    }],
                },
            }
        })
    };
    let template = json!({
        "Resources": {
            "Role": {
                "Type": ROLE,
                "Properties": {
                    "AssumeRolePolicyDocument": assume_role_policy_document(),
                }
            },
            "First": declared("dlm-deleted-first", "dlm-deleted-first"),
            "Second": declared("dlm-deleted-second", "dlm-deleted-second")
        },
        "Outputs": {
            "FirstRef": {"Value": {"Ref": "First"}},
            "SecondRef": {"Value": {"Ref": "Second"}}
        }
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &template).await;
    let ids: Vec<String> = ["FirstRef", "SecondRef"]
        .iter()
        .map(|key| {
            output_value(&described, key)
                .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
        })
        .collect();
    for policy_id in &ids {
        reported_policy(&router, case, policy_id).await;
    }

    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );
    settle_the_deletion(&router, case, &ids).await;

    // The policies the stack created are gone from the owning service, which is what makes a
    // stack's resources the stack's own rather than leftovers — and the read of one says so in
    // that service's own words.
    for policy_id in &ids {
        let (status, answered) = get_policy(&router, policy_id).await;
        let expected_message = format!("The resource {{{policy_id}}} was not found.");
        assert_eq!(
            (
                status,
                answered.get("__type").and_then(Value::as_str),
                answered.get("Message").and_then(Value::as_str),
            ),
            (
                NOT_FOUND_STATUS,
                Some(RESOURCE_NOT_FOUND),
                Some(expected_message.as_str()),
            ),
            "case {case:?}: expected the policy {policy_id:?} to be gone with the stack, got \
             {status} with {answered}"
        );
    }
    for target in ["dlm-deleted-first", "dlm-deleted-second"] {
        let listed = list_policies(&router, VOLUME, (target, "sim2real")).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected no policy left for {target:?}, got {listed:?}"
        );
    }
}

/// Wait until every policy of `ids` is gone from the owning service.
///
/// Waits for the CONDITION rather than for a span, under the harness's own generous deadline, so a
/// loaded host cannot decide the outcome.
async fn settle_the_deletion(router: &MultiServiceRouter, case: &str, ids: &[String]) {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut still_held: Vec<&String> = Vec::new();
        for policy_id in ids {
            let (status, _) = get_policy(router, policy_id).await;
            if status == 200 {
                still_held.push(policy_id);
            }
        }
        if still_held.is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected every policy the stack created to be deleted, these are \
             still held: {still_held:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// One refusal row: a declaration Amazon Data Lifecycle Manager refuses, what the request that
/// earns that refusal looks like against the service itself, and the class the failed resource is
/// reported under.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    /// The execution role the declaration names, where the row's refusal is about the role; `None`
    /// for a row that names the stack's own role.
    execution_role_arn: Option<String>,
    /// The schedule the declaration states, where the row's refusal is about the configuration.
    schedule: Value,
    /// The status the owning service answers, and the `HandlerErrorCode` the provider classifies
    /// that refusal under.
    status: u16,
    class: &'static str,
}

fn refused() -> Vec<Refused> {
    // The one account this emulator presents, so the only account whose roles it can pass. Read
    // from the framework rather than written out, which is what makes the "another account" row
    // genuinely foreign.
    let own = engine::emulator_account_id();
    let count_based = json!({
        "Name": "sim2real-refused",
        "CreateRule": {"Interval": 24, "IntervalUnit": "HOURS", "Times": ["03:00"]},
        "RetainRule": {"Count": 1},
    });
    vec![
        // The issue's own refusal: an execution role in ANOTHER account, which `iam:PassRole` does
        // not grant — the 403 that service answers, classified `AccessDenied`.
        Refused {
            case: "an execution role in another account",
            stack_name: "dlm-policy-cross-account",
            execution_role_arn: Some("arn:aws:iam::111122223333:role/CrossAccountDlmRole".to_string()),
            schedule: count_based.clone(),
            status: 403,
            class: "AccessDenied",
        },
        // The same class against a principal in THIS account that is not a role at all, which is
        // what makes the row about the rule rather than about one ARN.
        Refused {
            case: "an execution role that is not a role",
            stack_name: "dlm-policy-not-a-role",
            execution_role_arn: Some(format!("arn:aws:iam::{own}:user/not-a-role")),
            schedule: count_based,
            status: 403,
            class: "AccessDenied",
        },
        // A different class entirely: a schedule whose retention states NEITHER documented form,
        // which that service refuses as a bad request rather than as a denied role.
        Refused {
            case: "a schedule whose retention states neither form",
            stack_name: "dlm-policy-no-retention",
            execution_role_arn: None,
            schedule: json!({
                "Name": "sim2real-refused",
                "CreateRule": {"Interval": 24, "IntervalUnit": "HOURS", "Times": ["03:00"]},
                "RetainRule": {},
            }),
            status: 400,
            class: "InvalidRequest",
        },
    ]
}

#[tokio::test]
async fn a_declaration_dlm_refuses_rolls_the_stack_back() {
    for row in refused() {
        let router = router();
        let case = row.case;
        let description = format!("dlm refused {}", row.stack_name);
        let execution_role_arn = match &row.execution_role_arn {
            Some(arn) => json!(arn),
            None => json!({"Fn::GetAtt": ["Role", "Arn"]}),
        };
        let policy_details = json!({
            "PolicyType": EBS_SNAPSHOT_MANAGEMENT,
            "ResourceTypes": [VOLUME],
            "TargetTags": [{"Key": row.stack_name, "Value": "sim2real"}],
            "Schedules": [row.schedule.clone()],
        });
        let template = json!({
            "Resources": {
                "Role": {
                    "Type": ROLE,
                    "Properties": {
                        "AssumeRolePolicyDocument": assume_role_policy_document(),
                    }
                },
                "Policy": {
                    "Type": POLICY,
                    "Properties": {
                        "Description": description,
                        "State": "ENABLED",
                        "ExecutionRoleArn": execution_role_arn,
                        "PolicyDetails": policy_details.clone(),
                    }
                }
            },
            "Outputs": {"PolicyRef": {"Value": {"Ref": "Policy"}}}
        })
        .to_string();

        // What the OWNING service answers for exactly that create is what the failed resource has
        // to quote, so it is read from that service itself rather than written out here. A row
        // about the stack's own role names a role of this account, which that service accepts, so
        // the direct call carries the one the row refuses.
        let direct_role = row.execution_role_arn.clone().unwrap_or_else(|| {
            format!(
                "arn:aws:iam::{}:role/AWSDataLifecycleManagerDefaultRole",
                engine::emulator_account_id()
            )
        });
        let (status, refusal) = dlm(
            &router,
            "POST",
            "/policies",
            Some(&json!({
                "Description": format!("{description} direct"),
                "State": "ENABLED",
                "ExecutionRoleArn": direct_role,
                "PolicyDetails": policy_details,
            })),
        )
        .await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected Amazon Data Lifecycle Manager to refuse the declaration \
             itself with HTTP {}, got {status} with {refusal}",
            row.status
        );
        let sentence = refusal
            .get("Message")
            .or_else(|| refusal.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no message in {refusal}"))
            .to_string();

        submitted(&router, case, row.stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, row.stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the failed create to be rolled back rather than publish an \
             identity that service never minted, got {reached} with failure reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack_name).await
        );
        let (_, described) = describe_stack(&router, row.stack_name).await;
        assert_eq!(
            output_value(&described, "PolicyRef"),
            None,
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed, inside the
        // envelope a resource provider reports one in — and never a refusal of the template.
        let (_, events) = call(
            &router,
            "DescribeStackEvents",
            &[member("StackName", row.stack_name)],
        )
        .await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES) && !events.contains("VALIDATION_ERROR"),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = stack_event_reasons(&router, REGION, row.stack_name).await;
        let envelope = format!("(Service: Dlm, Status Code: {},", row.status);
        let reported = reasons
            .iter()
            .find(|reason| reason.contains(&sentence))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a failure reason carrying {sentence:?}, got {reasons:?}")
            });
        let (quoted, class) = quoted_and_class(case, reported);
        assert!(
            quoted.contains(&envelope) && quoted.contains("SDK Attempt Count:"),
            "case {case:?}: expected the quoted message to carry {envelope:?} and the SDK's \
             attempt count, got {quoted:?}"
        );
        assert_eq!(
            class, row.class,
            "case {case:?}: expected the refusal to be classified {:?}, got {class:?}",
            row.class
        );

        // And nothing was left behind: that service holds no policy for the declared scope.
        let listed = list_policies(&router, VOLUME, (row.stack_name, "sim2real")).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected that service to hold no policy for {:?}, got {listed:?}",
            row.stack_name
        );
    }
}

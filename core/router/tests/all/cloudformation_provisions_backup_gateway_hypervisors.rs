// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::BackupGateway::Hypervisor` deploys: the
//! hypervisor is imported into Backup Gateway's own control plane, the stack publishes the ARN
//! that service minted, deleting the stack takes the hypervisor away — and a declaration that
//! service REFUSES fails the resource with its own refusal and rolls the stack back.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what the stack
//! publishes is what that service answered, and what that service refuses is a FAILED RESOURCE
//! rather than a template-format error.** Two crates have to agree for that to hold
//! (CloudFormation submits the template and makes the calls, Backup Gateway serves them), so
//! the suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows of the deploy table differ along what the rule generalises over: the form of the
//! host (an IPv4 address and a fully-qualified domain name, the two `Host`'s own documentation
//! names), which optional members the declaration carries (none, the credentials, the
//! encryption key and the type's own `Tags`, the log group only `UpdateHypervisor` takes),
//! which published return value the output reads (`Ref`, `Fn::GetAtt HypervisorArn`), the
//! template spelling (JSON with long-form intrinsics, YAML with the short-form tags), and how
//! many hypervisors one stack declares.
//!
//! The refusal rows differ along the CLASS of constraint the declaration violates, and along
//! WHERE that class is decided. A value outside the pattern the type's own schema publishes —
//! a `Name` over the wrong alphabet, a `KmsKeyArn` that is no key ARN, a `LogGroupArn` that is
//! no log-group ARN — is refused by the stage that runs BEFORE any provider is invoked, so
//! nothing is imported at all and the operation's own validation event carries the detail. A
//! constraint that stage does not judge — a `Name` longer than its modelled maximum, a `Host`
//! shorter than its modelled minimum — reaches Backup Gateway, whose own refusal fails the
//! resource. Neither is ever the `Template format error: Unrecognized resource types` a type
//! CloudFormation does not know is refused with, which no row of any table here may see.
//!
//! Every wait here is a wait for a CONDITION — the stack's own status — under a deadline
//! generous enough that a loaded host cannot decide the outcome; nothing asserts how long a
//! step took.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::patterned_properties::{
    pattern_failure_reason, PATTERNED_PROPERTIES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::{
    PROPERTY_VALIDATION, REDACTED_VALUE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stacks and their
/// hypervisors belong to.
const REGION: &str = "us-east-1";

/// How long the suite is willing to wait for a stack operation to settle, and how often it
/// looks again. Generous: a freshly imported hypervisor is `PENDING` until Backup Gateway has
/// reached the host, which is a transition that service's own clock owes, and the suite waits
/// for the CONDITION (the stack's status) rather than for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(50);

/// Backup Gateway's signing name and the `X-Amz-Target` prefix its service shape's own name
/// gives, over the awsJson1_0 wire its model declares.
const BACKUP_GATEWAY: &str = "backup-gateway";
const TARGET_PREFIX: &str = "BackupOnPremises_v20210101";
const JSON_1_0: &str = "application/x-amz-json-1.0";

/// The resource type under test, spelled as the "AWS resource and property types reference"
/// spells it.
const HYPERVISOR: &str = "AWS::BackupGateway::Hypervisor";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The `EventType` a validation result is reported under, from the model's own `EventType`
/// enum (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// The `HypervisorState` a hypervisor Backup Gateway has reached is in: "The possible states
/// are `PENDING`, `ONLINE`, `OFFLINE`, or `ERROR`" (`HypervisorDetails$State`).
const ONLINE: &str = "ONLINE";

/// A KMS key ARN inside `KmsKeyArn`'s own pattern, for the rows that state one.
const KMS_KEY_ARN: &str =
    "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab";

/// A log-group ARN inside `LogGroupArn`'s own pattern, for the rows that state one.
const LOG_GROUP_ARN: &str =
    "arn:aws:logs:us-east-1:000000000000:log-group:/aws/backup-gateway/hypervisors:*";

/// A second one, for the update that re-points a hypervisor's log group.
const OTHER_LOG_GROUP_ARN: &str =
    "arn:aws:logs:us-east-1:000000000000:log-group:/aws/backup-gateway/moved:*";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
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
        service_com_amazonaws_backupgateway_backuponpremises_v20210101::EmulatedService::new(
            engine,
        ),
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

/// Call one Backup Gateway operation over the awsJson1_0 wire its model declares.
async fn backup_gateway(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{BACKUP_GATEWAY}.{REGION}.amazonaws.com"))
        .header("content-type", JSON_1_0)
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(BACKUP_GATEWAY))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `GetHypervisor` on one ARN: the `HypervisorDetails` that service reports, or `None` where
/// it reports no hypervisor at all.
async fn get_hypervisor(router: &MultiServiceRouter, arn: &str) -> Option<Value> {
    let (status, answered) =
        backup_gateway(router, "GetHypervisor", &json!({ "HypervisorArn": arn })).await;
    if status != 200 {
        return None;
    }
    answered.get("Hypervisor").cloned()
}

/// Every hypervisor Backup Gateway holds in this Region, as `ListHypervisors` reports them.
async fn list_hypervisors(router: &MultiServiceRouter) -> Vec<Value> {
    let (_, answered) = backup_gateway(router, "ListHypervisors", &json!({})).await;
    answered
        .get("Hypervisors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The tags Backup Gateway holds on one of its resources, as the map a client reads them as.
async fn tags_of(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let (_, answered) = backup_gateway(
        router,
        "ListTagsForResource",
        &json!({ "ResourceArn": arn }),
    )
    .await;
    answered
        .get("Tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    Some((
                        tag.get("Key").and_then(Value::as_str)?.to_string(),
                        tag.get("Value").and_then(Value::as_str)?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `DescribeStacks` answer once the stack rests in a status that is not in progress — or
/// once it is GONE, which a delete leaves it — or the last one read when the deadline passes.
/// Waits for the CONDITION, looking again until the stack has settled.
async fn settled_stack(router: &MultiServiceRouter, stack_name: &str) -> (u16, String) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        let settled = status != 200
            || opt_element(&body, "StackStatus")
                .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= deadline {
            return (status, body);
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Create the stack and answer its settled `DescribeStacks` body, asserting the template was
/// ADMITTED: a type AWS publishes is never refused as an unrecognised one.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected {HYPERVISOR} to be a type a template may declare, got {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let (status, settled) = settled_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {settled}"
    );
    settled
}

/// Whether `arn` has the form a `ServerArn` of a hypervisor publishes
/// (`^arn:(aws|aws-cn|aws-us-gov):backup-gateway(:[a-zA-Z-0-9]+){3}\/[a-zA-Z-0-9]+$`).
fn is_hypervisor_arn(arn: &str) -> bool {
    let prefix = format!("arn:aws:{BACKUP_GATEWAY}:{REGION}:");
    arn.starts_with(&prefix) && arn.contains(":hypervisor/") && !arn.ends_with(":hypervisor/")
}

/// What one row expects of one hypervisor the stack declares.
struct Declared {
    /// The output key whose value is the hypervisor's minted ARN.
    arn_output: &'static str,
    /// The host the declaration named, which the read must report.
    host: &'static str,
    /// The name it asked for, which the read must report.
    name: &'static str,
    /// The encryption key the declaration states, where it states one.
    kms_key_arn: Option<&'static str>,
    /// The log group it states, where it states one — the member only the update carries.
    log_group_arn: Option<&'static str>,
    /// The tags it asks for, which that service's own tag read must answer with.
    tags: &'static [(&'static str, &'static str)],
}

/// One row: a template and the hypervisors it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    declared: Vec<Declared>,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own stack: one hypervisor on a host stated as an IPv4 address, with the
        // published return value read back through `Ref`, in JSON with long-form intrinsics.
        Row {
            case: "one hypervisor, Ref read back, JSON spelling",
            stack_name: "backup-gateway-hypervisor-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Hypervisor": {
                        "Type": HYPERVISOR,
                        "Properties": { "Host": "10.0.0.11", "Name": "a-hypervisor" }
                    }
                },
                "Outputs": {
                    "HypervisorRef": {"Value": {"Ref": "Hypervisor"}},
                    "HypervisorArn": {
                        "Value": {"Fn::GetAtt": ["Hypervisor", "HypervisorArn"]}
                    }
                }
            })
            .to_string(),
            declared: vec![Declared {
                arn_output: "HypervisorRef",
                host: "10.0.0.11",
                name: "a-hypervisor",
                kms_key_arn: None,
                log_group_arn: None,
                tags: &[],
            }],
        },
        // A host stated as a fully-qualified domain name, with the `@sensitive` credentials the
        // import takes and the log group only `UpdateHypervisor` carries, read back through
        // `Fn::GetAtt HypervisorArn`.
        Row {
            case: "an FQDN host with credentials and a log group",
            stack_name: "backup-gateway-hypervisor-fqdn",
            template: json!({
                "Resources": {
                    "Hypervisor": {
                        "Type": HYPERVISOR,
                        "Properties": {
                            "Host": "vcenter.example.internal",
                            "Name": "hypervisor-fqdn",
                            "Username": "backup-operator",
                            "Password": "a-pass-phrase",
                            "LogGroupArn": LOG_GROUP_ARN,
                        }
                    }
                },
                "Outputs": {
                    "HypervisorArn": {
                        "Value": {"Fn::GetAtt": ["Hypervisor", "HypervisorArn"]}
                    }
                }
            })
            .to_string(),
            declared: vec![Declared {
                arn_output: "HypervisorArn",
                host: "vcenter.example.internal",
                name: "hypervisor-fqdn",
                kms_key_arn: None,
                log_group_arn: Some(LOG_GROUP_ARN),
                tags: &[],
            }],
        },
        // The YAML spelling with short-form tags, carrying the encryption key and the type's
        // own `Tags`.
        Row {
            case: "the encryption key and declared tags, YAML spelling",
            stack_name: "backup-gateway-hypervisor-yaml",
            template: format!(
                "\
Resources:
  Hypervisor:
    Type: {HYPERVISOR}
    Properties:
      Host: 192.168.40.5
      Name: hypervisor-yaml
      KmsKeyArn: {KMS_KEY_ARN}
      Tags:
        - Key: team
          Value: platform
Outputs:
  HypervisorArn:
    Value: !GetAtt Hypervisor.HypervisorArn
"
            ),
            declared: vec![Declared {
                arn_output: "HypervisorArn",
                host: "192.168.40.5",
                name: "hypervisor-yaml",
                kms_key_arn: Some(KMS_KEY_ARN),
                log_group_arn: None,
                tags: &[("team", "platform")],
            }],
        },
        // Two hypervisors in one stack, each publishing its OWN minted ARN.
        Row {
            case: "two hypervisors in one stack",
            stack_name: "backup-gateway-hypervisor-pair",
            template: json!({
                "Resources": {
                    "First": {
                        "Type": HYPERVISOR,
                        "Properties": { "Host": "10.0.0.13", "Name": "first-hypervisor" }
                    },
                    "Second": {
                        "Type": HYPERVISOR,
                        "Properties": { "Host": "10.0.0.14", "Name": "second-hypervisor" }
                    }
                },
                "Outputs": {
                    "FirstArn": {"Value": {"Fn::GetAtt": ["First", "HypervisorArn"]}},
                    "SecondArn": {"Value": {"Ref": "Second"}}
                }
            })
            .to_string(),
            declared: vec![
                Declared {
                    arn_output: "FirstArn",
                    host: "10.0.0.13",
                    name: "first-hypervisor",
                    kms_key_arn: None,
                    log_group_arn: None,
                    tags: &[],
                },
                Declared {
                    arn_output: "SecondArn",
                    host: "10.0.0.14",
                    name: "second-hypervisor",
                    kms_key_arn: None,
                    log_group_arn: None,
                    tags: &[],
                },
            ],
        },
    ]
}

/// The text one member of a reported hypervisor carries, for the comparisons below.
fn reported(hypervisor: &Value, member: &str) -> Option<String> {
    hypervisor
        .get(member)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_hypervisor_backup_gateway_imported() {
    for row in rows() {
        let router = build_router();
        let case = row.case;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body}"
        );
        let outputs: BTreeMap<String, String> = outputs_of(&body);

        for declared in &row.declared {
            let arn = outputs
                .get(declared.arn_output)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no {} output in {body}", declared.arn_output)
                })
                .clone();
            assert!(
                is_hypervisor_arn(&arn),
                "case {case:?}: expected the published value to be an ARN of the modelled \
                 hypervisor form, got {arn:?}"
            );

            // The identifier the stack published is one the OWNING service resolves, and the
            // hypervisor it resolves to is the one the template declared.
            let hypervisor = get_hypervisor(&router, &arn).await.unwrap_or_else(|| {
                panic!("case {case:?}: expected Backup Gateway to hold the published ARN {arn:?}")
            });
            assert_eq!(
                reported(&hypervisor, "Host").as_deref(),
                Some(declared.host),
                "case {case:?}: expected the declared host, got {hypervisor}"
            );
            assert_eq!(
                reported(&hypervisor, "Name").as_deref(),
                Some(declared.name),
                "case {case:?}: expected the declared name, got {hypervisor}"
            );
            assert_eq!(
                reported(&hypervisor, "HypervisorArn").as_deref(),
                Some(arn.as_str()),
                "case {case:?}: expected the record to carry the ARN the stack published, got \
                 {hypervisor}"
            );
            // A stack is not complete while the hypervisor it imported is still `PENDING`:
            // Backup Gateway has reached the host by the time the resource is reported created.
            assert_eq!(
                reported(&hypervisor, "State").as_deref(),
                Some(ONLINE),
                "case {case:?}: expected a completed create to leave the hypervisor {ONLINE}, \
                 got {hypervisor}"
            );
            assert_eq!(
                reported(&hypervisor, "KmsKeyArn").as_deref(),
                declared.kms_key_arn,
                "case {case:?}: expected the declared encryption key, got {hypervisor}"
            );
            assert_eq!(
                reported(&hypervisor, "LogGroupArn").as_deref(),
                declared.log_group_arn,
                "case {case:?}: expected the declared log group, got {hypervisor}"
            );

            let held = tags_of(&router, &arn).await;
            for (key, value) in declared.tags {
                assert_eq!(
                    held.get(*key).map(String::as_str),
                    Some(*value),
                    "case {case:?}: expected the declared tag {key:?} on the hypervisor, got \
                     {held:?}"
                );
            }
        }

        // One hypervisor per declaration and no more: the deployment imported what the
        // template declared rather than one import per attempt.
        let listed = list_hypervisors(&router).await;
        assert_eq!(
            listed.len(),
            row.declared.len(),
            "case {case:?}: expected Backup Gateway to hold {} hypervisor(s), got {listed:?}",
            row.declared.len()
        );
    }
}

#[tokio::test]
async fn deleting_the_stack_deletes_the_hypervisors_it_imported() {
    let router = build_router();
    let case = "two hypervisors deleted with their stack";
    let stack_name = "backup-gateway-hypervisor-deleted";
    let template = json!({
        "Resources": {
            "First": {
                "Type": HYPERVISOR,
                "Properties": { "Host": "10.0.0.21", "Name": "deleted-first" }
            },
            "Second": {
                "Type": HYPERVISOR,
                "Properties": { "Host": "10.0.0.22", "Name": "deleted-second" }
            }
        },
        "Outputs": {
            "FirstArn": {"Value": {"Ref": "First"}},
            "SecondArn": {"Value": {"Ref": "Second"}}
        }
    })
    .to_string();

    let body = create_and_settle(&router, case, stack_name, &template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {body}"
    );
    let outputs = outputs_of(&body);
    let arns: Vec<String> = ["FirstArn", "SecondArn"]
        .iter()
        .map(|key| {
            outputs
                .get(*key)
                .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {body}"))
                .clone()
        })
        .collect();
    for arn in &arns {
        assert!(
            get_hypervisor(&router, arn).await.is_some(),
            "case {case:?}: expected Backup Gateway to hold {arn:?} before the delete"
        );
    }

    let (status, deleted) =
        cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );
    let (_, settled) = settled_stack(&router, stack_name).await;

    for arn in &arns {
        assert!(
            get_hypervisor(&router, arn).await.is_none(),
            "case {case:?}: expected {arn:?} to be gone from Backup Gateway, the stack read \
             {settled}"
        );
    }
    let listed = list_hypervisors(&router).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected Backup Gateway to hold no hypervisor, got {listed:?}"
    );
}

#[tokio::test]
async fn a_stack_update_applies_the_change_to_the_hypervisor_backup_gateway_holds() {
    let router = build_router();
    let stack_name = "backup-gateway-hypervisor-updated";
    // The rows differ along which members the re-declaration changes: the name alone, the host
    // alone, and the log group together with the type's own tags. Each is applied to the
    // hypervisor the stack imported — its identity survives, and that service's own read
    // reports the new values.
    let rows: [Redeclared; 3] = [
        Redeclared {
            case: "the name alone",
            properties: json!({ "Host": "10.0.0.31", "Name": "renamed-hypervisor" }),
            host: "10.0.0.31",
            name: "renamed-hypervisor",
            log_group_arn: None,
            tags: &[],
        },
        Redeclared {
            case: "the host alone",
            properties: json!({ "Host": "10.0.0.32", "Name": "renamed-hypervisor" }),
            host: "10.0.0.32",
            name: "renamed-hypervisor",
            log_group_arn: None,
            tags: &[],
        },
        Redeclared {
            case: "the log group and the declared tags together",
            properties: json!({
                "Host": "10.0.0.32",
                "Name": "renamed-hypervisor",
                "LogGroupArn": OTHER_LOG_GROUP_ARN,
                "Tags": [{"Key": "team", "Value": "storage"}],
            }),
            host: "10.0.0.32",
            name: "renamed-hypervisor",
            log_group_arn: Some(OTHER_LOG_GROUP_ARN),
            tags: &[("team", "storage")],
        },
    ];

    let created = create_and_settle(
        &router,
        "the first deployment",
        stack_name,
        &json!({
            "Resources": {
                "Hypervisor": {
                    "Type": HYPERVISOR,
                    "Properties": { "Host": "10.0.0.31", "Name": "a-hypervisor" }
                }
            },
            "Outputs": {"HypervisorRef": {"Value": {"Ref": "Hypervisor"}}}
        })
        .to_string(),
    )
    .await;
    let arn = outputs_of(&created)
        .get("HypervisorRef")
        .unwrap_or_else(|| panic!("no HypervisorRef output in {created}"))
        .clone();

    for row in rows {
        let Redeclared {
            case,
            properties,
            host,
            name,
            log_group_arn: log_group,
            tags,
        } = row;
        let template = json!({
            "Resources": {"Hypervisor": {"Type": HYPERVISOR, "Properties": properties}},
            "Outputs": {"HypervisorRef": {"Value": {"Ref": "Hypervisor"}}}
        })
        .to_string();
        let (status, body) = cloudformation(
            &router,
            "UpdateStack",
            &[("StackName", stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
        );
        let (_, described) = settled_stack(&router, stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, got {described}"
        );
        assert_eq!(
            outputs_of(&described).get("HypervisorRef"),
            Some(&arn),
            "case {case:?}: expected the hypervisor to keep the identity Backup Gateway \
             minted, got {described}"
        );

        // The change reached the hypervisor THAT service holds, not just the stack's record.
        let listed = list_hypervisors(&router).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected the one hypervisor to be changed rather than a second one \
             imported, got {listed:?}"
        );
        let hypervisor = get_hypervisor(&router, &arn).await.unwrap_or_else(|| {
            panic!("case {case:?}: expected Backup Gateway to still hold {arn:?}")
        });
        assert_eq!(
            reported(&hypervisor, "Host").as_deref(),
            Some(host),
            "case {case:?}: expected the re-declared host, got {hypervisor}"
        );
        assert_eq!(
            reported(&hypervisor, "Name").as_deref(),
            Some(name),
            "case {case:?}: expected the re-declared name, got {hypervisor}"
        );
        assert_eq!(
            reported(&hypervisor, "LogGroupArn").as_deref(),
            log_group,
            "case {case:?}: expected the re-declared log group, got {hypervisor}"
        );
        let held = tags_of(&router, &arn).await;
        for (key, value) in tags {
            assert_eq!(
                held.get(*key).map(String::as_str),
                Some(*value),
                "case {case:?}: expected the re-declared tag {key:?}, got {held:?}"
            );
        }
    }
}

/// One row of the UPDATE table: the properties a re-declaration states, and what Backup
/// Gateway must report about the hypervisor the stack already imported afterwards.
struct Redeclared {
    case: &'static str,
    properties: Value,
    host: &'static str,
    name: &'static str,
    log_group_arn: Option<&'static str>,
    tags: &'static [(&'static str, &'static str)],
}

/// One refusal row: a declaration Backup Gateway refuses, and what the failed resource must
/// report.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    /// The properties the declaration states, one of which that service refuses.
    properties: fn() -> Value,
    /// The constraint CLAUSE Backup Gateway words that refusal with, which the resource's
    /// failure reason has to carry — the member's validation path and the requirement it
    /// failed.
    clause: &'static str,
}

/// A `Name` one character longer than its modelled maximum (`@length(1..100)`), built rather
/// than written out.
fn over_long_name() -> Value {
    json!({ "Host": "10.0.0.41", "Name": "a".repeat(101) })
}

const REFUSED: &[Refused] = &[
    // A constraint the stage that runs before provisioning does not judge: a name past its
    // modelled maximum length, which is Backup Gateway's own to refuse.
    Refused {
        case: "a name past the modelled maximum length",
        stack_name: "backup-gateway-hypervisor-long-name",
        properties: over_long_name,
        clause: "at 'name' failed to satisfy constraint: Member must have length less than \
                 or equal to 100",
    },
    // A different member and the opposite bound: a host below its modelled minimum length.
    Refused {
        case: "a host below the modelled minimum length",
        stack_name: "backup-gateway-hypervisor-short-host",
        properties: || json!({ "Host": "ab", "Name": "short-host-hypervisor" }),
        clause: "at 'host' failed to satisfy constraint: Member must have length greater \
                 than or equal to 3",
    },
];

#[tokio::test]
async fn a_declaration_backup_gateway_refuses_rolls_the_stack_back() {
    for row in REFUSED {
        let router = build_router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "Hypervisor": { "Type": HYPERVISOR, "Properties": (row.properties)() }
            },
            "Outputs": {"HypervisorRef": {"Value": {"Ref": "Hypervisor"}}}
        })
        .to_string();

        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the failed create to be rolled back rather than publish \
             an ARN Backup Gateway never minted, got {body}"
        );
        assert!(
            outputs_of(&body).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {body}"
        );

        // The failure is the OWNING service's own refusal, on the resource that failed, and
        // never a refusal of the template: no event reports a template-format error at all.
        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack_name)])
                .await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reasons = elements(&events, "ResourceStatusReason");
        assert!(
            reasons.iter().any(|reason| reason.contains(row.clause)
                && reason.contains("validation error")
                && reason.contains("HandlerErrorCode: InvalidRequest")),
            "case {case:?}: expected a failure reason carrying {:?}, got {reasons:?}",
            row.clause
        );

        // And nothing was imported: Backup Gateway holds no hypervisor at all.
        let listed = list_hypervisors(&router).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected Backup Gateway to hold no hypervisor, got {listed:?}"
        );
    }
}

/// One row of the table the stage BEFORE provisioning refuses: a declaration carrying a value
/// outside the pattern the type's own schema publishes for that property, and the property it
/// is about.
struct RefusedBeforeProvisioning {
    case: &'static str,
    stack_name: &'static str,
    properties: fn() -> Value,
    /// The property the validation must name, whose pattern the value does not match.
    property: &'static str,
}

const REFUSED_BEFORE_PROVISIONING: &[RefusedBeforeProvisioning] = &[
    // The issue's own refusal: a name carrying a character outside the type's documented
    // alphabet.
    RefusedBeforeProvisioning {
        case: "a name outside the published alphabet",
        stack_name: "backup-gateway-hypervisor-bad-name",
        properties: || json!({ "Host": "10.0.0.40", "Name": "a hypervisor!" }),
        property: "Name",
    },
    // A key reference that is no KMS ARN at all, which is the second property class the type's
    // schema publishes a form for.
    RefusedBeforeProvisioning {
        case: "an encryption key that is no KMS key ARN",
        stack_name: "backup-gateway-hypervisor-bad-key",
        properties: || {
            json!({
                "Host": "10.0.0.42",
                "Name": "bad-key-hypervisor",
                "KmsKeyArn": "arn:aws:kms:us-east-1:000000000000:not-a-key/0123456789abcdef",
            })
        },
        property: "KmsKeyArn",
    },
    // A log group that is no log-group ARN: the one declared property the IMPORT does not
    // carry, which this stage judges all the same, since it judges the DECLARATION rather than
    // one call's members.
    RefusedBeforeProvisioning {
        case: "a log group that is no log-group ARN",
        stack_name: "backup-gateway-hypervisor-bad-log-group",
        properties: || {
            json!({
                "Host": "10.0.0.43",
                "Name": "bad-log-group-hypervisor",
                "LogGroupArn": "not-a-log-group-arn",
            })
        },
        property: "LogGroupArn",
    },
];

/// One element of an answered event, decoded — a reason quoting a value names it as the XML
/// document escapes it, and what the assertion is about is the text a client reads.
fn decoded(event: &str, name: &str) -> String {
    elements(event, name).into_iter().next().unwrap_or_default()
}

/// A declaration carrying a value outside the pattern its type publishes never reaches Backup
/// Gateway at all: the stage that runs before any provider is invoked refuses it, the stack
/// rolls back, and the operation's own events carry the validation result — the resource, the
/// pattern, the property's own path, and the value as far as a type carrying credentials among
/// its properties may disclose it.
#[tokio::test]
async fn a_value_outside_its_published_pattern_is_refused_before_anything_is_imported() {
    for row in REFUSED_BEFORE_PROVISIONING {
        let router = build_router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "Hypervisor": { "Type": HYPERVISOR, "Properties": (row.properties)() }
            },
            "Outputs": {"HypervisorRef": {"Value": {"Ref": "Hypervisor"}}}
        })
        .to_string();

        let body = create_and_settle(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the refused declaration to be rolled back rather than \
             publish an ARN Backup Gateway never minted, got {body}"
        );
        assert!(
            outputs_of(&body).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {body}"
        );

        let (_, events) =
            cloudformation(&router, "DescribeEvents", &[("StackName", row.stack_name)]).await;
        assert!(
            !events.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        let reported: Vec<(String, String, String, String, String, String)> =
            members_of(&events)
                .into_iter()
                .filter(|event| opt_element(event, "EventType").as_deref() == Some(VALIDATION_ERROR))
                .map(|event| {
                    (
                        decoded(event, "LogicalResourceId"),
                        decoded(event, "ValidationName"),
                        decoded(event, "ValidationStatus"),
                        decoded(event, "ValidationFailureMode"),
                        decoded(event, "ValidationStatusReason"),
                        decoded(event, "ValidationPath"),
                    )
                })
                .collect();
        let pattern = PATTERNED_PROPERTIES
            .iter()
            .find(|catalogued| {
                catalogued.resource_type == HYPERVISOR && catalogued.property == row.property
            })
            .map(|catalogued| catalogued.pattern)
            .unwrap_or_else(|| panic!("case {case:?}: the pattern of the property is catalogued"));
        assert_eq!(
            reported,
            vec![(
                "Hypervisor".to_string(),
                PROPERTY_VALIDATION.to_string(),
                "FAILED".to_string(),
                "FAIL".to_string(),
                pattern_failure_reason(REDACTED_VALUE, pattern),
                format!("/Resources/Hypervisor/Properties/{}", row.property),
            )],
            "case {case:?}: expected the one validation result the refused value owes, got \
             {reported:?} in {events}"
        );

        // And nothing was asked of Backup Gateway: it holds no hypervisor at all.
        let listed = list_hypervisors(&router).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected Backup Gateway to hold no hypervisor, got {listed:?}"
        );
    }
}

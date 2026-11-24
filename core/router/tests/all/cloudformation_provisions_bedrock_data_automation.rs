// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Bedrock::Blueprint`, an
//! `AWS::Bedrock::DataAutomationProject` or an `AWS::Bedrock::DataAutomationLibrary` deploys:
//! each resource is created in Amazon Bedrock Data Automation's build-time plane, the stack
//! publishes the ARN that service minted, deleting the stack takes the resource away — and a
//! blueprint whose declared `Type` is outside the documented value set fails the resource with
//! that service's own refusal and rolls the stack back.
//!
//! The rules under test, one per table:
//!
//! 1. **A resource type AWS publishes is one a template may declare, and a declaration of it is
//!    deployed by calling the service that OWNS the type — so what the stack publishes is what
//!    that service answered, never a `Template format error: Unrecognized resource types`.**
//!    The rows of the deploy table differ along what that rule generalises over: which of the
//!    three types the stack declares, how many resources one stack declares, which optional
//!    properties the declaration carries (a stage, a description, tags, a customer-managed key,
//!    the output configurations of a project), the form the blueprint's `Schema` is written in
//!    (text and a JSON document), which published return value the output reads (`Ref` and
//!    `Fn::GetAtt`), and the template spelling (JSON with long-form intrinsics, YAML with the
//!    short-form tags).
//!
//! 2. **A changed declaration is APPLIED to the resource the stack created rather than
//!    replacing it**: each resource keeps the ARN that service minted and carries what the
//!    re-declaration states. Its mirror: **a change the owning service's own update CANNOT
//!    carry fails the resource rather than being dropped behind a success** — a project's
//!    `ProjectType` and a blueprint's `Type`, neither of which is a member of that type's
//!    update.
//!
//! 3. **A resource whose create the owning service REFUSED carries no physical resource id at
//!    all, and the stack rolls back with that service's own refusal as the resource's failure
//!    reason** — nothing minted one, so the stack reports none rather than the stand-in the
//!    naming rules computed, and nothing was created.
//!
//! Two crates have to agree for either to hold (CloudFormation submits the template and makes
//! the calls, Amazon Bedrock Data Automation serves them), so the suite lives here: a
//! crate-local test can only ever see one side of it.
//!
//! Every wait here is a wait for a CONDITION — the stack's own status, or the owning service's
//! own answer about a resource — under a deadline generous enough that a loaded host cannot
//! decide the outcome; nothing asserts how long a step took.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::time::Instant;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stacks and the resources
/// belong to.
const REGION: &str = "us-east-1";

/// How long the suite is willing to wait for a stack operation or a resource read to settle, and
/// how often it looks again. Generous: a stack converges over as many clock-owed transitions as
/// it has dependency levels, and the suite waits for the CONDITION rather than for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(50);

/// The three resource types under test, and the signing name every Bedrock-family model
/// declares.
const BLUEPRINT_TYPE: &str = "AWS::Bedrock::Blueprint";
const PROJECT_TYPE: &str = "AWS::Bedrock::DataAutomationProject";
const LIBRARY_TYPE: &str = "AWS::Bedrock::DataAutomationLibrary";
const BEDROCK: &str = "bedrock";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for types AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The `DataAutomationProjectStatus` a built project rests in, and the
/// `DataAutomationLibraryStatus` a created library carries
/// (`smithy_query(query="shape",shape="DataAutomationProjectStatus")` ->
/// COMPLETED | IN_PROGRESS | FAILED;
/// `smithy_query(query="shape",shape="DataAutomationLibraryStatus")` -> ACTIVE | DELETING).
const PROJECT_COMPLETED: &str = "COMPLETED";
const LIBRARY_ACTIVE: &str = "ACTIVE";

/// The clauses the owning service's refusal of a `type` outside its enumerated set carries: the
/// member path, and the value set itself — `smithy_query(query="shape",shape="Type")` lists
/// exactly DOCUMENT, IMAGE, AUDIO and VIDEO, in that order.
const ENUM_CLAUSE: &str =
    "Value at 'type' failed to satisfy constraint: Member must satisfy enum value set: \
     [DOCUMENT, IMAGE, AUDIO, VIDEO]";

/// The SDK diagnostic that service's refusal is reported inside, and the handler class a
/// refused create of an invalid request falls in.
const REFUSAL_DIAGNOSTIC: &str =
    "(Service: Bedrock Data Automation; Operation: CreateBlueprint; Status Code: 400; \
     Error Code: ValidationException; Request ID: ";
const INVALID_REQUEST_CLASS: &str = "HandlerErrorCode: InvalidRequest";

/// A blueprint schema, as the text `BlueprintSchema` declares (a string of 1..100000
/// characters).
const SCHEMA_TEXT: &str = r#"{"class":"invoice","description":"An invoice"}"#;

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
        service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::EmulatedService::new(
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

/// Call one Amazon Bedrock Data Automation operation on the URI its own `@http` binding names,
/// over the restJson1 wire that model declares.
async fn bedrock_data_automation(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body
        .map(|document| serde_json::to_vec(document).expect("the request body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header(
            "host",
            format!("bedrock-data-automation.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/json")
        .header("authorization", authorization(BEDROCK))
        .body(Bytes::from(payload))
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

/// One ARN as a single URI path segment, which is how an `@httpLabel` carrying one travels.
fn urlencoded(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            ':' => "%3A".to_string(),
            '/' => "%2F".to_string(),
            other => other.to_string(),
        })
        .collect()
}

/// Which of the three types a declared resource is, which decides the collection its reads are
/// bound under, the member its read answers it in and the ARN form it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Blueprint,
    Project,
    Library,
}

impl Kind {
    /// The collection one resource of this kind is addressed beneath, from that operation's own
    /// `@http` binding (`POST /blueprints/{blueprintArn}/`, and so on).
    fn collection(self) -> &'static str {
        match self {
            Self::Blueprint => "/blueprints/",
            Self::Project => "/data-automation-projects/",
            Self::Library => "/data-automation-libraries/",
        }
    }

    /// The member a read answers the resource inside.
    fn member(self) -> &'static str {
        match self {
            Self::Blueprint => "blueprint",
            Self::Project => "project",
            Self::Library => "library",
        }
    }

    /// The resource path the ARN of this kind carries, from that ARN member's own pattern.
    fn arn_path(self) -> &'static str {
        match self {
            Self::Blueprint => "blueprint",
            Self::Project => "data-automation-project",
            Self::Library => "data-automation-library",
        }
    }
}

/// The resource Amazon Bedrock Data Automation holds under `arn`, or `None` where it holds
/// none. Every read of the three is bound to `POST` on the resource's own URI, so the request
/// carries an empty document.
async fn read_resource(router: &MultiServiceRouter, kind: Kind, arn: &str) -> Option<Value> {
    let (status, answered) = bedrock_data_automation(
        router,
        "POST",
        &format!("{}{}/", kind.collection(), urlencoded(arn)),
        Some(&json!({})),
    )
    .await;
    if status != 200 {
        return None;
    }
    answered.get(kind.member()).cloned()
}

/// Every blueprint that service holds, as `ListBlueprints` reports them.
async fn list_blueprints(router: &MultiServiceRouter) -> Vec<Value> {
    let (_, answered) =
        bedrock_data_automation(router, "POST", "/blueprints/", Some(&json!({}))).await;
    answered
        .get("blueprints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Whether `arn` has the form Amazon Bedrock Data Automation mints an ARN of this kind in: the
/// `bedrock` namespace, this Region, a twelve-digit account and a resource segment of 12 to 36
/// characters of `[a-zA-Z0-9-]` under the kind's own path (`BlueprintArn`,
/// `DataAutomationProjectArn` and `DataAutomationLibraryArn` each fix that form).
fn is_resource_arn(kind: Kind, arn: &str) -> bool {
    let Some(rest) = arn.strip_prefix(&format!("arn:aws:bedrock:{REGION}:")) else {
        return false;
    };
    let Some((account, identifier)) = rest.split_once(&format!(":{}/", kind.arn_path())) else {
        return false;
    };
    account.len() == 12
        && account.bytes().all(|byte| byte.is_ascii_digit())
        && (12..=36).contains(&identifier.len())
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
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

/// Whether the owning service has stopped holding `arn`, waited out as the CONDITION it is: a
/// library's delete is accepted while that service still holds it, so the read is taken again
/// until it answers nothing or the deadline passes.
async fn gone(router: &MultiServiceRouter, kind: Kind, arn: &str) -> bool {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        if read_resource(router, kind, arn).await.is_none() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Every `ResourceStatusReason` a stack has recorded, which is what a failed deployment has to
/// say for itself: the assertions below quote them, so a row that fails names the reason the
/// deployment gave rather than only the status it reached.
async fn failure_reasons(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (_, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    elements(&events, "ResourceStatusReason")
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
        "case {case:?}: expected the Bedrock Data Automation types to be types a template may \
         declare, got {body}"
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

/// What one row expects of one resource the stack declares.
struct Declared {
    /// Which type it is.
    kind: Kind,
    /// The output key whose value is the ARN that service minted for it.
    arn_output: &'static str,
    /// The members the owning service's own read must report, as (member, value): the values the
    /// declaration stated about the resource.
    reported: &'static [(&'static str, &'static str)],
}

/// One row: a template and the resources it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    declared: Vec<Declared>,
}

/// The `StandardOutputConfiguration` document a project declares, in the TEMPLATE's spelling —
/// every property is the API member of the same name with an upper-cased initial.
fn standard_output_configuration() -> Value {
    json!({
        "Document": {
            "Extraction": {
                "Granularity": { "Types": ["PAGE"] },
                "BoundingBox": { "State": "ENABLED" }
            },
            "GenerativeField": { "State": "ENABLED" }
        }
    })
}

fn rows() -> Vec<Row> {
    vec![
        // A named blueprint whose schema is written as TEXT, its ARN read back through `Ref` —
        // the type's primary identifier.
        Row {
            case: "a named blueprint with a schema written as text, Ref read back",
            stack_name: "bda-blueprint-text-schema",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Blueprint": {
                        "Type": BLUEPRINT_TYPE,
                        "Properties": {
                            "BlueprintName": "cfn-invoice-blueprint",
                            "Type": "DOCUMENT",
                            "Schema": SCHEMA_TEXT,
                        }
                    }
                },
                "Outputs": { "BlueprintRef": { "Value": { "Ref": "Blueprint" } } }
            })
            .to_string(),
            declared: vec![Declared {
                kind: Kind::Blueprint,
                arn_output: "BlueprintRef",
                reported: &[
                    ("blueprintName", "cfn-invoice-blueprint"),
                    ("type", "DOCUMENT"),
                ],
            }],
        },
        // The schema written as a JSON DOCUMENT, beside the type's own tags and the `LIVE`
        // stage, with the ARN read through `Fn::GetAtt` instead.
        Row {
            case: "a blueprint whose schema is a document, tagged, at the LIVE stage",
            stack_name: "bda-blueprint-document-schema",
            template: json!({
                "Resources": {
                    "Blueprint": {
                        "Type": BLUEPRINT_TYPE,
                        "Properties": {
                            "BlueprintName": "cfn-receipt-blueprint",
                            "Type": "IMAGE",
                            "BlueprintStage": "LIVE",
                            "Schema": { "class": "receipt", "description": "A receipt" },
                            "Tags": [{ "Key": "team", "Value": "platform" }],
                        }
                    }
                },
                "Outputs": {
                    "BlueprintArn": { "Value": { "Fn::GetAtt": ["Blueprint", "BlueprintArn"] } }
                }
            })
            .to_string(),
            declared: vec![Declared {
                kind: Kind::Blueprint,
                arn_output: "BlueprintArn",
                reported: &[
                    ("blueprintName", "cfn-receipt-blueprint"),
                    ("type", "IMAGE"),
                    ("blueprintStage", "LIVE"),
                ],
            }],
        },
        // A project alone: the output configurations the declaration states, a description and
        // the `LIVE` stage, its ARN read through `Fn::GetAtt`.
        Row {
            case: "a described project at the LIVE stage",
            stack_name: "bda-project",
            template: json!({
                "Resources": {
                    "Project": {
                        "Type": PROJECT_TYPE,
                        "Properties": {
                            "ProjectName": "cfn-invoice-project",
                            "ProjectDescription": "The project this stack deploys.",
                            "ProjectStage": "LIVE",
                            "StandardOutputConfiguration": standard_output_configuration(),
                        }
                    }
                },
                "Outputs": {
                    "ProjectArn": { "Value": { "Fn::GetAtt": ["Project", "ProjectArn"] } }
                }
            })
            .to_string(),
            declared: vec![Declared {
                kind: Kind::Project,
                arn_output: "ProjectArn",
                reported: &[
                    ("projectName", "cfn-invoice-project"),
                    ("projectDescription", "The project this stack deploys."),
                    ("projectStage", "LIVE"),
                    ("status", PROJECT_COMPLETED),
                ],
            }],
        },
        // A library, in the YAML spelling with the short-form intrinsic, carrying the
        // description its own type declares.
        Row {
            case: "a described library, YAML spelling",
            stack_name: "bda-library",
            template: format!(
                "\
Resources:
  Library:
    Type: {LIBRARY_TYPE}
    Properties:
      LibraryName: cfn-entity-library
      LibraryDescription: The library this stack deploys.
Outputs:
  LibraryArn:
    Value: !GetAtt Library.LibraryArn
"
            ),
            declared: vec![Declared {
                kind: Kind::Library,
                arn_output: "LibraryArn",
                reported: &[
                    ("libraryName", "cfn-entity-library"),
                    ("libraryDescription", "The library this stack deploys."),
                    ("status", LIBRARY_ACTIVE),
                ],
            }],
        },
        // Two resources in one stack, the project reading the blueprint's own published ARN:
        // each publishes the identity that service minted for it, and the project's custom
        // output configuration names a blueprint that really exists.
        Row {
            case: "a project whose custom output configuration names the stack's blueprint",
            stack_name: "bda-blueprint-and-project",
            template: json!({
                "Resources": {
                    "Blueprint": {
                        "Type": BLUEPRINT_TYPE,
                        "Properties": {
                            "BlueprintName": "cfn-joined-blueprint",
                            "Type": "DOCUMENT",
                            "Schema": SCHEMA_TEXT,
                        }
                    },
                    "Project": {
                        "Type": PROJECT_TYPE,
                        "Properties": {
                            "ProjectName": "cfn-joined-project",
                            "StandardOutputConfiguration": standard_output_configuration(),
                            "CustomOutputConfiguration": {
                                "Blueprints": [{
                                    "BlueprintArn": { "Fn::GetAtt": ["Blueprint", "BlueprintArn"] }
                                }]
                            }
                        }
                    }
                },
                "Outputs": {
                    "BlueprintArn": { "Value": { "Ref": "Blueprint" } },
                    "ProjectArn": { "Value": { "Ref": "Project" } }
                }
            })
            .to_string(),
            declared: vec![
                Declared {
                    kind: Kind::Blueprint,
                    arn_output: "BlueprintArn",
                    reported: &[("blueprintName", "cfn-joined-blueprint")],
                },
                Declared {
                    kind: Kind::Project,
                    arn_output: "ProjectArn",
                    reported: &[
                        ("projectName", "cfn-joined-project"),
                        ("status", PROJECT_COMPLETED),
                    ],
                },
            ],
        },
    ]
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_resources_bedrock_data_automation_minted() {
    for row in rows() {
        let router = build_router();
        let case = row.case;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body} with reasons {:?}",
            failure_reasons(&router, row.stack_name).await
        );
        let outputs: BTreeMap<String, String> = outputs_of(&body);

        let (_, resources) = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", row.stack_name)],
        )
        .await;
        let physical_ids = physical_ids_of(&resources);

        for declared in &row.declared {
            let arn = outputs
                .get(declared.arn_output)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no {} output in {body}", declared.arn_output)
                })
                .clone();
            assert!(
                is_resource_arn(declared.kind, &arn),
                "case {case:?}: expected the published value to be an ARN of the modelled \
                 {:?} form, got {arn:?}",
                declared.kind
            );
            assert!(
                physical_ids.values().any(|physical| *physical == arn),
                "case {case:?}: expected the ARN that service minted to be the resource's \
                 physical id, got {physical_ids:?}"
            );

            // The identity the stack published is one the OWNING service resolves, and the
            // resource it resolves to is the one the template declared.
            let reported = read_resource(&router, declared.kind, &arn)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected Amazon Bedrock Data Automation to hold the \
                         published {arn:?}"
                    )
                });
            for (member, value) in declared.reported {
                assert_eq!(
                    reported.get(*member).and_then(Value::as_str),
                    Some(*value),
                    "case {case:?}: expected the read to report {member:?} as {value:?}, got \
                     {reported}"
                );
            }
        }

        // Deleting the stack takes every resource it created away.
        let (status, deleted) =
            cloudformation(&router, "DeleteStack", &[("StackName", row.stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {deleted}"
        );
        let (_, settled) = settled_stack(&router, row.stack_name).await;
        assert!(
            opt_element(&settled, "StackStatus")
                .is_none_or(|stack_status| stack_status == "DELETE_COMPLETE"),
            "case {case:?}: expected the stack to be deleted, got {settled} with reasons {:?}",
            failure_reasons(&router, row.stack_name).await
        );
        for declared in &row.declared {
            let arn = outputs
                .get(declared.arn_output)
                .expect("the output was read above");
            assert!(
                gone(&router, declared.kind, arn).await,
                "case {case:?}: expected the deleted stack's {:?} {arn:?} to be gone",
                declared.kind
            );
        }
    }
}

#[tokio::test]
async fn a_changed_declaration_updates_the_resources_in_place() {
    let case = "a re-declared blueprint schema and project description";
    let router = build_router();
    let stack = "bda-updated";
    // The two properties a re-declaration changes, with everything that identifies either
    // resource left alone: a blueprint's schema and a project's description.
    let template = |schema: &str, description: &str| {
        json!({
            "Resources": {
                "Blueprint": {
                    "Type": BLUEPRINT_TYPE,
                    "Properties": {
                        "BlueprintName": "cfn-updated-blueprint",
                        "Type": "DOCUMENT",
                        "Schema": schema,
                    }
                },
                "Project": {
                    "Type": PROJECT_TYPE,
                    "Properties": {
                        "ProjectName": "cfn-updated-project",
                        "ProjectDescription": description,
                        "StandardOutputConfiguration": standard_output_configuration(),
                    }
                }
            },
            "Outputs": {
                "BlueprintArn": { "Value": { "Ref": "Blueprint" } },
                "ProjectArn": { "Value": { "Ref": "Project" } }
            }
        })
        .to_string()
    };

    let body = create_and_settle(
        &router,
        case,
        stack,
        &template(SCHEMA_TEXT, "The first description."),
    )
    .await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the create to complete, got {body} with reasons {:?}",
        failure_reasons(&router, stack).await
    );
    let created = outputs_of(&body);
    let blueprint_arn = created
        .get("BlueprintArn")
        .unwrap_or_else(|| panic!("case {case:?}: no BlueprintArn output in {body}"))
        .clone();
    let project_arn = created
        .get("ProjectArn")
        .unwrap_or_else(|| panic!("case {case:?}: no ProjectArn output in {body}"))
        .clone();

    let changed_schema = r#"{"class":"invoice","description":"A revised invoice"}"#;
    let (status, updated) = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", stack),
            (
                "TemplateBody",
                &template(changed_schema, "The second description."),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {updated}"
    );
    let (_, settled) = settled_stack(&router, stack).await;
    assert_eq!(
        opt_element(&settled, "StackStatus").as_deref(),
        Some("UPDATE_COMPLETE"),
        "case {case:?}: expected the update to complete, got {settled} with reasons {:?}",
        failure_reasons(&router, stack).await
    );

    // Each resource keeps the identity that service minted, and carries what the
    // re-declaration states rather than what the first deployment gave it.
    let after = outputs_of(&settled);
    assert_eq!(
        (
            after.get("BlueprintArn"),
            after.get("ProjectArn")
        ),
        (Some(&blueprint_arn), Some(&project_arn)),
        "case {case:?}: expected both resources to keep the identities that service minted, \
         got {settled}"
    );
    let blueprint = read_resource(&router, Kind::Blueprint, &blueprint_arn)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected that service to still hold the blueprint"));
    assert_eq!(
        blueprint.get("schema").and_then(Value::as_str),
        Some(changed_schema),
        "case {case:?}: expected the re-declared schema, got {blueprint}"
    );
    let project = read_resource(&router, Kind::Project, &project_arn)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected that service to still hold the project"));
    assert_eq!(
        project.get("projectDescription").and_then(Value::as_str),
        Some("The second description."),
        "case {case:?}: expected the re-declared description, got {project}"
    );

    // One blueprint, changed rather than a second one created beside it.
    assert_eq!(
        list_blueprints(&router).await.len(),
        1,
        "case {case:?}: expected the one blueprint to be changed rather than a second minted"
    );
}

/// The fixed sentence a resource whose change the owning service's own update cannot carry is
/// reported as not applied with, which is what the deployment fails that resource on.
const NOT_APPLIED_IN_PLACE: &str =
    "NotImplemented: this emulator does not change the properties of an existing";

/// One row of the table below: a property no update of its type takes, stated two ways.
struct Retyped {
    case: &'static str,
    stack_name: &'static str,
    /// The template, built from the value that property is stated with.
    template: fn(&str) -> String,
    /// The value the stack is created with, and the one the re-declaration states.
    first: &'static str,
    second: &'static str,
}

#[tokio::test]
async fn a_change_the_owning_update_cannot_carry_is_reported_as_not_applied() {
    // Two rows, one per property no update of its type takes: a project's `ProjectType`
    // (`UpdateDataAutomationProject` has no `projectType` member) and a blueprint's `Type`
    // (`UpdateBlueprint` has no `type` member). Each is a change the owning service cannot be
    // asked to make, so the deployment must FAIL the resource rather than report a write
    // nothing applied.
    let rows = [
        Retyped {
            case: "a re-declared project type",
            stack_name: "bda-project-retyped",
            template: |project_type| {
                json!({
                    "Resources": {
                        "Project": {
                            "Type": PROJECT_TYPE,
                            "Properties": {
                                "ProjectName": "cfn-retyped-project",
                                "ProjectType": project_type,
                                "StandardOutputConfiguration": standard_output_configuration(),
                            }
                        }
                    },
                    "Outputs": { "ProjectArn": { "Value": { "Ref": "Project" } } }
                })
                .to_string()
            },
            first: "ASYNC",
            second: "SYNC",
        },
        Retyped {
            case: "a re-declared blueprint type",
            stack_name: "bda-blueprint-retyped",
            template: |blueprint_type| {
                json!({
                    "Resources": {
                        "Blueprint": {
                            "Type": BLUEPRINT_TYPE,
                            "Properties": {
                                "BlueprintName": "cfn-retyped-blueprint",
                                "Type": blueprint_type,
                                "Schema": SCHEMA_TEXT,
                            }
                        }
                    },
                    "Outputs": { "BlueprintArn": { "Value": { "Ref": "Blueprint" } } }
                })
                .to_string()
            },
            first: "DOCUMENT",
            second: "IMAGE",
        },
    ];

    for row in rows {
        let (case, stack) = (row.case, row.stack_name);
        let router = build_router();
        let body = create_and_settle(&router, case, stack, &(row.template)(row.first)).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body} with reasons {:?}",
            failure_reasons(&router, stack).await
        );

        let (status, updated) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", stack),
                ("TemplateBody", &(row.template)(row.second)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {updated}"
        );
        let (_, settled) = settled_stack(&router, stack).await;
        assert_eq!(
            opt_element(&settled, "StackStatus").as_deref(),
            Some("UPDATE_ROLLBACK_COMPLETE"),
            "case {case:?}: expected the deployment to fail the resource rather than report a \
             change nothing applied, got {settled}"
        );
        let reasons = failure_reasons(&router, stack).await;
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains(NOT_APPLIED_IN_PLACE)),
            "case {case:?}: expected a reason carrying {NOT_APPLIED_IN_PLACE:?}, got {reasons:?}"
        );
    }
}

#[tokio::test]
async fn a_blueprint_type_outside_the_value_set_rolls_the_stack_back() {
    let case = "a blueprint whose Type is outside the documented value set";
    let router = build_router();
    let stack = "bda-blueprint-invalid-type";
    let template = json!({
        "Resources": {
            "Blueprint": {
                "Type": BLUEPRINT_TYPE,
                "Properties": {
                    "BlueprintName": "cfn-refused-blueprint",
                    // Not one of DOCUMENT, IMAGE, AUDIO or VIDEO.
                    "Type": "SPREADSHEET",
                    "Schema": SCHEMA_TEXT,
                }
            }
        },
        "Outputs": { "BlueprintRef": { "Value": { "Ref": "Blueprint" } } }
    })
    .to_string();

    let body = create_and_settle(&router, case, stack, &template).await;
    assert_eq!(
        opt_element(&body, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the failed create to be rolled back rather than publish an \
         identity nothing minted, got {body}"
    );
    assert!(
        outputs_of(&body).is_empty(),
        "case {case:?}: expected no output for a resource nothing created, got {body}"
    );

    // The failure is the OWNING service's own refusal, on the resource that failed, inside the
    // diagnostic its SDK words — never a refusal of the template.
    let (_, events) = cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert!(
        !events.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected no template-format error for a type AWS publishes, got {events}"
    );
    let reasons = elements(&events, "ResourceStatusReason");
    assert!(
        reasons.iter().any(|reason| reason.contains(ENUM_CLAUSE)
            && reason.contains(REFUSAL_DIAGNOSTIC)
            && reason.contains(INVALID_REQUEST_CLASS)),
        "case {case:?}: expected a failure reason carrying {ENUM_CLAUSE:?} inside the SDK \
         diagnostic, got {reasons:?}"
    );

    // Nothing minted an identity for it, so the resource reports no physical id at all...
    let (_, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        physical_ids_of(&resources).get("Blueprint").map(String::as_str),
        None,
        "case {case:?}: expected the refused blueprint to carry no PhysicalResourceId, got \
         {resources}"
    );
    // ...and nothing was created: that service holds no blueprint at all.
    assert!(
        list_blueprints(&router).await.is_empty(),
        "case {case:?}: expected the owning service to hold no blueprint, got some"
    );
}

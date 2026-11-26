// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack may declare an `AWS::Bedrock::ApplicationInferenceProfile`, and a
//! stack that reports `CREATE_COMPLETE` has really created the profile in Amazon Bedrock's
//! own control plane under the identity THAT service minted.
//!
//! The rule under test: **a resource type Amazon Web Services publishes is a type a template
//! may declare — the `Template format error: Unrecognized resource types: [<types>]` refusal
//! is reserved for a type CloudFormation does not publish — and a stack creates each resource
//! it declares by calling the OWNING service's own create API, reporting back the identifiers
//! that service assigned.** Two crates have to agree for that to hold (CloudFormation
//! submits the template and makes the call, Amazon Bedrock's control plane serves it), so the
//! suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: the template spelling (JSON with
//! long-form intrinsics, YAML with the short-form tags), which published return value the
//! output reads back (`Ref`, `Fn::GetAtt InferenceProfileArn`,
//! `Fn::GetAtt InferenceProfileId`), what the declared `ModelSource.CopyFrom` names (a
//! foundation-model ARN, a SYSTEM_DEFINED inference profile ARN that service publishes),
//! where the name comes from (a literal, a stack parameter), how many profiles one stack
//! declares, and whether the declaration carries the type's own `Tags`.
//!
//! The deletion half and the refusal half are the same rule: deleting the stack deletes the
//! profiles it created, a declaration Amazon Bedrock REFUSES leaves the stack rolled back
//! with that service's own diagnostic on the failed resource and no profile behind, and a
//! type CloudFormation really does not publish is still refused at submission.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stacks and the profiles
/// belong to. A region of a geography Amazon Bedrock publishes cross-region inference
/// profiles in, which is what one row's `CopyFrom` reads from that service's own listing.
const REGION: &str = "us-east-1";

/// How long one poll of a stack's status waits before asking again, and how many times: a
/// deployment settles well inside this budget, which is generous so a loaded host does not
/// decide the outcome.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLLS: usize = 100;

/// The signing name the whole Bedrock family shares; the route decides which member serves a
/// request.
const BEDROCK: &str = "bedrock";

/// The type under test, and a type of the SAME namespace that CloudFormation publishes no
/// resource type for — the control for the refusal the issue's template was answered with.
const PROFILE_TYPE: &str = "AWS::Bedrock::ApplicationInferenceProfile";
const UNPUBLISHED_TYPE: &str = "AWS::Bedrock::NotAnInferenceProfile";

/// The refusal a template declaring a type CloudFormation does not publish is answered with,
/// and the wire error code it carries.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";
const VALIDATION_ERROR: &str = "ValidationError";

/// The only value `InferenceProfileStatus` carries
/// (`smithy_query(query="shape",shape="InferenceProfileStatus")`), and the
/// `InferenceProfileType` a profile an account creates is classified as
/// (`smithy_query(query="shape",shape="InferenceProfileType")`: `SYSTEM_DEFINED`,
/// `APPLICATION`).
const ACTIVE: &str = "ACTIVE";
const APPLICATION: &str = "APPLICATION";
const SYSTEM_DEFINED: &str = "SYSTEM_DEFINED";

/// A `FoundationModelArn` of this Region, which is one of the two forms
/// `InferenceProfileModelSource$copyFrom` admits
/// (`^arn:aws(|-us-gov|-cn|-iso|-iso-b):bedrock:(|[0-9a-z-]{0,20}):(|[0-9]{12}):\
/// (inference-profile|foundation-model)/[a-zA-Z0-9-:.]+$`): a model ARN carries no account.
fn foundation_model_arn(model_id: &str) -> String {
    format!("arn:aws:bedrock:{REGION}::foundation-model/{model_id}")
}

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
        service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
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

/// One call on the Bedrock control plane's own restJson1 wire.
async fn bedrock(router: &MultiServiceRouter, method: &str, path: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("bedrock.{REGION}.amazonaws.com"))
        .header("authorization", authorization(BEDROCK))
        .body(Bytes::new())
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

/// `GetInferenceProfile` on one `inferenceProfileIdentifier`
/// (`GET /inference-profiles/{inferenceProfileIdentifier}`), the identifier escaped as a
/// single path segment the way an SDK writes an ARN into a path label.
async fn get_profile(router: &MultiServiceRouter, identifier: &str) -> (u16, Value) {
    let label = identifier
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F");
    bedrock(router, "GET", &format!("/inference-profiles/{label}")).await
}

/// Every profile `ListInferenceProfiles` reports of `profile_type`
/// (`GET /inference-profiles?type=<type>`).
async fn list_profiles(router: &MultiServiceRouter, profile_type: &str) -> Vec<Value> {
    let (status, listed) = bedrock(
        router,
        "GET",
        &format!("/inference-profiles?type={profile_type}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from ListInferenceProfiles, got {status} with {listed}"
    );
    listed
        .get("inferenceProfileSummaries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `inferenceProfileArn` of the first SYSTEM_DEFINED profile Amazon Bedrock publishes in
/// this Region — the source one row's declaration copies from, which is the shape a client
/// deploying an application profile over a cross-region one uses.
async fn published_profile_arn(router: &MultiServiceRouter) -> String {
    let published = list_profiles(router, SYSTEM_DEFINED).await;
    let first = published.first().unwrap_or_else(|| {
        panic!(
            "Amazon Bedrock publishes {SYSTEM_DEFINED} inference profiles in {REGION}, so the \
             listing must carry at least one"
        )
    });
    first
        .get("inferenceProfileArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a listed profile carries an inferenceProfileArn, got {first}"))
        .to_string()
}

/// Submit a template and poll the stack until it first reports a status that is not
/// `*_IN_PROGRESS`; answers the `DescribeStacks` body it was last read from.
///
/// The completion is owed from the instant the resources settle, which no fixed sleep can
/// name, so the condition is polled rather than waited out.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
    parameters: &[(&str, &str)],
) -> String {
    let mut members = vec![("StackName", stack), ("TemplateBody", template)];
    let positional: Vec<(String, String)> = parameters
        .iter()
        .enumerate()
        .flat_map(|(index, (key, value))| {
            let position = index + 1;
            vec![
                (
                    format!("Parameters.member.{position}.ParameterKey"),
                    (*key).to_string(),
                ),
                (
                    format!("Parameters.member.{position}.ParameterValue"),
                    (*value).to_string(),
                ),
            ]
        })
        .collect();
    for (key, value) in &positional {
        members.push((key.as_str(), value.as_str()));
    }
    let (status, body) = cloudformation(router, "CreateStack", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    assert!(
        !body.contains(UNRECOGNIZED_PREFIX),
        "case {case:?}: a template declaring {PROFILE_TYPE} must not be refused as an \
         unrecognized type, got {body}"
    );
    settled(router, case, stack).await
}

/// Poll `DescribeStacks` until the stack reports a status that is not `*_IN_PROGRESS`.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let mut last = String::new();
    for _ in 0..POLLS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        last = body;
        if !reported.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// Poll `DescribeStacks` until the stack reports a settled status OR no longer resolves at
/// all, which is what a stack whose delete has finished does. Answers the body it was last
/// read from, whichever of the two it is.
async fn settled_or_gone(router: &MultiServiceRouter, stack: &str) -> String {
    let mut last = String::new();
    for _ in 0..POLLS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        last = body;
        if status != 200 || !reported.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// The status the last `DescribeStacks` body reports.
fn stack_status(body: &str) -> String {
    opt_element(body, "StackStatus").unwrap_or_default()
}

/// What a row expects of ONE profile the stack declares.
struct Declared {
    /// The output key whose value is the ARN Amazon Bedrock minted for the profile.
    arn_output: &'static str,
    /// The output key whose value is the id that service minted, where the row reads one.
    id_output: Option<&'static str>,
    /// The name the declaration asked for, which the profile has to carry.
    name: &'static str,
}

/// One row: a template, the parameters the create passes, and the profiles it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// Built from the source ARN the row copies from, which one row reads out of that
    /// service's own listing.
    template: fn(&str) -> String,
    /// Whether the row's `CopyFrom` is the published SYSTEM_DEFINED profile rather than a
    /// foundation model ARN.
    copies_published_profile: bool,
    parameters: Vec<(&'static str, &'static str)>,
    declared: Vec<Declared>,
}

fn rows() -> Vec<Row> {
    vec![
        // One profile over a foundation model, in JSON with long-form intrinsics, publishing
        // its ARN through `Ref` and the id that service minted through `Fn::GetAtt`.
        Row {
            case: "one profile over a foundation model, Ref and GetAtt read back, JSON",
            stack_name: "bedrock-app-profile-json",
            template: |source| {
                json!({
                    "Resources": {
                        "Profile": {
                            "Type": PROFILE_TYPE,
                            "Properties": {
                                "InferenceProfileName": "cfn-profile-one",
                                "Description": "Tracks one team s usage",
                                "ModelSource": {"CopyFrom": source},
                            },
                        },
                    },
                    "Outputs": {
                        "ProfileRef": {"Value": {"Ref": "Profile"}},
                        "ProfileId": {
                            "Value": {"Fn::GetAtt": ["Profile", "InferenceProfileId"]}
                        },
                    },
                })
                .to_string()
            },
            copies_published_profile: false,
            parameters: Vec::new(),
            declared: vec![Declared {
                arn_output: "ProfileRef",
                id_output: Some("ProfileId"),
                name: "cfn-profile-one",
            }],
        },
        // The issue's own shape: a profile copied from a SYSTEM_DEFINED profile that service
        // publishes, in YAML with the short-form tags, named by a stack PARAMETER, carrying
        // the type's own `Tags`, and publishing its ARN through
        // `Fn::GetAtt InferenceProfileArn`.
        Row {
            case: "one profile over a published profile, GetAtt ARN read back, YAML",
            stack_name: "bedrock-app-profile-yaml",
            template: |source| {
                format!(
                    "Parameters:\n  \
                       ProfileName:\n    \
                         Type: String\n\
                     Resources:\n  \
                       Profile:\n    \
                         Type: {PROFILE_TYPE}\n    \
                         Properties:\n      \
                           InferenceProfileName: !Ref ProfileName\n      \
                           ModelSource:\n        \
                             CopyFrom: {source}\n      \
                           Tags:\n        \
                             - Key: team\n          \
                               Value: platform\n\
                     Outputs:\n  \
                       ProfileArn:\n    \
                         Value: !GetAtt Profile.InferenceProfileArn\n"
                )
            },
            copies_published_profile: true,
            parameters: vec![("ProfileName", "cfn-profile-two")],
            declared: vec![Declared {
                arn_output: "ProfileArn",
                id_output: None,
                name: "cfn-profile-two",
            }],
        },
        // TWO profiles in one stack, so the rule holds for a stack declaring more than one of
        // the type.
        Row {
            case: "two profiles in one stack",
            stack_name: "bedrock-app-profile-pair",
            template: |source| {
                json!({
                    "Resources": {
                        "First": {
                            "Type": PROFILE_TYPE,
                            "Properties": {
                                "InferenceProfileName": "cfn-profile-first",
                                "ModelSource": {"CopyFrom": source},
                            },
                        },
                        "Second": {
                            "Type": PROFILE_TYPE,
                            "Properties": {
                                "InferenceProfileName": "cfn-profile-second",
                                "ModelSource": {
                                    "CopyFrom": foundation_model_arn("amazon.titan-text-lite-v1")
                                },
                            },
                        },
                    },
                    "Outputs": {
                        "FirstArn": {
                            "Value": {"Fn::GetAtt": ["First", "InferenceProfileArn"]}
                        },
                        "FirstId": {"Value": {"Fn::GetAtt": ["First", "InferenceProfileId"]}},
                        "SecondArn": {"Value": {"Ref": "Second"}},
                    },
                })
                .to_string()
            },
            copies_published_profile: false,
            parameters: Vec::new(),
            declared: vec![
                Declared {
                    arn_output: "FirstArn",
                    id_output: Some("FirstId"),
                    name: "cfn-profile-first",
                },
                Declared {
                    arn_output: "SecondArn",
                    id_output: None,
                    name: "cfn-profile-second",
                },
            ],
        },
    ]
}

#[tokio::test]
async fn a_stack_creates_the_application_inference_profiles_it_declares() {
    for row in rows() {
        let case = row.case;
        let router = build_router();
        let source = if row.copies_published_profile {
            published_profile_arn(&router).await
        } else {
            foundation_model_arn("amazon.nova-lite-v1:0")
        };

        let deployed = deploy(
            &router,
            case,
            row.stack_name,
            &(row.template)(&source),
            &row.parameters,
        )
        .await;
        assert_eq!(
            stack_status(&deployed),
            "CREATE_COMPLETE",
            "case {case:?}: a stack declaring {PROFILE_TYPE} must reach CREATE_COMPLETE, got \
             {deployed}"
        );

        let outputs = outputs_of(&deployed);
        let mut published = Vec::new();
        for declared in &row.declared {
            let arn = outputs
                .get(declared.arn_output)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: no {:?} output in {deployed}",
                        declared.arn_output
                    )
                })
                .clone();

            // The handle the stack published resolves in the service that OWNS the profile,
            // and what that service holds is what the declaration asked for.
            let (status, profile) = get_profile(&router, &arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetInferenceProfile on the published {arn:?} must answer 200, \
                 got {status} with {profile}"
            );
            assert_eq!(
                profile.get("inferenceProfileName").and_then(Value::as_str),
                Some(declared.name),
                "case {case:?}: the profile must carry the declared name, got {profile}"
            );
            assert_eq!(
                profile.get("inferenceProfileArn").and_then(Value::as_str),
                Some(arn.as_str()),
                "case {case:?}: the ARN the stack published must be the one that service \
                 reports, got {profile}"
            );
            assert_eq!(
                profile.get("type").and_then(Value::as_str),
                Some(APPLICATION),
                "case {case:?}: a profile an account creates is classified {APPLICATION}, got \
                 {profile}"
            );
            assert_eq!(
                profile.get("status").and_then(Value::as_str),
                Some(ACTIVE),
                "case {case:?}: the created profile must be {ACTIVE}, got {profile}"
            );
            let models = profile
                .get("models")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                !models.is_empty(),
                "case {case:?}: the profile must track the model its source names, got \
                 {profile}"
            );

            // The id the stack publishes is the one that service minted, not a value
            // CloudFormation decided.
            if let Some(id_output) = declared.id_output {
                let published_id = outputs.get(id_output).unwrap_or_else(|| {
                    panic!("case {case:?}: no {id_output:?} output in {deployed}")
                });
                assert_eq!(
                    profile.get("inferenceProfileId").and_then(Value::as_str),
                    Some(published_id.as_str()),
                    "case {case:?}: the id the stack published must be the one that service \
                     reports, got {profile}"
                );
            }
            published.push(arn);
        }

        // Deleting the stack deletes the profiles it created: the owning service no longer
        // holds one.
        let (status, body) = cloudformation(
            &router,
            "DeleteStack",
            &[("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {body}"
        );
        let after = settled_or_gone(&router, row.stack_name).await;
        assert!(
            after.contains("DELETE_COMPLETE") || after.contains("does not exist"),
            "case {case:?}: a deleted stack reports DELETE_COMPLETE or no longer resolves, got \
             {after}"
        );
        for arn in published {
            let (status, read) = get_profile(&router, &arn).await;
            assert_eq!(
                status, 404,
                "case {case:?}: the profile {arn:?} the stack created must be gone once the \
                 stack is deleted, got {status} with {read}"
            );
        }
    }
}

#[tokio::test]
async fn a_declaration_amazon_bedrock_refuses_rolls_the_stack_back() {
    let router = build_router();
    let case = "a profile name Amazon Bedrock refuses";

    // `InferenceProfileName` is `^([0-9a-zA-Z][ _-]?)+$`
    // (`smithy_query(query="operation",operation="CreateInferenceProfile")`), so a name
    // carrying a `/` is one that service refuses — and the refusal is the resource's, not the
    // template's.
    let template = json!({
        "Resources": {
            "Profile": {
                "Type": PROFILE_TYPE,
                "Properties": {
                    "InferenceProfileName": "cfn/profile/refused",
                    "ModelSource": {"CopyFrom": foundation_model_arn("amazon.nova-lite-v1:0")},
                },
            },
        },
    })
    .to_string();

    let rolled_back = deploy(&router, case, "bedrock-app-profile-refused", &template, &[]).await;
    assert_eq!(
        stack_status(&rolled_back),
        "ROLLBACK_COMPLETE",
        "case {case:?}: a stack whose only resource that service refuses must end \
         ROLLBACK_COMPLETE, got {rolled_back}"
    );

    // The failure is reported on the RESOURCE, and the reason is the owning service's own
    // diagnostic: it names the member Amazon Bedrock refused.
    let (status, events) = cloudformation(
        &router,
        "DescribeStackEvents",
        &[("StackName", "bedrock-app-profile-refused")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with {events}"
    );
    assert!(
        elements(&events, "ResourceStatus").contains(&"CREATE_FAILED".to_string()),
        "case {case:?}: expected a CREATE_FAILED resource event, got {events}"
    );
    let reasons = elements(&events, "ResourceStatusReason");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("inferenceProfileName")),
        "case {case:?}: expected a failure reason naming the member that service refused, got \
         {reasons:?}"
    );

    // And nothing was left behind: the account holds no application profile at all.
    let held = list_profiles(&router, APPLICATION).await;
    assert!(
        held.is_empty(),
        "case {case:?}: a rolled-back stack must leave no inference profile, got {held:?}"
    );
}

#[tokio::test]
async fn a_type_cloudformation_does_not_publish_is_still_refused() {
    let router = build_router();
    let case = "a type of the same namespace CloudFormation publishes no schema for";

    let template = json!({
        "Resources": {
            "NotAProfile": {
                "Type": UNPUBLISHED_TYPE,
                "Properties": {"InferenceProfileName": "cfn-profile-unknown-type"},
            },
        },
    })
    .to_string();

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", "bedrock-unknown-type"),
            ("TemplateBody", &template),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: an unrecognized resource type is refused at submission, got {status} \
         with {body}"
    );
    assert!(
        body.contains(VALIDATION_ERROR)
            && body.contains(&format!("{UNRECOGNIZED_PREFIX}{UNPUBLISHED_TYPE}]")),
        "case {case:?}: expected the {VALIDATION_ERROR} naming {UNPUBLISHED_TYPE}, got {body}"
    );

    // And nothing was created: no stack, and no profile in the owning service.
    let (status, described) = cloudformation(
        &router,
        "DescribeStacks",
        &[("StackName", "bedrock-unknown-type")],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the refused template created no stack, got {status} with {described}"
    );
    let held = list_profiles(&router, APPLICATION).await;
    assert!(
        held.is_empty(),
        "case {case:?}: a refused template creates no inference profile, got {held:?}"
    );
}

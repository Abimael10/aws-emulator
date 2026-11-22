// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares the `AWS::Rekognition::*` resource types deploys: the
//! face collection, the Custom Labels project and the dataset of that project are created in
//! Amazon Rekognition's own control plane, the stack publishes the identifiers that service
//! holds them under, and deleting the stack takes all three away.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by CALLING the service that owns the type — so a `Ref` over
//! such a resource resolves to the identifier that service really knows it by, an
//! `Fn::GetAtt Arn` to the ARN that service reports for it, and the resource is one the owning
//! service returns from its own reads.** Two crates have to agree for that to hold
//! (CloudFormation submits the template and makes the calls, Amazon Rekognition serves them),
//! so the suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: which of the three types the stack
//! declares and how many of each, the FORM of the declared collection id (the alphabet
//! `^[a-zA-Z0-9_.\-]+$` admits — letters, digits, dots, hyphens, underscores — at its
//! one-character minimum and well inside its 255-character maximum), which half of a project's
//! data the dataset is (`TRAIN` and `TEST`, the two the modelled `DatasetType` admits), which
//! published return value each output reads (`Ref`, `Fn::GetAtt Arn`), the template spelling
//! (JSON with long-form intrinsics and YAML with the short-form tags), and whether the
//! declaration carries the types' own `Tags`.
//!
//! No row may ever see `Template format error: Unrecognized resource types`, which is the
//! refusal a type CloudFormation does not know is answered with and the defect this suite pins.
//!
//! Every wait here is a poll for the CONDITION under a generous deadline — a dataset's creation
//! and a project's deletion are transitions the clock owes, and nothing in this suite asserts
//! how long one took.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the stacks' resources belong
/// to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries (`engine::mock_account_id`).
const ACCOUNT: &str = "000000000000";

/// The terminal statuses a `CreateStack` and a `DeleteStack` settle to, as `DescribeStacks`
/// reports them. The polls below wait for a TERMINAL one and then say which it was, so a stack
/// that rolled back fails with its own status rather than by running the deadline out.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";
const TERMINAL_STATUSES: [&str; 6] = [
    CREATE_COMPLETE,
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
    DELETE_COMPLETE,
    "DELETE_FAILED",
];

/// The `DatasetStatus` a dataset whose creation is complete reports (modelled `DatasetStatus`).
const DATASET_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The error code Amazon Rekognition answers for a resource it does not hold.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// How long one poll waits before looking again, and how many times it looks.
///
/// The product is a ceiling on the WAIT and nothing else: a dataset's creation and a project's
/// deletion are spans the owning service owes, so the loops look again until the condition
/// holds, and the deadline is generous enough that a loaded host running the whole workspace's
/// tests at once still gets there.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 1_200;

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// calls CloudFormation makes while provisioning are dispatched back into this same router, so
/// the collection, the project and the dataset the stack deploys are the ones this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
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

/// Call one Amazon Rekognition operation over its `awsJson1_1` wire.
async fn rekognition(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("rekognition.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("RekognitionService.{operation}"))
        .header("authorization", authorization("rekognition"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The `StackStatus` and the whole `DescribeStacks` answer for one stack.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "DescribeStacks on {stack:?} must answer 200, got {status} with {body}"
    );
    (
        opt_element(&body, "StackStatus").unwrap_or_default(),
        body,
    )
}

/// Poll `DescribeStacks` until the stack first reports a TERMINAL status, and answer it with
/// the answer it was read from.
///
/// One deadline over one read, rather than a wait for one named status: a deployment that
/// settles anywhere but the expected status is answered the instant it settles, so a row that
/// rolled back fails naming the status it reached instead of being waited out.
async fn settled(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let mut last = stack_status(router, stack).await;
    for _ in 0..POLLS {
        if TERMINAL_STATUSES.contains(&last.0.as_str()) {
            return last;
        }
        tokio::time::sleep(POLL).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// Every collection id Amazon Rekognition reports for this account, read through its own
/// `ListCollections`.
async fn listed_collections(router: &MultiServiceRouter) -> BTreeSet<String> {
    let (status, answered) = rekognition(router, "ListCollections", &json!({})).await;
    assert_eq!(
        status, 200,
        "ListCollections must answer 200, got {status} with {answered}"
    );
    answered
        .get("CollectionIds")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The error code one refused Rekognition answer carries, as the envelope's own `__type`
/// names it (the shape name, possibly behind a `#`-separated namespace).
fn error_code(answered: &Value) -> String {
    answered
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The ARN a collection of `collection_id` is published under, as `DescribeCollection`'s own
/// `CollectionARN` carries it.
fn collection_arn(collection_id: &str) -> String {
    format!("arn:aws:rekognition:{REGION}:{ACCOUNT}:collection/{collection_id}")
}

/// The project description Amazon Rekognition reports for one project name, or `None` where it
/// reports none.
async fn described_project(router: &MultiServiceRouter, project_name: &str) -> Option<Value> {
    let (status, answered) = rekognition(
        router,
        "DescribeProjects",
        &json!({"ProjectNames": [project_name]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeProjects on {project_name:?} must answer 200, got {status} with {answered}"
    );
    answered
        .get("ProjectDescriptions")
        .and_then(Value::as_array)
        .and_then(|descriptions| descriptions.first())
        .cloned()
}

/// What one row expects of one COLLECTION the stack declares.
struct DeclaredCollection {
    /// The output key whose value must be the declared id (`Ref` over the resource).
    id_output: &'static str,
    /// The output key whose value must be the ARN that service reports, where the row reads
    /// one.
    arn_output: Option<&'static str>,
    /// The `CollectionId` the declaration states, which is the id Rekognition must hold the
    /// collection under.
    collection_id: String,
}

/// What one row expects of one PROJECT the stack declares.
struct DeclaredProject {
    /// The output key whose value must be the declared name (`Ref` over the resource).
    name_output: &'static str,
    /// The output key whose value must be the `ProjectArn` that service minted.
    arn_output: &'static str,
    /// The `ProjectName` the declaration states.
    project_name: String,
}

/// What one row expects of one DATASET the stack declares.
struct DeclaredDataset {
    /// The output key whose value must be the `DatasetArn` that service minted (`Ref` over the
    /// resource).
    arn_output: &'static str,
    /// The `DatasetType` the declaration states, which the dataset must report.
    dataset_type: &'static str,
    /// The name of the project the dataset belongs to, whose own description must list it.
    project_name: String,
}

/// One row: a template and what the resources it declares must be.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    collections: Vec<DeclaredCollection>,
    projects: Vec<DeclaredProject>,
    datasets: Vec<DeclaredDataset>,
}

fn rows() -> Vec<Row> {
    vec![
        // The reported stack: a collection, a project, and the project's TRAIN dataset, with
        // every published return value read back, in JSON with long-form intrinsics. The
        // collection id spans the whole alphabet the modelled pattern admits.
        Row {
            case: "a collection, a project and its TRAIN dataset, JSON spelling",
            stack_name: "rek-estate",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Coll": {
                        "Type": "AWS::Rekognition::Collection",
                        "Properties": {"CollectionId": "rek.estate_faces-01"}
                    },
                    "Proj": {
                        "Type": "AWS::Rekognition::Project",
                        "Properties": {"ProjectName": "rek-estate-labels"}
                    },
                    "Ds": {
                        "Type": "AWS::Rekognition::Dataset",
                        "Properties": {
                            "ProjectArn": {"Fn::GetAtt": ["Proj", "Arn"]},
                            "DatasetType": "TRAIN"
                        }
                    }
                },
                "Outputs": {
                    "CollId": {"Value": {"Ref": "Coll"}},
                    "CollArn": {"Value": {"Fn::GetAtt": ["Coll", "Arn"]}},
                    "ProjName": {"Value": {"Ref": "Proj"}},
                    "ProjArn": {"Value": {"Fn::GetAtt": ["Proj", "Arn"]}},
                    "DsArn": {"Value": {"Ref": "Ds"}}
                }
            })
            .to_string(),
            collections: vec![DeclaredCollection {
                id_output: "CollId",
                arn_output: Some("CollArn"),
                collection_id: "rek.estate_faces-01".to_string(),
            }],
            projects: vec![DeclaredProject {
                name_output: "ProjName",
                arn_output: "ProjArn",
                project_name: "rek-estate-labels".to_string(),
            }],
            datasets: vec![DeclaredDataset {
                arn_output: "DsArn",
                dataset_type: "TRAIN",
                project_name: "rek-estate-labels".to_string(),
            }],
        },
        // TWO collections in one stack, in YAML with the short-form tags, carrying the type's
        // own `Tags`: one id at the modelled one-character minimum, one all digits. Each
        // publishes its OWN declared id and its own ARN.
        Row {
            case: "two tagged collections in one stack, YAML spelling",
            stack_name: "rek-pair",
            template: "\
Resources:
  First:
    Type: AWS::Rekognition::Collection
    Properties:
      CollectionId: a
      Tags:
        - Key: team
          Value: vision
  Second:
    Type: AWS::Rekognition::Collection
    Properties:
      CollectionId: \"20260101\"
Outputs:
  FirstId:
    Value: !Ref First
  FirstArn:
    Value: !GetAtt First.Arn
  SecondId:
    Value: !Ref Second
"
            .to_string(),
            collections: vec![
                DeclaredCollection {
                    id_output: "FirstId",
                    arn_output: Some("FirstArn"),
                    collection_id: "a".to_string(),
                },
                DeclaredCollection {
                    id_output: "SecondId",
                    arn_output: None,
                    collection_id: "20260101".to_string(),
                },
            ],
            projects: Vec::new(),
            datasets: Vec::new(),
        },
        // A project whose TEST dataset is the other half of the modelled `DatasetType`, with no
        // collection beside it: the dataset's `Ref` is the ARN Rekognition minted either way.
        Row {
            case: "a project and its TEST dataset alone",
            stack_name: "rek-test-half",
            template: json!({
                "Resources": {
                    "Proj": {
                        "Type": "AWS::Rekognition::Project",
                        "Properties": {"ProjectName": "rek_test_half.labels"}
                    },
                    "Ds": {
                        "Type": "AWS::Rekognition::Dataset",
                        "Properties": {
                            "ProjectArn": {"Fn::GetAtt": ["Proj", "Arn"]},
                            "DatasetType": "TEST"
                        }
                    }
                },
                "Outputs": {
                    "ProjName": {"Value": {"Ref": "Proj"}},
                    "ProjArn": {"Value": {"Fn::GetAtt": ["Proj", "Arn"]}},
                    "DsArn": {"Value": {"Ref": "Ds"}}
                }
            })
            .to_string(),
            projects: vec![DeclaredProject {
                name_output: "ProjName",
                arn_output: "ProjArn",
                project_name: "rek_test_half.labels".to_string(),
            }],
            datasets: vec![DeclaredDataset {
                arn_output: "DsArn",
                dataset_type: "TEST",
                project_name: "rek_test_half.labels".to_string(),
            }],
            collections: Vec::new(),
        },
    ]
}

/// Submit one row's template and wait for the create to settle, asserting it was admitted and
/// completed. Answers the stack's `Outputs` and its `StackId`.
async fn deployed(router: &MultiServiceRouter, row: &Row) -> (BTreeMap<String, String>, String) {
    let case = row.case;
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", row.stack_name),
            ("TemplateBody", &row.template),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — every AWS::Rekognition::* type is \
         one a template may declare — got {status} with body {body}"
    );
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected every declared type to be recognised, got {body}"
    );
    let (reached, body) = settled(router, row.stack_name).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the create to complete, got {reached} with {body}"
    );
    let stack_id = opt_element(&body, "StackId").unwrap_or_else(|| {
        panic!("case {case:?}: no StackId in the completed stack's answer {body}")
    });
    (outputs_of(&body), stack_id)
}

/// The output `key` carries, panicking with the case and the whole answer when it does not.
fn output(outputs: &BTreeMap<String, String>, case: &str, key: &str) -> String {
    outputs
        .get(key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {outputs:?}"))
        .clone()
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_rekognition_resources_that_service_holds() {
    for row in rows() {
        let router = build_router();
        let case = row.case;
        let (outputs, stack_id) = deployed(&router, &row).await;

        // A COLLECTION: `Ref` is the declared id, which is what Rekognition holds it under, and
        // `Fn::GetAtt Arn` is the ARN that service reports for it.
        let listed = listed_collections(&router).await;
        for declared in &row.collections {
            let published = output(&outputs, case, declared.id_output);
            assert_eq!(
                published, declared.collection_id,
                "case {case:?}: expected Ref over the collection to be the declared \
                 CollectionId {:?}, got {published:?}",
                declared.collection_id
            );
            assert!(
                listed.contains(&declared.collection_id),
                "case {case:?}: expected ListCollections to report the declared collection \
                 {:?}, got {listed:?}",
                declared.collection_id
            );
            let (status, described) = rekognition(
                &router,
                "DescribeCollection",
                &json!({"CollectionId": declared.collection_id}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DescribeCollection on {:?} to answer 200, got {status} \
                 with {described}",
                declared.collection_id
            );
            let reported = described
                .get("CollectionARN")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                reported,
                collection_arn(&declared.collection_id),
                "case {case:?}: expected the reported CollectionARN to be the collection's own \
                 ARN, got {reported:?}"
            );
            if let Some(key) = declared.arn_output {
                let published = output(&outputs, case, key);
                assert_eq!(
                    published, reported,
                    "case {case:?}: expected Fn::GetAtt Arn to be the ARN Rekognition reports \
                     ({reported:?}), got {published:?}"
                );
            }
        }

        // A PROJECT: `Ref` is the declared name, and `Fn::GetAtt Arn` is the `ProjectArn` that
        // service MINTED — which carries a trailing numeric segment no naming rule computes, so
        // a published value without it would name no project.
        for declared in &row.projects {
            let published_name = output(&outputs, case, declared.name_output);
            assert_eq!(
                published_name, declared.project_name,
                "case {case:?}: expected Ref over the project to be the declared ProjectName \
                 {:?}, got {published_name:?}",
                declared.project_name
            );
            let described = described_project(&router, &declared.project_name)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected DescribeProjects to report the declared project \
                         {:?}, it reports none",
                        declared.project_name
                    )
                });
            let reported = described
                .get("ProjectArn")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let published_arn = output(&outputs, case, declared.arn_output);
            assert_eq!(
                published_arn, reported,
                "case {case:?}: expected Fn::GetAtt Arn to be the ProjectArn Rekognition \
                 reports ({reported:?}), got {published_arn:?}"
            );
            let expected_prefix =
                format!("arn:aws:rekognition:{REGION}:{ACCOUNT}:project/{}/", declared.project_name);
            assert!(
                published_arn.starts_with(&expected_prefix)
                    && published_arn[expected_prefix.len()..]
                        .chars()
                        .all(|character| character.is_ascii_digit())
                    && published_arn.len() > expected_prefix.len(),
                "case {case:?}: expected the published ProjectArn to have the modelled \
                 project/<name>/<version> form, got {published_arn:?}"
            );
        }

        // A DATASET: `Ref` is the `DatasetArn` that service minted, the dataset reads back from
        // `DescribeDataset` with its creation complete, and the owning project's own description
        // lists it.
        for declared in &row.datasets {
            let published = output(&outputs, case, declared.arn_output);
            let expected_prefix = format!(
                "arn:aws:rekognition:{REGION}:{ACCOUNT}:project/{}/dataset/{}/",
                declared.project_name,
                declared.dataset_type.to_ascii_lowercase()
            );
            assert!(
                published.starts_with(&expected_prefix)
                    && published[expected_prefix.len()..]
                        .chars()
                        .all(|character| character.is_ascii_digit())
                    && published.len() > expected_prefix.len(),
                "case {case:?}: expected the published DatasetArn to have the modelled \
                 project/<name>/dataset/(train|test)/<version> form, got {published:?}"
            );
            let (status, described) = rekognition(
                &router,
                "DescribeDataset",
                &json!({"DatasetArn": published}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DescribeDataset on the published ARN to answer 200, \
                 got {status} with {described}"
            );
            let reported_status = described
                .pointer("/DatasetDescription/Status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                reported_status, DATASET_CREATE_COMPLETE,
                "case {case:?}: expected the dataset the stack completed to report its creation \
                 complete, got {reported_status:?} in {described}"
            );
            let project = described_project(&router, &declared.project_name)
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected DescribeProjects to report the dataset's project \
                         {:?}, it reports none",
                        declared.project_name
                    )
                });
            let summarised: Vec<(String, String, String)> = project
                .get("Datasets")
                .and_then(Value::as_array)
                .map(|datasets| {
                    datasets
                        .iter()
                        .map(|dataset| {
                            (
                                text_of(dataset, "DatasetArn"),
                                text_of(dataset, "DatasetType"),
                                text_of(dataset, "Status"),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            let expected = (
                published.clone(),
                declared.dataset_type.to_string(),
                DATASET_CREATE_COMPLETE.to_string(),
            );
            assert!(
                summarised.contains(&expected),
                "case {case:?}: expected the project's own Datasets to list {expected:?}, got \
                 {summarised:?}"
            );
        }

        // Deleting the stack takes every one of them away in the service that owns it.
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
        // A deleted stack is no longer described by NAME, so the delete is polled by the
        // `StackId` the create published: that identity outlives the stack's own name.
        let (reached, body) = settled(&router, &stack_id).await;
        assert_eq!(
            reached, DELETE_COMPLETE,
            "case {case:?}: expected the delete to complete, got {reached} with {body}"
        );

        let listed = listed_collections(&router).await;
        for declared in &row.collections {
            assert!(
                !listed.contains(&declared.collection_id),
                "case {case:?}: expected the deleted stack's collection {:?} to be gone, \
                 ListCollections still reports {listed:?}",
                declared.collection_id
            );
            let (status, described) = rekognition(
                &router,
                "DescribeCollection",
                &json!({"CollectionId": declared.collection_id}),
            )
            .await;
            assert_eq!(
                (status, error_code(&described).as_str()),
                (400, RESOURCE_NOT_FOUND),
                "case {case:?}: expected DescribeCollection on the deleted collection {:?} to be \
                 refused as not found, got {status} with {described}",
                declared.collection_id
            );
        }
        for declared in &row.datasets {
            let published = output(&outputs, case, declared.arn_output);
            let (status, described) = rekognition(
                &router,
                "DescribeDataset",
                &json!({"DatasetArn": published}),
            )
            .await;
            assert_eq!(
                (status, error_code(&described).as_str()),
                (400, RESOURCE_NOT_FOUND),
                "case {case:?}: expected DescribeDataset on the deleted dataset {published:?} to \
                 be refused as not found, got {status} with {described}"
            );
        }
        for declared in &row.projects {
            let described = described_project(&router, &declared.project_name).await;
            assert!(
                described.is_none(),
                "case {case:?}: expected the deleted stack's project {:?} to be gone, \
                 DescribeProjects still reports {described:?}",
                declared.project_name
            );
        }
    }
}

/// The text one reported member carries, or the empty string where the answer carries none.
fn text_of(reported: &Value, member: &str) -> String {
    reported
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// RULE: a `CollectionId` is one collection's identity, so a SECOND stack that declares a
/// collection id a collection already holds is not deployed, and the collection the first stack
/// created is left exactly as it was.
///
/// The rows differ along who holds the id first — a stack of this account, and a client that
/// created the collection through Rekognition's own `CreateCollection` outside any stack —
/// because what makes the second declaration impossible is that the id is TAKEN, not who took
/// it.
#[tokio::test]
async fn a_collection_id_another_collection_holds_is_not_deployed_twice() {
    // (case, the id contested, whether a stack takes it first).
    let rows: [(&str, &str, bool); 2] = [
        ("an id another stack of the account holds", "rek-dup-stack", true),
        ("an id a client took outside any stack", "rek-dup-client", false),
    ];
    for (case, collection_id, taken_by_a_stack) in rows {
        let router = build_router();
        let template = json!({
            "Resources": {"Coll": {
                "Type": "AWS::Rekognition::Collection",
                "Properties": {"CollectionId": collection_id}
            }},
            "Outputs": {"CollId": {"Value": {"Ref": "Coll"}}}
        })
        .to_string();

        if taken_by_a_stack {
            let (status, body) = cloudformation(
                &router,
                "CreateStack",
                &[("StackName", "rek-holder"), ("TemplateBody", &template)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the holding stack must be accepted, got {status} with {body}"
            );
            let (reached, body) = settled(&router, "rek-holder").await;
            assert_eq!(
                reached, CREATE_COMPLETE,
                "case {case:?}: the holding stack must complete, got {reached} with {body}"
            );
        } else {
            let (status, answered) = rekognition(
                &router,
                "CreateCollection",
                &json!({"CollectionId": collection_id}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the client's own create must be accepted, got {status} with \
                 {answered}"
            );
        }

        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", "rek-second"), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack must answer 200 — the deployment starts, and the stack \
             reports the outcome — got {status} with {body}"
        );
        let (reached, body) = settled(&router, "rek-second").await;
        assert_ne!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the second stack NOT to complete under an id a collection \
             already holds, got {reached} with {body}"
        );

        // The collection that held the id is untouched: one collection still answers to it.
        let listed = listed_collections(&router).await;
        assert!(
            listed.contains(collection_id),
            "case {case:?}: expected the collection that held {collection_id:?} to survive the \
             refused deployment, ListCollections reports {listed:?}"
        );
        let (status, described) = rekognition(
            &router,
            "DescribeCollection",
            &json!({"CollectionId": collection_id}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the surviving collection to still be described, got \
             {status} with {described}"
        );
        assert_eq!(
            described.get("CollectionARN").and_then(Value::as_str),
            Some(collection_arn(collection_id).as_str()),
            "case {case:?}: expected the surviving collection to keep its own ARN, got \
             {described}"
        );
    }
}

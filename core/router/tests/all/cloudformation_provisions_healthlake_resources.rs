// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::HealthLake::FHIRDatastore` or an
//! `AWS::HealthLake::DataTransformationProfile` deploys: the resource is created in AWS
//! HealthLake's own control plane, the stack publishes the identifiers that service minted,
//! and deleting the stack takes the resource away again.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what the
//! stack publishes is what that service answered, and a client's own read of that service
//! resolves it.** Two crates have to agree for that to hold (CloudFormation submits the
//! template and makes the calls, HealthLake serves them), so the suite lives here: a
//! crate-local test can only ever see one side of it. The
//! `Template format error: Unrecognized resource types` a type CloudFormation does not know
//! is refused with must never be the answer for either type, which every row checks.
//!
//! The data-store rows differ along what the rule generalises over: which published return
//! value the output reads (`Ref`, `Fn::GetAtt DatastoreArn`, `Fn::GetAtt DatastoreEndpoint`,
//! `Fn::GetAtt DatastoreStatus`), whether the declaration names the store at all, whether it
//! carries the type's own `Tags` and an optional encryption configuration, and the template
//! spelling (JSON with long-form intrinsics, YAML with the short-form tags).
//!
//! The profile rows differ along the source format the profile converts from (both the
//! modelled values), whether the declaration carries a description and tags, which output
//! reads which return value, and how many of the type's read-only values the outputs read
//! (the ARN and the id alone, or those beside the version, the target format and the instant
//! HealthLake recorded).
//!
//! Every wait here is a poll for the CONDITION under a generous deadline: nothing sleeps for a
//! fixed time and nothing asserts how long a step took.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{block_of, form_encode, members_of, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stacks belong to.
const REGION: &str = "us-east-1";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for types AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// How long a poll keeps looking before it gives up. Generous on purpose: the spans a stack
/// and a data store each owe the clock are short, the wait is on the CONDITION rather than on
/// a duration, and this deadline is only here so a failure is a failure rather than a hang.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks: short enough that a settled condition is seen
/// promptly, long enough not to spin.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The `StackStatus` values a stack operation is still in flight in, which is what a poll
/// waits out.
const IN_FLIGHT: &[&str] = &[
    "CREATE_IN_PROGRESS",
    "DELETE_IN_PROGRESS",
    "ROLLBACK_IN_PROGRESS",
    "UPDATE_IN_PROGRESS",
    "UPDATE_COMPLETE_CLEANUP_IN_PROGRESS",
    "UPDATE_ROLLBACK_IN_PROGRESS",
    "UPDATE_ROLLBACK_COMPLETE_CLEANUP_IN_PROGRESS",
];

/// The profile mapping the bodies below declare as a profile's DRAFT content: one file path to
/// its content, which is the `StringMap` `ProfileMappingSource$ProfileMapping` takes.
const MAPPING_FILE: &str = "mapping-config.yaml";
const MAPPING_CONTENT: &str = "version: 1\nresourceType: Patient\n";

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
        service_com_amazonaws_healthlake_healthlake::EmulatedService::new(engine),
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

/// Call one AWS HealthLake operation over its awsJson1_0 wire, which dispatches by
/// `X-Amz-Target` (`HealthLake.<Operation>`). Answered with the HTTP status and the parsed
/// document.
async fn healthlake(
    router: &MultiServiceRouter,
    operation: &str,
    request: Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("healthlake.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("HealthLake.{operation}"))
        .header("authorization", authorization("healthlake"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Create a stack, poll it to a status no operation is still in flight in, and answer the
/// `DescribeStacks` body that reported it.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    // The stack-level `Tags` every row carries, which CloudFormation propagates to each
    // resource it creates in the stack beside the three that name the stack and the
    // resource's place in it.
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", template),
            ("Tags.member.1.Key", "xsvc-healthlake"),
            ("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — an AWS::HealthLake::* resource is \
         a type a template may declare — got {status} with body {body}"
    );
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected the type to be recognised, got {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// Poll `DescribeStacks` until the stack reports a status no operation is still in flight in,
/// and answer the body that reported it.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !IN_FLIGHT.contains(&reported.as_str()) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never settled; it still reports {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Delete a stack and poll until CloudFormation no longer reports it as an operation in
/// flight: the deletion is finished when the stack reports `DELETE_COMPLETE` or the service no
/// longer resolves it at all.
async fn delete_and_settle(router: &MultiServiceRouter, case: &str, stack_name: &str) {
    let (status, body) =
        cloudformation(router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if status != 200 || !IN_FLIGHT.contains(&reported.as_str()) {
            assert!(
                status != 200 || reported == "DELETE_COMPLETE",
                "case {case:?}: expected the stack's deletion to complete, got {reported:?} \
                 ({body})"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack's deletion never finished; it still reports {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One output of a settled stack, by its key.
fn output(case: &str, body: &str, key: &str) -> String {
    let outputs: BTreeMap<String, String> = outputs_of(body);
    outputs
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {body}"))
}

/// The keys of the declared outputs a `DescribeStacks` answer reports with NO `OutputValue`
/// member at all.
///
/// `Output$OutputValue` is optional, and this service leaves it out for an output whose value
/// it could not evaluate — so a deployed stack that reports an output without one is a
/// published return value nothing resolved, which is exactly what a client reading the
/// outputs as a map breaks on.
fn outputs_with_no_value(body: &str) -> Vec<String> {
    let outputs = block_of(body, "Outputs").unwrap_or_default();
    members_of(outputs)
        .into_iter()
        .filter(|member| opt_element(member, "OutputValue").is_none())
        .map(|member| opt_element(member, "OutputKey").unwrap_or_default())
        .collect()
}

/// Whether `id` has the 32-lowercase-hexadecimal form every identifier HealthLake mints
/// carries (the `ProfileIdString` pattern `^[a-f0-9]{32}$`, and the 32 characters the
/// `AmazonResourceName` pattern's `datastore/fhir/.{32}` admits for a data store).
fn is_healthlake_id(id: &str) -> bool {
    id.len() == 32 && id.chars().all(|character| character.is_ascii_hexdigit())
        && id.chars().all(|character| !character.is_ascii_uppercase())
}

/// What one data-store row expects of the store its stack declares.
struct Datastore {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    /// The output key whose value is the id HealthLake minted, where the row reads one.
    id_output: &'static str,
    /// The output key whose value is the ARN that service minted.
    arn_output: &'static str,
    /// The output keys whose values are the endpoint it serves the store at and the status it
    /// reports the store in, where the row reads them.
    endpoint_output: Option<&'static str>,
    status_output: Option<&'static str>,
    /// The name the declaration asked for, which the described store must carry.
    declared_name: Option<&'static str>,
}

fn datastore_rows() -> Vec<Datastore> {
    vec![
        // The issue's own stack: the required release version with a name and the type's own
        // tags, with every published return value read back, in JSON.
        Datastore {
            case: "a named data store with every return value read back, JSON spelling",
            stack_name: "healthlake-datastore-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {"Datastore": {
                    "Type": "AWS::HealthLake::FHIRDatastore",
                    "Properties": {
                        "DatastoreTypeVersion": "R4",
                        "DatastoreName": "xsvc-healthlake-round-trip",
                        "Tags": [{"Key": "sim2real-check", "Value": "round-trip"}],
                    },
                }},
                "Outputs": {
                    "DatastoreId": {"Value": {"Ref": "Datastore"}},
                    "DatastoreArn": {"Value": {"Fn::GetAtt": ["Datastore", "DatastoreArn"]}},
                    "DatastoreEndpoint": {
                        "Value": {"Fn::GetAtt": ["Datastore", "DatastoreEndpoint"]},
                    },
                    "DatastoreStatus": {
                        "Value": {"Fn::GetAtt": ["Datastore", "DatastoreStatus"]},
                    },
                },
            })
            .to_string(),
            id_output: "DatastoreId",
            arn_output: "DatastoreArn",
            endpoint_output: Some("DatastoreEndpoint"),
            status_output: Some("DatastoreStatus"),
            declared_name: Some("xsvc-healthlake-round-trip"),
        },
        // A store the declaration does not NAME at all — the type requires only the release
        // version — carrying an optional structured property, with the id read through
        // `Fn::GetAtt DatastoreId` rather than through `Ref`.
        Datastore {
            case: "an unnamed data store with an encryption configuration",
            stack_name: "healthlake-datastore-unnamed",
            template: json!({
                "Resources": {"Store": {
                    "Type": "AWS::HealthLake::FHIRDatastore",
                    "Properties": {
                        "DatastoreTypeVersion": "R4",
                        "SseConfiguration": {
                            "KmsEncryptionConfig": {"CmkType": "AWS_OWNED_KMS_KEY"},
                        },
                    },
                }},
                "Outputs": {
                    "Id": {"Value": {"Fn::GetAtt": ["Store", "DatastoreId"]}},
                    "Arn": {"Value": {"Fn::GetAtt": ["Store", "DatastoreArn"]}},
                },
            })
            .to_string(),
            id_output: "Id",
            arn_output: "Arn",
            endpoint_output: None,
            status_output: None,
            declared_name: None,
        },
        // The YAML spelling with short-form intrinsics, reading the endpoint and the status
        // that service decides.
        Datastore {
            case: "the YAML spelling, reading the endpoint and the status",
            stack_name: "healthlake-datastore-yaml",
            template: "\
Resources:
  Datastore:
    Type: AWS::HealthLake::FHIRDatastore
    Properties:
      DatastoreTypeVersion: R4
      DatastoreName: xsvc-healthlake-yaml
Outputs:
  Id:
    Value: !Ref Datastore
  Arn:
    Value: !GetAtt Datastore.DatastoreArn
  Endpoint:
    Value: !GetAtt Datastore.DatastoreEndpoint
  Status:
    Value: !GetAtt Datastore.DatastoreStatus
"
            .to_string(),
            id_output: "Id",
            arn_output: "Arn",
            endpoint_output: Some("Endpoint"),
            status_output: Some("Status"),
            declared_name: Some("xsvc-healthlake-yaml"),
        },
    ]
}

/// HealthLake's own report of one data store, or `None` where it no longer holds one under
/// that id.
async fn described_datastore(
    router: &MultiServiceRouter,
    datastore_id: &str,
) -> Option<Value> {
    let (status, answered) = healthlake(
        router,
        "DescribeFHIRDatastore",
        json!({"DatastoreId": datastore_id}),
    )
    .await;
    match status {
        200 => answered.get("DatastoreProperties").cloned(),
        _ => None,
    }
}

/// Every data-store row: the stack deploys, the store is HealthLake's own, the stack's outputs
/// are exactly what that service reports for it, and deleting the stack takes the store away.
#[tokio::test]
async fn a_deployed_stack_publishes_the_fhir_datastore_healthlake_minted() {
    for row in datastore_rows() {
        let router = build_router();
        let case = row.case;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body}"
        );
        let unresolved = outputs_with_no_value(&body);
        assert!(
            unresolved.is_empty(),
            "case {case:?}: every declared output of a deployed stack must carry a value, \
             these carry none: {unresolved:?} ({body})"
        );

        let datastore_id = output(case, &body, row.id_output);
        assert!(
            is_healthlake_id(&datastore_id),
            "case {case:?}: expected the published id to be one HealthLake mints, got \
             {datastore_id:?}"
        );

        // The identifier the stack published is one the OWNING service resolves, and the store
        // it resolves to is the one the template declared.
        let reported = described_datastore(&router, &datastore_id)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected HealthLake to describe the published id \
                     {datastore_id:?}, it holds no such data store"
                )
            });
        assert_eq!(
            reported.get("DatastoreStatus").and_then(Value::as_str),
            Some("ACTIVE"),
            "case {case:?}: expected the store to be serving once the stack completed, got \
             {reported}"
        );
        assert_eq!(
            reported.get("DatastoreTypeVersion").and_then(Value::as_str),
            Some("R4"),
            "case {case:?}: expected the declared release version, got {reported}"
        );
        assert_eq!(
            reported.get("DatastoreName").and_then(Value::as_str),
            row.declared_name,
            "case {case:?}: expected the declared name, got {reported}"
        );

        // Each published return value is the value that service reports, not one recomputed
        // here.
        let published_arn = output(case, &body, row.arn_output);
        assert_eq!(
            reported.get("DatastoreArn").and_then(Value::as_str),
            Some(published_arn.as_str()),
            "case {case:?}: expected the published ARN to be the one HealthLake reports, got \
             {published_arn:?} against {reported}"
        );
        assert!(
            published_arn.ends_with(&format!(":datastore/fhir/{datastore_id}")),
            "case {case:?}: expected the ARN to name the published id, got {published_arn:?}"
        );
        if let Some(key) = row.endpoint_output {
            let published = output(case, &body, key);
            assert_eq!(
                reported.get("DatastoreEndpoint").and_then(Value::as_str),
                Some(published.as_str()),
                "case {case:?}: expected the published endpoint to be the one HealthLake \
                 serves the store at, got {published:?} against {reported}"
            );
        }
        if let Some(key) = row.status_output {
            let published = output(case, &body, key);
            assert_eq!(
                published, "ACTIVE",
                "case {case:?}: expected the published status to be the one the store reached, \
                 got {published:?}"
            );
        }

        // The store is one of the account's, which that service's own listing reports.
        let (status, listed) = healthlake(&router, "ListFHIRDatastores", json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListFHIRDatastores must answer 200, got {status} {listed}"
        );
        assert!(
            listed
                .get("DatastorePropertiesList")
                .and_then(Value::as_array)
                .map(|stores| stores.iter().any(|store| {
                    store.get("DatastoreId").and_then(Value::as_str) == Some(&datastore_id)
                }))
                .unwrap_or_default(),
            "case {case:?}: expected the listing to carry the published id {datastore_id:?}, \
             got {listed}"
        );

        // Deleting the stack takes the store away in HealthLake itself.
        delete_and_settle(&router, case, row.stack_name).await;
        let after = described_datastore(&router, &datastore_id).await;
        let status_after = after
            .as_ref()
            .and_then(|reported| reported.get("DatastoreStatus"))
            .and_then(Value::as_str)
            .unwrap_or("DELETED");
        assert_eq!(
            status_after, "DELETED",
            "case {case:?}: expected the stack's deletion to remove the store, HealthLake \
             still reports {after:?}"
        );
    }
}

/// What one profile row expects of the profile its stack declares.
struct Profile {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    /// The output key whose value is the ARN the type's `Ref` returns.
    arn_output: &'static str,
    /// The output key whose value is the `ProfileId` HealthLake minted.
    id_output: &'static str,
    /// The name, the source format and the description the declaration asked for.
    declared_name: &'static str,
    source_format: &'static str,
    declared_description: Option<&'static str>,
    /// The output keys whose values are the version the profile was created at, the format it
    /// converts to and the instant HealthLake recorded for it, where the row reads them.
    version_output: Option<&'static str>,
    target_format_output: Option<&'static str>,
    last_updated_output: Option<&'static str>,
}

/// The `Source` a profile declaration states: raw DRAFT content, the variant a CI/CD workflow
/// uses.
fn profile_source() -> Value {
    json!({"ProfileMapping": {"ProfileMapping": {MAPPING_FILE: MAPPING_CONTENT}}})
}

fn profile_rows() -> Vec<Profile> {
    vec![
        // The issue's own stack: a comma-separated-values profile with a description and the
        // type's own tags, publishing the ARN through `Ref` and the id through `Fn::GetAtt`.
        Profile {
            case: "a CSV profile with a description, Ref and Fn::GetAtt ProfileId read back",
            stack_name: "healthlake-profile-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {"Profile": {
                    "Type": "AWS::HealthLake::DataTransformationProfile",
                    "Properties": {
                        "ProfileName": "xsvc-healthlake-csv",
                        "SourceFormat": "CSV",
                        "ProfileDescription": "the round-trip profile",
                        "Source": profile_source(),
                        "Tags": [{"Key": "sim2real-hl", "Value": "round-trip"}],
                    },
                }},
                "Outputs": {
                    "ProfileArn": {"Value": {"Ref": "Profile"}},
                    "ProfileId": {"Value": {"Fn::GetAtt": ["Profile", "ProfileId"]}},
                    "Version": {"Value": {"Fn::GetAtt": ["Profile", "Version"]}},
                    "TargetFormat": {"Value": {"Fn::GetAtt": ["Profile", "TargetFormat"]}},
                    "LastUpdatedAt": {
                        "Value": {"Fn::GetAtt": ["Profile", "LastUpdatedAt"]},
                    },
                },
            })
            .to_string(),
            arn_output: "ProfileArn",
            id_output: "ProfileId",
            declared_name: "xsvc-healthlake-csv",
            source_format: "CSV",
            declared_description: Some("the round-trip profile"),
            version_output: Some("Version"),
            target_format_output: Some("TargetFormat"),
            last_updated_output: Some("LastUpdatedAt"),
        },
        // The other source format the schema states, with no description and no tags, reading
        // the ARN through `Fn::GetAtt ProfileArn` instead of through `Ref`.
        Profile {
            case: "a C-CDA profile with no description, Fn::GetAtt ProfileArn read back",
            stack_name: "healthlake-profile-ccda",
            template: json!({
                "Resources": {"Mapping": {
                    "Type": "AWS::HealthLake::DataTransformationProfile",
                    "Properties": {
                        "ProfileName": "xsvc-healthlake-ccda",
                        "SourceFormat": "CCDA",
                        "Source": profile_source(),
                    },
                }},
                "Outputs": {
                    "Arn": {"Value": {"Fn::GetAtt": ["Mapping", "ProfileArn"]}},
                    "Id": {"Value": {"Fn::GetAtt": ["Mapping", "ProfileId"]}},
                    "CreatedVersion": {"Value": {"Fn::GetAtt": ["Mapping", "Version"]}},
                },
            })
            .to_string(),
            arn_output: "Arn",
            id_output: "Id",
            declared_name: "xsvc-healthlake-ccda",
            source_format: "CCDA",
            declared_description: None,
            version_output: Some("CreatedVersion"),
            target_format_output: None,
            last_updated_output: None,
        },
    ]
}

/// Every profile row: the stack deploys, the profile is HealthLake's own, the stack's outputs
/// name it as that service does, and deleting the stack takes the profile away.
#[tokio::test]
async fn a_deployed_stack_publishes_the_data_transformation_profile_healthlake_minted() {
    for row in profile_rows() {
        let router = build_router();
        let case = row.case;
        let body = create_and_settle(&router, case, row.stack_name, &row.template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body}"
        );
        let unresolved = outputs_with_no_value(&body);
        assert!(
            unresolved.is_empty(),
            "case {case:?}: every declared output of a deployed stack must carry a value, \
             these carry none: {unresolved:?} ({body})"
        );

        let profile_id = output(case, &body, row.id_output);
        assert!(
            is_healthlake_id(&profile_id),
            "case {case:?}: expected the published id to be one HealthLake mints, got \
             {profile_id:?}"
        );
        let profile_arn = output(case, &body, row.arn_output);
        assert!(
            profile_arn.ends_with(&format!(":dataTransformationProfile/{profile_id}")),
            "case {case:?}: expected the ARN to name the published id, got {profile_arn:?}"
        );

        // The ARN the stack published is one the OWNING service resolves: its own tag read of
        // a profile takes exactly that handle.
        let (status, tagged) = healthlake(
            &router,
            "ListTagsForResource",
            json!({"ResourceARN": profile_arn}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HealthLake to resolve the published ARN \
             {profile_arn:?}, got {status} {tagged}"
        );

        // And the profile it resolves to is the one the template declared.
        let (status, reported) = healthlake(
            &router,
            "GetDataTransformationProfile",
            json!({"ProfileId": profile_id}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDataTransformationProfile must answer 200, got {status} \
             {reported}"
        );
        assert_eq!(
            reported.get("ProfileName").and_then(Value::as_str),
            Some(row.declared_name),
            "case {case:?}: expected the declared name, got {reported}"
        );
        assert_eq!(
            reported.get("SourceFormat").and_then(Value::as_str),
            Some(row.source_format),
            "case {case:?}: expected the declared source format, got {reported}"
        );
        assert_eq!(
            reported.get("ProfileDescription").and_then(Value::as_str),
            row.declared_description,
            "case {case:?}: expected the declared description, got {reported}"
        );
        assert_eq!(
            reported
                .get("ProfileMapping")
                .and_then(|mapping| mapping.get(MAPPING_FILE))
                .and_then(Value::as_str),
            Some(MAPPING_CONTENT),
            "case {case:?}: expected the declared mapping content, got {reported}"
        );

        // Each read-only value the type publishes is the one that service reports for the
        // profile, not one recomputed here.
        if let Some(key) = row.version_output {
            let published = output(case, &body, key);
            assert_eq!(
                Some(published.as_str()),
                reported
                    .get("Version")
                    .and_then(Value::as_u64)
                    .map(|version| version.to_string())
                    .as_deref(),
                "case {case:?}: expected the published version to be the one HealthLake \
                 reports, got {published:?} against {reported}"
            );
        }
        if let Some(key) = row.target_format_output {
            let published = output(case, &body, key);
            assert_eq!(
                reported.get("TargetFormat").and_then(Value::as_str),
                Some(published.as_str()),
                "case {case:?}: expected the published target format to be the one HealthLake \
                 reports, got {published:?} against {reported}"
            );
        }
        if let Some(key) = row.last_updated_output {
            let published = output(case, &body, key);
            // The attribute is TEXT, and the text an instant carries is the ISO-8601 form,
            // which parsing it back is what checks.
            assert!(
                published.contains('T') && published.ends_with('Z'),
                "case {case:?}: expected the published instant to be ISO-8601, got \
                 {published:?}"
            );
            let parsed = aws_smithy_types::DateTime::from_str(
                &published,
                aws_smithy_types::date_time::Format::DateTime,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: expected the published instant {published:?} to parse, \
                     got {error}"
                )
            });
            let instant = reported
                .get("LastUpdatedAt")
                .and_then(Value::as_f64)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a reported instant, got {reported}")
                });
            // The two sides are the same instant up to the one microsecond two decompositions
            // of a fractional second can disagree by.
            let apart = (parsed.as_secs_f64() - instant).abs();
            assert!(
                apart < 0.000_002,
                "case {case:?}: expected the published instant to be the one HealthLake \
                 reports ({instant}), got {published:?} ({apart} seconds apart)"
            );
        }

        // The profile is one of the account's under the format it converts from, which that
        // service's own listing reports.
        let (status, listed) = healthlake(
            &router,
            "ListDataTransformationProfiles",
            json!({"SourceFormat": row.source_format}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListDataTransformationProfiles must answer 200, got {status} \
             {listed}"
        );
        assert!(
            listed
                .get("Items")
                .and_then(Value::as_array)
                .map(|items| items.iter().any(|item| {
                    item.get("ProfileId").and_then(Value::as_str) == Some(&profile_id)
                        && item.get("ProfileName").and_then(Value::as_str)
                            == Some(row.declared_name)
                }))
                .unwrap_or_default(),
            "case {case:?}: expected the listing to carry the published id {profile_id:?} \
             under the declared name, got {listed}"
        );

        // Deleting the stack takes the profile away in HealthLake itself.
        delete_and_settle(&router, case, row.stack_name).await;
        let (status, after) = healthlake(
            &router,
            "GetDataTransformationProfile",
            json!({"ProfileId": profile_id}),
        )
        .await;
        assert!(
            !(200..300).contains(&status),
            "case {case:?}: expected the stack's deletion to remove the profile, HealthLake \
             still answers {status} {after}"
        );
        assert_eq!(
            after
                .get("__type")
                .and_then(Value::as_str)
                .map(|code| code.rsplit('#').next().unwrap_or(code)),
            Some("ResourceNotFoundException"),
            "case {case:?}: expected HealthLake's own not-found refusal for the removed \
             profile, got {status} {after}"
        );
    }
}

/// One emulator, several stacks: the declaration a value outside its schema's enum refuses
/// leaves HealthLake holding nothing, and the valid stacks deployed before and after it each
/// publish the profile that service minted for them.
///
/// The rule is the same one the tables above assert, over a sequence rather than a fresh
/// emulator per row: a stack's own deployment is decided by its own declaration, so neither a
/// refused stack nor a stack already deployed changes what the next one publishes.
#[tokio::test]
async fn a_refused_profile_leaves_the_stacks_around_it_publishing_their_own() {
    let router = build_router();
    let case = "a refused profile stack between two deployed ones";

    // A profile the schema admits, deployed first.
    let first = create_and_settle(
        &router,
        case,
        "healthlake-profile-sequence-first",
        &profile_rows()[0].template,
    )
    .await;
    assert_eq!(
        opt_element(&first, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the first stack to deploy, got {first}"
    );
    let first_id = output(case, &first, "ProfileId");

    // A source format the schema does not state: the stack is accepted and rolls back.
    let refused_template = json!({
        "Resources": {"Profile": {
            "Type": "AWS::HealthLake::DataTransformationProfile",
            "Properties": {
                "ProfileName": "xsvc-healthlake-refused",
                "SourceFormat": "XML",
                "Source": profile_source(),
            },
        }},
        "Outputs": {"ProfileArn": {"Value": {"Ref": "Profile"}}},
    })
    .to_string();
    let refused = create_and_settle(
        &router,
        case,
        "healthlake-profile-sequence-refused",
        &refused_template,
    )
    .await;
    assert_eq!(
        opt_element(&refused, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the refused declaration to roll its stack back, got {refused}"
    );

    // A second profile the schema admits, deployed after the refusal: its own outputs resolve
    // and name the profile HealthLake minted for IT.
    let second = create_and_settle(
        &router,
        case,
        "healthlake-profile-sequence-second",
        &profile_rows()[1].template,
    )
    .await;
    assert_eq!(
        opt_element(&second, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the third stack to deploy, got {second}"
    );
    let second_id = output(case, &second, "Id");
    assert_ne!(
        first_id, second_id,
        "case {case:?}: expected each stack to publish its own profile, got {first_id:?} twice"
    );

    // HealthLake holds exactly the two profiles the two deployed stacks asked for, and none
    // for the refused one.
    for (format, held) in [("CSV", &first_id), ("CCDA", &second_id)] {
        let (status, listed) = healthlake(
            &router,
            "ListDataTransformationProfiles",
            json!({"SourceFormat": format}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListDataTransformationProfiles({format}) must answer 200, got \
             {status} {listed}"
        );
        let ids: Vec<&str> = listed
            .get("Items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("ProfileId").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            ids,
            vec![held.as_str()],
            "case {case:?}: expected the {format} listing to hold exactly the profile its \
             stack published, got {listed}"
        );
    }
}

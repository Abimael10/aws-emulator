// SPDX-License-Identifier: CC0-1.0

//! `AWS::HealthLake::FHIRDatastore` and `AWS::HealthLake::DataTransformationProfile` are
//! resource types a template may DECLARE, and a declaration carrying a value their registry
//! schemas do not state for an enum property fails INSIDE the stack rather than at the
//! operation that submitted it.
//!
//! RULE: a resource type Amazon Web Services publishes belongs to the catalogue a submitted
//! template is held to, so a body declaring it is ADMITTED and the type reports the identity
//! and the return values its own "Return values" section publishes; a value outside the set
//! that type's registry schema states for one of its properties is then refused by the
//! pre-deployment validation, which fails the stack and records the schema validator's own
//! sentence about that property — never the
//! `Template format error: Unrecognized resource types: [<types>]` a type CloudFormation does
//! not know is refused with, which no row here may ever see.
//!
//! What the stack then DOES with an admitted declaration — the calls to HealthLake's own
//! `CreateFHIRDatastore` and `CreateDataTransformationProfile`, the identifiers that service
//! mints, the resources' removal with the stack — needs that service mounted beside this one,
//! so it lives in the router's own cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_healthlake_resources.rs`). This suite
//! asserts admission, the catalogue entries behind it, and the in-stack property refusal,
//! which is what a client sees before any resource is attempted.
//!
//! The admission rows differ along what the rule generalises over: which of the two types the
//! body declares, the declared properties (the required ones alone, those plus the type's own
//! `Tags`, those plus an optional structured property), the template spelling (JSON and YAML),
//! how many resources one body declares, and whether the declaration stands beside a resource
//! of another type. The refusal rows differ along which type's property is judged, how the
//! rejected value differs from one the schema states (a version the set does not carry, a
//! format spelled in another case, a format the owning API does not model at all) and against
//! the values each set admits, which are deployed.
//!
//! Nothing here waits out a span: every stack is polled to its terminal status.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, published_attribute_names, replacement_properties,
    HEALTHLAKE_DATA_TRANSFORMATION_PROFILE, HEALTHLAKE_FHIR_DATASTORE,
};

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, xml_blocks, xml_text, REGION,
};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's schema. Source: `state::property_validation::PROPERTY_VALIDATION`.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";

/// The `EventType` a validation result is reported under, from the model's `EventType` enum
/// (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `OperationEvent$ValidationFailureMode` for a validation whose failure was handled by
/// FAILING the operation, and the `ValidationStatus` of one the declaration did not pass.
const FAIL: &str = "FAIL";
const FAILED: &str = "FAILED";

/// The FHIR release versions `AWS::HealthLake::FHIRDatastore`'s schema states for
/// `DatastoreTypeVersion`, as a refusal lists them back. Source: HealthLake's own
/// `FHIRVersion` (`smithy_query(query="shape", shape="FHIRVersion")`), the enum
/// `CreateFHIRDatastore$DatastoreTypeVersion` targets.
const FHIR_VERSIONS: &str = "R4";

/// The source formats `AWS::HealthLake::DataTransformationProfile`'s schema states for
/// `SourceFormat`, in the order a refusal lists them back. Source: that service's own
/// `SourceFormat` (`smithy_query(query="shape", shape="SourceFormat")`).
const SOURCE_FORMATS: &str = "CCDA, CSV";

/// The profile mapping the bodies below declare as the DRAFT content of a profile: one file
/// path to its content, which is the `StringMap` `ProfileMappingSource$ProfileMapping` takes.
const MAPPING_FILE: &str = "mapping-config.yaml";
const MAPPING_CONTENT: &str = "version: 1\nresourceType: Patient\n";

/// How the schema validator reports a value outside the set its property's schema states.
/// Source: `state::property_validation::enum_properties::enum_failure_reason`.
fn not_a_valid_enum_value(value: &str, values: &str) -> String {
    format!("{value} is not a valid enum value. Supported values: [{values}]")
}

/// One admission row: a template body declaring one or both types, and the name of the stack
/// it is submitted under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

/// The `Source` a profile declaration states: raw DRAFT content, the variant a CI/CD workflow
/// uses.
fn profile_source() -> Value {
    json!({"ProfileMapping": {"ProfileMapping": {MAPPING_FILE: MAPPING_CONTENT}}})
}

fn rows() -> Vec<Row> {
    vec![
        // The reported case for the data store: the required version and a name, with every
        // return value read back through `Outputs`.
        Row {
            case: "a data store with its required version, read back through every output",
            stack_name: "healthlake-datastore-admitted",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {"Datastore": {
                    "Type": HEALTHLAKE_FHIR_DATASTORE,
                    "Properties": {
                        "DatastoreTypeVersion": "R4",
                        "DatastoreName": "healthlake-cfn-admitted",
                        "Tags": [{"Key": "team", "Value": "clinical"}],
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
        },
        // The same type with the version alone — the one property its contract requires — and
        // an optional structured property beside it.
        Row {
            case: "a data store with the required version and an encryption configuration",
            stack_name: "healthlake-datastore-minimal",
            template: json!({
                "Resources": {"Datastore": {
                    "Type": HEALTHLAKE_FHIR_DATASTORE,
                    "Properties": {
                        "DatastoreTypeVersion": "R4",
                        "SseConfiguration": {
                            "KmsEncryptionConfig": {"CmkType": "AWS_OWNED_KMS_KEY"},
                        },
                    },
                }},
            })
            .to_string(),
        },
        // The reported case for the profile, with both of its return values read back.
        Row {
            case: "a profile with its raw mapping source, read back through both outputs",
            stack_name: "healthlake-profile-admitted",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {"Profile": {
                    "Type": HEALTHLAKE_DATA_TRANSFORMATION_PROFILE,
                    "Properties": {
                        "ProfileName": "healthlake-cfn-profile",
                        "SourceFormat": "CSV",
                        "ProfileDescription": "the admitted profile",
                        "Source": profile_source(),
                        "Tags": [{"Key": "team", "Value": "clinical"}],
                    },
                }},
                "Outputs": {
                    "ProfileArn": {"Value": {"Ref": "Profile"}},
                    "ProfileId": {"Value": {"Fn::GetAtt": ["Profile", "ProfileId"]}},
                },
            })
            .to_string(),
        },
        // The YAML spelling, with a short-form intrinsic reading one of the data store's
        // attributes.
        Row {
            case: "the YAML spelling of a data store",
            stack_name: "healthlake-datastore-yaml",
            template: format!(
                "\
Resources:
  Datastore:
    Type: {HEALTHLAKE_FHIR_DATASTORE}
    Properties:
      DatastoreTypeVersion: R4
      DatastoreName: healthlake-cfn-yaml
Outputs:
  Endpoint:
    Value: !GetAtt Datastore.DatastoreEndpoint
"
            ),
        },
        // Both types in one body, beside a resource of another type: the catalogue admits
        // every type a body declares or none of them.
        Row {
            case: "a data store and two profiles beside a resource of another type",
            stack_name: "healthlake-mixed",
            template: json!({
                "Resources": {
                    "Datastore": {
                        "Type": HEALTHLAKE_FHIR_DATASTORE,
                        "Properties": {"DatastoreTypeVersion": "R4"},
                    },
                    "FirstProfile": {
                        "Type": HEALTHLAKE_DATA_TRANSFORMATION_PROFILE,
                        "Properties": {
                            "ProfileName": "healthlake-cfn-first",
                            "SourceFormat": "CCDA",
                            "Source": profile_source(),
                        },
                    },
                    "SecondProfile": {
                        "Type": HEALTHLAKE_DATA_TRANSFORMATION_PROFILE,
                        "Properties": {
                            "ProfileName": "healthlake-cfn-second",
                            "SourceFormat": "CSV",
                            "Source": profile_source(),
                        },
                    },
                    "Topic": {
                        "Type": "AWS::SNS::Topic",
                        "Properties": {"TopicName": "healthlake-cfn-topic"},
                    },
                },
            })
            .to_string(),
        },
    ]
}

/// Assert one answer admitted the body rather than refusing its resource types.
fn assert_admitted(case: &str, status_code: u16, body: &str) {
    let message = xml_text(body, "Message").unwrap_or_default();
    assert!(
        !message.starts_with(UNRECOGNIZED_PREFIX),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {message:?}"
    );
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected HTTP 200 for a body declaring a published type, got \
         {status_code} with body {body}"
    );
}

/// Every operation that submits a template for deployment admits a body declaring either
/// HealthLake type, and so does the operation whose whole answer is a verdict on the body.
#[tokio::test]
async fn every_operation_that_takes_a_template_admits_the_healthlake_types() {
    for row in rows() {
        let router = router();
        let (status_code, body) = create_stack(&router, row.stack_name, &row.template, &[]).await;
        assert_admitted(row.case, status_code, &body);
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            stack_id.contains(":stack/"),
            "case {:?}: expected CreateStack to answer a stack id, got {body}",
            row.case
        );

        let (status_code, body) = call(
            &router,
            "ValidateTemplate",
            &[member("TemplateBody", &row.template)],
        )
        .await;
        assert_admitted(row.case, status_code, &body);

        let (status_code, body) = call(
            &router,
            "CreateChangeSet",
            &[
                member("StackName", &format!("{}-change-set", row.stack_name)),
                member("ChangeSetName", "a-change-set"),
                member("ChangeSetType", "CREATE"),
                member("TemplateBody", &row.template),
            ],
        )
        .await;
        assert_admitted(row.case, status_code, &body);
    }
}

/// The catalogue entries the rule rests on: both types are published and recognised, a
/// near-miss spelling of either still is not, and each type reports the identity and the
/// return values its own "Return values" section publishes.
#[test]
fn the_catalogue_publishes_the_healthlake_types_and_what_they_return() {
    for resource_type in [
        HEALTHLAKE_FHIR_DATASTORE,
        HEALTHLAKE_DATA_TRANSFORMATION_PROFILE,
    ] {
        assert!(
            AWS_RESOURCE_TYPES.contains(&resource_type),
            "expected {resource_type:?} in the published catalogue, it is absent"
        );
        assert!(
            is_published(resource_type) && is_recognized(resource_type),
            "expected {resource_type:?} to be a type a template may declare, it is not"
        );
        // HealthLake mints the identifier each resource is addressed by, so the identity in
        // force is the one that service answered rather than a name computed from the
        // template.
        assert!(
            identity_is_service_assigned(resource_type),
            "expected the identity of {resource_type:?} to be the one HealthLake assigns, it \
             is not"
        );
    }
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::HealthLake::FHIRDatastores",
        "AWS::HealthLake::FhirDatastore",
        "AWS::HealthLake::DataTransformationProfiles",
        "AWS::HealthLake::DataTransformationJob",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    assert_eq!(
        published_attribute_names(HEALTHLAKE_FHIR_DATASTORE),
        &[
            "DatastoreArn",
            "DatastoreEndpoint",
            "DatastoreId",
            "DatastoreStatus"
        ],
        "expected the data store's four published return values, got {:?}",
        published_attribute_names(HEALTHLAKE_FHIR_DATASTORE)
    );
    assert_eq!(
        published_attribute_names(HEALTHLAKE_DATA_TRANSFORMATION_PROFILE),
        &[
            "LastUpdatedAt",
            "ProfileArn",
            "ProfileId",
            "TargetFormat",
            "Version"
        ],
        "expected the profile's five published return values, got {:?}",
        published_attribute_names(HEALTHLAKE_DATA_TRANSFORMATION_PROFILE)
    );
    // `UpdateFHIRDatastore` takes neither the release version, the preloaded data nor the
    // encryption key, and `UpdateDataTransformationProfile` takes only a new mapping and a
    // change description — so a declaration restating one of those asks for a different
    // resource.
    assert_eq!(
        replacement_properties(HEALTHLAKE_FHIR_DATASTORE),
        &[
            "DatastoreTypeVersion",
            "PreloadDataConfig",
            "SseConfiguration"
        ],
        "expected the data store's create-only properties, got {:?}",
        replacement_properties(HEALTHLAKE_FHIR_DATASTORE)
    );
    assert_eq!(
        replacement_properties(HEALTHLAKE_DATA_TRANSFORMATION_PROFILE),
        &["KmsKeyId", "ProfileName", "Source", "SourceFormat"],
        "expected the profile's create-only properties, got {:?}",
        replacement_properties(HEALTHLAKE_DATA_TRANSFORMATION_PROFILE)
    );
}

/// One refusal row: the type declared, the logical id it is declared under, the property the
/// schema states a value set for, the value the declaration carries and the value the
/// validation must refuse — `None` for a value the schema states, which is deployed.
struct Judged {
    case: &'static str,
    stack: &'static str,
    resource_type: &'static str,
    logical_id: &'static str,
    property: &'static str,
    declared: &'static str,
    values: &'static str,
    refused: bool,
}

fn judged_rows() -> Vec<Judged> {
    vec![
        // The reported case: a FHIR release version HealthLake does not host.
        Judged {
            case: "a FHIR release version the schema does not state",
            stack: "healthlake-datastore-r5",
            resource_type: HEALTHLAKE_FHIR_DATASTORE,
            logical_id: "Datastore",
            property: "DatastoreTypeVersion",
            declared: "R5",
            values: FHIR_VERSIONS,
            refused: true,
        },
        Judged {
            case: "a release version differing from the stated one only in case",
            stack: "healthlake-datastore-lowercase",
            resource_type: HEALTHLAKE_FHIR_DATASTORE,
            logical_id: "Datastore",
            property: "DatastoreTypeVersion",
            declared: "r4",
            values: FHIR_VERSIONS,
            refused: true,
        },
        Judged {
            case: "the one release version the schema states",
            stack: "healthlake-datastore-r4",
            resource_type: HEALTHLAKE_FHIR_DATASTORE,
            logical_id: "Datastore",
            property: "DatastoreTypeVersion",
            declared: "R4",
            values: FHIR_VERSIONS,
            refused: false,
        },
        // The reported case for the profile: a source format that service cannot convert.
        Judged {
            case: "a source format the schema does not state",
            stack: "healthlake-profile-xml",
            resource_type: HEALTHLAKE_DATA_TRANSFORMATION_PROFILE,
            logical_id: "Profile",
            property: "SourceFormat",
            declared: "XML",
            values: SOURCE_FORMATS,
            refused: true,
        },
        Judged {
            case: "a source format differing from a stated one only in case",
            stack: "healthlake-profile-lowercase",
            resource_type: HEALTHLAKE_DATA_TRANSFORMATION_PROFILE,
            logical_id: "Profile",
            property: "SourceFormat",
            declared: "csv",
            values: SOURCE_FORMATS,
            refused: true,
        },
        // Each value the set admits, including the one the report leading here did not cite.
        Judged {
            case: "the comma-separated-values format the schema states",
            stack: "healthlake-profile-csv",
            resource_type: HEALTHLAKE_DATA_TRANSFORMATION_PROFILE,
            logical_id: "Profile",
            property: "SourceFormat",
            declared: "CSV",
            values: SOURCE_FORMATS,
            refused: false,
        },
        Judged {
            case: "the clinical-document format the schema states",
            stack: "healthlake-profile-ccda",
            resource_type: HEALTHLAKE_DATA_TRANSFORMATION_PROFILE,
            logical_id: "Profile",
            property: "SourceFormat",
            declared: "CCDA",
            values: SOURCE_FORMATS,
            refused: false,
        },
    ]
}

/// The template one judged row submits: the declaration of its type, with the judged property
/// carrying the row's value.
fn judged_template(row: &Judged) -> String {
    let mut properties = match row.resource_type {
        HEALTHLAKE_FHIR_DATASTORE => json!({"DatastoreName": "healthlake-judged"}),
        _ => json!({
            "ProfileName": "healthlake-judged",
            "Source": profile_source(),
        }),
    };
    if let Value::Object(document) = &mut properties {
        document.insert(
            row.property.to_string(),
            Value::String(row.declared.to_string()),
        );
    }
    json!({
        "Resources": {row.logical_id: {"Type": row.resource_type, "Properties": properties}},
    })
    .to_string()
}

/// The validation RESULTS `DescribeEvents` reports for `stack`: one tuple per event carrying a
/// `ValidationName`, in the order the operation's events are reported.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "EventType").as_deref() == Some(VALIDATION_ERROR))
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "EventType").unwrap_or_default(),
                xml_text(event, "ValidationName").unwrap_or_default(),
                xml_text(event, "ValidationStatus").unwrap_or_default(),
                xml_text(event, "ValidationFailureMode").unwrap_or_default(),
                xml_text(event, "ValidationStatusReason").unwrap_or_default(),
                xml_text(event, "ValidationPath").unwrap_or_default(),
            )
        })
        .collect()
}

/// How many resources `ListStackResources` reports for `stack`, which is how a declaration
/// refused before provisioning is told from one that was deployed: the refused operation asks
/// nothing of any owning service, so the stack holds no resource at all.
async fn listed_resources(router: &MultiServiceRouter, case: &str, stack: &str) -> usize {
    let (code, body) = call(router, "ListStackResources", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack's resources must be readable, got {code} {body}"
    );
    xml_blocks(&body, "StackResourceSummaries")
        .first()
        .map(|summaries| xml_blocks(summaries, "member").len())
        .unwrap_or_default()
}

/// How many events `stack` recorded ABOUT its declared resource, which is how a declaration
/// refused before provisioning is told from one that was admitted into deployment: the refused
/// operation asks nothing of any owning service, so the resource reports no event at all,
/// while an admitted one is attempted and reports its own.
async fn resource_events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> usize {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "LogicalResourceId").as_deref() == Some(logical_id))
        .count()
}

/// Every judged row: a value outside the set its type's registry schema states fails the
/// stack at the pre-deployment stage — `CreateStack` is ACCEPTED, the stack rolls back, the
/// one validation result names the resource, the rejected value, the set and the property's
/// own path, and the stack holds no resource — while a value the schema states is admitted
/// into deployment instead of being refused there.
#[tokio::test]
async fn a_healthlake_value_outside_its_schemas_enum_fails_inside_the_stack() {
    for row in judged_rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();

        let (code, created) =
            create_stack(&router, row.stack, &judged_template(&row), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the template declares types \
             CloudFormation publishes — got {code} {created}"
        );
        let message = xml_text(&created, "Message").unwrap_or_default();
        assert!(
            !message.starts_with(UNRECOGNIZED_PREFIX),
            "case {case:?}: a published type must never be refused as unrecognized, got \
             {message:?}"
        );
        assert!(
            xml_text(&created, "StackId")
                .unwrap_or_default()
                .contains(":stack/"),
            "case {case:?}: expected CreateStack to answer a stack id, got {created}"
        );

        let settled = settled_stack_status(case, &router, REGION, row.stack).await;
        let results = validation_results(&router, case, row.stack).await;
        if row.refused {
            assert_eq!(
                settled,
                status::ROLLBACK_COMPLETE,
                "case {case:?}: expected the refused declaration to roll the stack back, got \
                 {settled:?}"
            );
            let expected = vec![(
                row.logical_id.to_string(),
                VALIDATION_ERROR.to_string(),
                PROPERTY_VALIDATION.to_string(),
                FAILED.to_string(),
                FAIL.to_string(),
                not_a_valid_enum_value(row.declared, row.values),
                format!(
                    "/Resources/{}/Properties/{}",
                    row.logical_id, row.property
                ),
            )];
            assert_eq!(
                results, expected,
                "case {case:?}: expected the validation results {expected:?}, got {results:?}"
            );
            let held = listed_resources(&router, case, row.stack).await;
            assert_eq!(
                held, 0,
                "case {case:?}: a refusal before provisioning must leave the stack holding no \
                 resource, got {held}"
            );
            let attempted = resource_events(&router, case, row.stack, row.logical_id).await;
            assert_eq!(
                attempted, 0,
                "case {case:?}: a refusal before provisioning must ask nothing of the owning \
                 service, so the resource must report no event, got {attempted}"
            );
            // The logical resource itself has no record, so a read of it is refused.
            let (code, body) = call(
                &router,
                "DescribeStackResource",
                &[
                    member("StackName", row.stack),
                    member("LogicalResourceId", row.logical_id),
                ],
            )
            .await;
            assert_eq!(
                code, 400,
                "case {case:?}: expected the resource read to be refused, got {code} {body}"
            );
            assert_eq!(
                xml_text(&body, "Message").unwrap_or_default(),
                format!(
                    "Resource {} does not exist for stack {}",
                    row.logical_id, row.stack
                ),
                "case {case:?}: expected the absent-resource refusal, got {body}"
            );
        } else {
            assert!(
                results.is_empty(),
                "case {case:?}: a value the schema states must record no validation result, \
                 got {results:?}"
            );
            // A value the schema states is ADMITTED into deployment, which is what the
            // resource's own events say: this suite mounts HealthLake nowhere, so the
            // declaration is then attempted and fails at the owning service rather than at
            // this stage — the stack rolls back carrying no validation result, and the
            // resource reports the events of an attempt. (The deployed path itself is the
            // router's cross-service suite.)
            let attempted = resource_events(&router, case, row.stack, row.logical_id).await;
            assert!(
                attempted > 0,
                "case {case:?}: a value the schema states must reach provisioning, so the \
                 resource must report its own events, got {attempted}"
            );
            assert_eq!(
                settled,
                status::ROLLBACK_COMPLETE,
                "case {case:?}: expected the admitted declaration to be attempted and fail at \
                 the owning service, which is mounted nowhere here, got {settled:?}"
            );
        }
    }
}

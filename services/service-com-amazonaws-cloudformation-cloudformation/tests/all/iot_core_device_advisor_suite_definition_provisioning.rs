// SPDX-License-Identifier: CC0-1.0

//! The AWS IoT Core Device Advisor test suite a stack declares, end to end: the type is one a
//! template may DECLARE, the declaration is deployed by calling the service that owns it, the
//! handles the stack publishes resolve there, a changed declaration reaches the suite in
//! place, a configuration that service refuses rolls the stack back with its own words, and
//! deleting the stack takes the suite away.
//!
//! RULE: a resource type AWS publishes is one a template may declare, and a declaration of it
//! is DEPLOYED by calling the service that owns the type — so `CreateStack` never answers
//! `Template format error: Unrecognized resource types`, the handle the stack publishes
//! (`Ref`) resolves in that service, every value the type publishes is the one that service
//! reported, and a value that service refuses fails the RESOURCE rather than the submission.
//!
//! The rows differ along what the rule generalises over: how many devices the configuration
//! names (none, one by certificate, two by thing and certificate), whether the suite is
//! intended for qualification (and so whether an empty root group is legal), whether the
//! declaration carries the type's own `Tags`, how many suites one stack declares, and the
//! template spelling (JSON with long-form intrinsics, YAML with the short-form tags).
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs`
//! builds — the CloudFormation calls over its awsQuery wire, the Device Advisor calls over
//! that service's own `restJson1` wire — so what is asserted is what a client that deploys the
//! template and then calls that service observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, published_attribute_names,
    IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
};

use crate::harness::{
    call, create_stack, failed_resource_reason, member, quoted_and_class, router,
    settled_stack_status, stack_event_reasons, REGION, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::{describe_stack, output_value};

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no row of this suite may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The role Device Advisor assumes to reach the devices under test, of the form that member
/// requires (`suiteDefinitionConfiguration.devicePermissionRoleArn` is an IAM ARN). No role has
/// to exist for the suite to be recorded: that service resolves the role when it runs the
/// suite, not when it stores the definition.
const DEVICE_PERMISSION_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-advisor";

/// The two forms a `DeviceUnderTest` names a device by that this suite declares: a thing and a
/// certificate of AWS IoT, both ARNs of that service (`DeviceUnderTest$thingArn`,
/// `$certificateArn`).
const THING_ARN: &str = "arn:aws:iot:us-east-1:000000000000:thing/advisor-thing";
const CERTIFICATE_ARN: &str = "arn:aws:iot:us-east-1:000000000000:cert/advisor-cert";

/// A root group document a non-qualification suite declares: the test groups the suite runs,
/// which `SuiteDefinitionConfiguration$rootGroup` carries as text (`@length 0..2048`).
const ROOT_GROUP: &str =
    r#"{"configuration":{},"tests":[{"name":"MQTT Connect","configuration":{}}]}"#;

/// A second, different one, for the row that changes a suite's declaration in place.
const CHANGED_ROOT_GROUP: &str =
    r#"{"configuration":{},"tests":[{"name":"MQTT Publish","configuration":{}}]}"#;

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// The error code that service answers for a suite definition it does not hold, and the status
/// its model binds to that shape (`ResourceNotFoundException`, `@httpError 404`).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The fixed text that service words a not-found with, which the id the request named is
/// appended to (`Unable to find resource with suiteDefinitionId: <id>`).
const NOT_FOUND_PREFIX: &str = "Unable to find resource with suiteDefinitionId: ";

/// One Device Advisor call over the `restJson1` wire its model declares, on the method and URI
/// that operation's own `@http` binding names, signed for `iotdeviceadvisor` — that model's own
/// `sigv4Name` — in the Region the stack's resources live in.
async fn device_advisor(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://iotdeviceadvisor.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("iotdeviceadvisor.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iotdeviceadvisor/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed AWS IoT Core Device Advisor request");
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

/// That service's own read of ONE suite definition, by the id it minted.
async fn get_suite_definition(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    device_advisor(router, "GET", &format!("/suiteDefinitions/{id}"), None).await
}

/// The tags that service holds on one resource, by the ARN its tagging calls address it by.
async fn resource_tags(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, answered) =
        device_advisor(router, "GET", &format!("/tags/{}", encoded(arn)), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the tags of {arn:?}, got {status} with {answered}"
    );
    answered.get("tags").cloned().unwrap_or(Value::Null)
}

/// Every suite definition id that service lists for the account.
async fn listed_suite_definition_ids(router: &MultiServiceRouter) -> Vec<String> {
    let (_, page) = device_advisor(router, "GET", "/suiteDefinitions", None).await;
    page.get("suiteDefinitionInformationList")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("suiteDefinitionId").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// An ARN as ONE path segment of a URI, so its own `:` and `/` address the resource rather
/// than another path: everything outside RFC 3986's unreserved set (`A-Z a-z 0-9 - _ . ~`)
/// escaped, by the same maintained encoder the emulator's own in-process calls use
/// (`state::provisioning::percent_encoded`) rather than a copy written here.
fn encoded(arn: &str) -> String {
    const UNRESERVED: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');
    percent_encoding::utf8_percent_encode(arn, UNRESERVED).to_string()
}

/// One declared suite a row expects.
struct DeclaredSuite {
    /// The output key whose value is the suite's `Ref` — the id that service minted.
    ref_output: &'static str,
    /// The output keys carrying its `SuiteDefinitionArn` and `SuiteDefinitionVersion`, where
    /// the row reads them back.
    arn_output: Option<&'static str>,
    version_output: Option<&'static str>,
    name: &'static str,
    root_group: &'static str,
    intended_for_qualification: bool,
    /// How many devices the declaration names, which is what the stored configuration must
    /// report back.
    device_count: usize,
    /// The tags its own declaration asks for, beside the stack's.
    declared_tags: &'static [(&'static str, &'static str)],
}

/// One row: a template body, and the suites it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    suites: Vec<DeclaredSuite>,
}

fn rows() -> Vec<Row> {
    vec![
        // One suite naming no device at all, with every published return value read back
        // through `Outputs`.
        Row {
            case: "one suite, no devices, every return value read back",
            stack_name: "device-advisor-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Suite": {
                        "Type": IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
                        "Properties": {
                            "SuiteDefinitionConfiguration": {
                                "SuiteDefinitionName": "Connectivity",
                                "DevicePermissionRoleArn": DEVICE_PERMISSION_ROLE_ARN,
                                "RootGroup": ROOT_GROUP
                            }
                        }
                    }
                },
                "Outputs": {
                    "SuiteRef": {"Value": {"Ref": "Suite"}},
                    "SuiteId": {
                        "Value": {"Fn::GetAtt": ["Suite", "SuiteDefinitionId"]}
                    },
                    "SuiteArn": {
                        "Value": {"Fn::GetAtt": ["Suite", "SuiteDefinitionArn"]}
                    },
                    "SuiteVersion": {
                        "Value": {"Fn::GetAtt": ["Suite", "SuiteDefinitionVersion"]}
                    }
                }
            })
            .to_string(),
            suites: vec![DeclaredSuite {
                ref_output: "SuiteRef",
                arn_output: Some("SuiteArn"),
                version_output: Some("SuiteVersion"),
                name: "Connectivity",
                root_group: ROOT_GROUP,
                intended_for_qualification: false,
                device_count: 0,
                declared_tags: &[],
            }],
        },
        // A qualification suite declaring an EMPTY root group — legal exactly because
        // `intendedForQualification` is true — beside one device named by its certificate and
        // the type's own `Tags`.
        Row {
            case: "a qualification suite with an empty root group, one device and Tags",
            stack_name: "device-advisor-qualification",
            template: json!({
                "Resources": {
                    "Suite": {
                        "Type": IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
                        "Properties": {
                            "SuiteDefinitionConfiguration": {
                                "SuiteDefinitionName": "Qualification",
                                "DevicePermissionRoleArn": DEVICE_PERMISSION_ROLE_ARN,
                                "IntendedForQualification": true,
                                "RootGroup": "",
                                "Devices": [{"CertificateArn": CERTIFICATE_ARN}]
                            },
                            "Tags": [{"Key": "team", "Value": "iot"}]
                        }
                    }
                },
                "Outputs": {
                    "SuiteRef": {"Value": {"Ref": "Suite"}}
                }
            })
            .to_string(),
            suites: vec![DeclaredSuite {
                ref_output: "SuiteRef",
                arn_output: None,
                version_output: None,
                name: "Qualification",
                // An empty root group asks for the default qualification group, so what the
                // stored configuration reports is NOT the empty string the template stated:
                // the row checks the name, the devices and the flag, and leaves the resolved
                // group to the `rootGroup` assertion below, which skips an empty declaration.
                root_group: "",
                intended_for_qualification: true,
                device_count: 1,
                declared_tags: &[("team", "iot")],
            }],
        },
        // The YAML spelling with the short-form intrinsics, and TWO devices — the upper bound
        // `DeviceUnderTestList` admits (`@length 0..2`) — named one by thing and one by
        // certificate.
        Row {
            case: "the YAML spelling, two devices at the list's upper bound",
            stack_name: "device-advisor-yaml",
            template: format!(
                "\
Resources:
  Suite:
    Type: {IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION}
    Properties:
      SuiteDefinitionConfiguration:
        SuiteDefinitionName: Yaml Suite
        DevicePermissionRoleArn: {DEVICE_PERMISSION_ROLE_ARN}
        IntendedForQualification: false
        RootGroup: '{ROOT_GROUP}'
        Devices:
          - ThingArn: {THING_ARN}
          - CertificateArn: {CERTIFICATE_ARN}
Outputs:
  SuiteRef:
    Value: !Ref Suite
  SuiteArn:
    Value: !GetAtt Suite.SuiteDefinitionArn
"
            ),
            suites: vec![DeclaredSuite {
                ref_output: "SuiteRef",
                arn_output: Some("SuiteArn"),
                version_output: None,
                name: "Yaml Suite",
                root_group: ROOT_GROUP,
                intended_for_qualification: false,
                device_count: 2,
                declared_tags: &[],
            }],
        },
        // Two suites in one stack: each publishes its OWN id and is its own record in the
        // owning service.
        Row {
            case: "two suites in one stack, each its own record",
            stack_name: "device-advisor-pair",
            template: json!({
                "Resources": {
                    "First": {
                        "Type": IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
                        "Properties": {
                            "SuiteDefinitionConfiguration": {
                                "SuiteDefinitionName": "Pair First",
                                "DevicePermissionRoleArn": DEVICE_PERMISSION_ROLE_ARN,
                                "RootGroup": ROOT_GROUP,
                                "Devices": [{"ThingArn": THING_ARN}]
                            }
                        }
                    },
                    "Second": {
                        "Type": IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
                        "Properties": {
                            "SuiteDefinitionConfiguration": {
                                "SuiteDefinitionName": "Pair Second",
                                "DevicePermissionRoleArn": DEVICE_PERMISSION_ROLE_ARN,
                                "RootGroup": CHANGED_ROOT_GROUP
                            }
                        }
                    }
                },
                "Outputs": {
                    "FirstRef": {"Value": {"Ref": "First"}},
                    "SecondRef": {"Value": {"Ref": "Second"}}
                }
            })
            .to_string(),
            suites: vec![
                DeclaredSuite {
                    ref_output: "FirstRef",
                    arn_output: None,
                    version_output: None,
                    name: "Pair First",
                    root_group: ROOT_GROUP,
                    intended_for_qualification: false,
                    device_count: 1,
                    declared_tags: &[],
                },
                DeclaredSuite {
                    ref_output: "SecondRef",
                    arn_output: None,
                    version_output: None,
                    name: "Pair Second",
                    root_group: CHANGED_ROOT_GROUP,
                    intended_for_qualification: false,
                    device_count: 0,
                    declared_tags: &[],
                },
            ],
        },
    ]
}

/// The stack-level tag every stack of this suite carries, which CloudFormation propagates to
/// the resources it creates.
fn stack_tag(stack_name: &str) -> Vec<(String, String)> {
    vec![
        member("Tags.member.1.Key", STACK_TAG_KEY),
        member("Tags.member.1.Value", stack_name),
    ]
}

/// Submit `template` as `stack_name` under the stack's own tag: a type AWS publishes is
/// ADMITTED, so this never sees the template-format refusal.
async fn submitted(router: &MultiServiceRouter, case: &str, stack_name: &str, template: &str) {
    let (status, body) = create_stack(router, stack_name, template, &stack_tag(stack_name)).await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected {IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION} to be a type a \
         template may declare, got {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — \
         {IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION} is a type a template may declare — got \
         {status} with body {body}"
    );
}

/// Deploy `template` as `stack_name` and answer the `DescribeStacks` body of the COMPLETED
/// stack, quoting the stack's own failure reasons when the create did not complete.
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

/// One output of a settled stack, or an explanation naming the body it is missing from.
fn published(case: &str, described: &str, key: &str) -> String {
    output_value(described, key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
}

/// Whether `id` is spelled as the `UUID` that service draws for a suite definition: the
/// canonical `8-4-4-4-12` hexadecimal form, which is inside the member's own `@length 12..36`.
fn is_minted_uuid(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit()))
}

/// THE RULE, the positive direction: every suite the stack reports as created is a record that
/// service holds, under the id the stack published, configured as the template declared — and
/// every value the type publishes is the one that service reported.
#[tokio::test]
async fn a_deployed_stack_publishes_the_suite_the_owning_service_created() {
    for row in rows() {
        let router = router();
        let case = row.case;
        let described = deployed(&router, case, row.stack_name, &row.template).await;

        for declared in &row.suites {
            let id = published(case, &described, declared.ref_output);
            assert!(
                is_minted_uuid(&id),
                "case {case:?}: expected the published handle to be a suite definition id that \
                 service draws, got {id:?}"
            );

            let (status, reported) = get_suite_definition(&router, &id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the owning service to report the suite the stack \
                 created under {id:?}, got {status} with {reported}"
            );
            assert_eq!(
                reported.get("suiteDefinitionId").and_then(Value::as_str),
                Some(id.as_str()),
                "case {case:?}: expected the suite to carry the published id, got {reported}"
            );
            assert_eq!(
                reported
                    .pointer("/suiteDefinitionConfiguration/suiteDefinitionName")
                    .and_then(Value::as_str),
                Some(declared.name),
                "case {case:?}: expected the declared suite name, got {reported}"
            );
            assert_eq!(
                reported
                    .pointer("/suiteDefinitionConfiguration/devicePermissionRoleArn")
                    .and_then(Value::as_str),
                Some(DEVICE_PERMISSION_ROLE_ARN),
                "case {case:?}: expected the declared device permission role, got {reported}"
            );
            assert_eq!(
                reported
                    .pointer("/suiteDefinitionConfiguration/intendedForQualification")
                    .and_then(Value::as_bool),
                Some(declared.intended_for_qualification),
                "case {case:?}: expected the declared qualification flag, got {reported}"
            );
            if !declared.root_group.is_empty() {
                assert_eq!(
                    reported
                        .pointer("/suiteDefinitionConfiguration/rootGroup")
                        .and_then(Value::as_str),
                    Some(declared.root_group),
                    "case {case:?}: expected the declared root group, got {reported}"
                );
            }
            let devices = reported
                .pointer("/suiteDefinitionConfiguration/devices")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or_default();
            assert_eq!(
                devices, declared.device_count,
                "case {case:?}: expected {} declared devices, got {reported}",
                declared.device_count
            );

            // ...and the values the type publishes are that service's own rather than
            // stand-ins: each is the one its own read reports for this suite.
            let reported_arn = reported
                .get("suiteDefinitionArn")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(key) = declared.arn_output {
                assert_eq!(
                    published(case, &described, key),
                    reported_arn,
                    "case {case:?}: expected the published SuiteDefinitionArn to be the ARN \
                     that service reports for the suite, got {reported}"
                );
            }
            if let Some(key) = declared.version_output {
                assert_eq!(
                    published(case, &described, key).as_str(),
                    reported
                        .get("suiteDefinitionVersion")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    "case {case:?}: expected the published SuiteDefinitionVersion to be the \
                     label that service reports for the suite, got {reported}"
                );
            }
            if row.stack_name == "device-advisor-round-trip" {
                assert_eq!(
                    published(case, &described, "SuiteId"),
                    id,
                    "case {case:?}: expected the published SuiteDefinitionId to be the id the \
                     Ref resolves to"
                );
            }

            // The tags the suite carries are the declaration's own beside the stack's.
            let held = resource_tags(&router, case, reported_arn).await;
            assert_eq!(
                held.get(STACK_TAG_KEY).and_then(Value::as_str),
                Some(row.stack_name),
                "case {case:?}: expected the stack's own tag on the suite, got {held}"
            );
            for (key, value) in declared.declared_tags {
                assert_eq!(
                    held.get(*key).and_then(Value::as_str),
                    Some(*value),
                    "case {case:?}: expected the declared tag {key:?}, got {held}"
                );
            }
        }
    }
}

/// A changed declaration reaches the suite IN PLACE: the id the stack publishes does not move,
/// and the owning service reports what the new template declares under a later version label.
#[tokio::test]
async fn a_changed_declaration_reaches_the_suite_in_place() {
    let router = router();
    let case = "a renamed suite with a replaced root group and a dropped device";
    let stack_name = "device-advisor-updated";
    let before = json!({
        "Resources": {
            "Suite": {
                "Type": IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
                "Properties": {
                    "SuiteDefinitionConfiguration": {
                        "SuiteDefinitionName": "Before Suite",
                        "DevicePermissionRoleArn": DEVICE_PERMISSION_ROLE_ARN,
                        "RootGroup": ROOT_GROUP,
                        "Devices": [{"ThingArn": THING_ARN}]
                    },
                    "Tags": [{"Key": "stage", "Value": "before"}]
                }
            }
        },
        "Outputs": {
            "SuiteRef": {"Value": {"Ref": "Suite"}},
            "SuiteVersion": {"Value": {"Fn::GetAtt": ["Suite", "SuiteDefinitionVersion"]}}
        }
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &before).await;
    let id = published(case, &described, "SuiteRef");
    let first_version = published(case, &described, "SuiteVersion");

    let after = json!({
        "Resources": {
            "Suite": {
                "Type": IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
                "Properties": {
                    "SuiteDefinitionConfiguration": {
                        "SuiteDefinitionName": "After Suite",
                        "DevicePermissionRoleArn": DEVICE_PERMISSION_ROLE_ARN,
                        "RootGroup": CHANGED_ROOT_GROUP,
                        "Devices": []
                    },
                    "Tags": [{"Key": "stage", "Value": "after"}]
                }
            }
        },
        "Outputs": {
            "SuiteRef": {"Value": {"Ref": "Suite"}},
            "SuiteVersion": {"Value": {"Fn::GetAtt": ["Suite", "SuiteDefinitionVersion"]}}
        }
    })
    .to_string();

    let (status, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &after),
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {body}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );
    let described = describe_stack(&router, stack_name).await.1;

    // The handle did not move: the suite was changed, not replaced.
    assert_eq!(
        published(case, &described, "SuiteRef"),
        id,
        "case {case:?}: expected the same suite definition id after the update"
    );

    let (status, reported) = get_suite_definition(&router, &id).await;
    assert_eq!(
        (
            status,
            reported
                .pointer("/suiteDefinitionConfiguration/suiteDefinitionName")
                .and_then(Value::as_str),
            reported
                .pointer("/suiteDefinitionConfiguration/rootGroup")
                .and_then(Value::as_str),
        ),
        (200, Some("After Suite"), Some(CHANGED_ROOT_GROUP)),
        "case {case:?}: expected the changed declaration on the suite, got {reported}"
    );
    assert_eq!(
        reported
            .pointer("/suiteDefinitionConfiguration/devices")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "case {case:?}: expected the dropped device, got {reported}"
    );

    // The version the type publishes is the label that service allocated for the change, not
    // the one the first deployment published.
    let published_version = published(case, &described, "SuiteVersion");
    assert_eq!(
        Some(published_version.as_str()),
        reported
            .get("suiteDefinitionVersion")
            .and_then(Value::as_str),
        "case {case:?}: expected the published version to be the label that service reports, \
         got {reported}"
    );
    assert_ne!(
        published_version, first_version,
        "case {case:?}: expected the change to publish a later version label than {first_version:?}"
    );

    let arn = reported
        .get("suiteDefinitionArn")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let held = resource_tags(&router, case, arn).await;
    assert_eq!(
        held.get("stage").and_then(Value::as_str),
        Some("after"),
        "case {case:?}: expected the re-valued declared tag, got {held}"
    );
}

/// Deleting the stack takes the suite away from the owning service, which is what makes a
/// stack's resources the stack's own rather than leftovers — and that service then answers its
/// own not-found for the id the stack published.
#[tokio::test]
async fn deleting_the_stack_takes_the_suite_away() {
    let router = router();
    let case = "a suite removed with the stack";
    let stack_name = "device-advisor-deleted";
    let template = json!({
        "Resources": {
            "Suite": {
                "Type": IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
                "Properties": {
                    "SuiteDefinitionConfiguration": {
                        "SuiteDefinitionName": "Deleted Suite",
                        "DevicePermissionRoleArn": DEVICE_PERMISSION_ROLE_ARN,
                        "RootGroup": ROOT_GROUP
                    }
                }
            }
        },
        "Outputs": {
            "SuiteRef": {"Value": {"Ref": "Suite"}}
        }
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &template).await;
    let id = published(case, &described, "SuiteRef");
    assert_eq!(
        get_suite_definition(&router, &id).await.0,
        200,
        "case {case:?}: expected the suite {id:?} to exist before the delete"
    );

    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );

    // Wait for the CONDITION — the record released — under the harness's own generous
    // deadline, so a loaded host cannot decide the outcome.
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let status = get_suite_definition(&router, &id).await.0;
        if status != 200 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the suite to be released, that service still answers \
             {status}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }

    let (status, answered) = get_suite_definition(&router, &id).await;
    assert_eq!(
        status, NOT_FOUND_STATUS,
        "case {case:?}: expected the suite to be gone with the stack, got {status} with \
         {answered}"
    );
    let code = answered
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        code.ends_with(RESOURCE_NOT_FOUND),
        "case {case:?}: expected {RESOURCE_NOT_FOUND} for the removed suite, got {answered}"
    );
    assert_eq!(
        answered.get("message").and_then(Value::as_str),
        Some(format!("{NOT_FOUND_PREFIX}{id}").as_str()),
        "case {case:?}: expected the not-found to name the id the request carried, got \
         {answered}"
    );
    assert!(
        listed_suite_definition_ids(&router).await.is_empty(),
        "case {case:?}: expected no suite the stack created left in the account, got {:?}",
        listed_suite_definition_ids(&router).await
    );
}

/// One refusal row: a configuration the owning service refuses, and the member its refusal
/// names.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    configuration: Value,
    /// Text the owning service's own refusal must carry, which is what says the diagnostic is
    /// that service's rather than a template-format error.
    names: &'static str,
}

/// The rows differ along WHICH rule of `SuiteDefinitionConfiguration` the declaration breaks —
/// the prose rule on `rootGroup` ("If `intendedForQualification` is false, `rootGroup` cannot
/// be an empty string"), the ARN form `devicePermissionRoleArn` requires, and the requirement
/// that a `DeviceUnderTest` name a device at all ("A thing ARN, certificate ARN or device role
/// ARN is required") — so the refusal is the owning service's whatever the fault is.
fn refused_configurations() -> Vec<Refused> {
    vec![
        Refused {
            case: "an empty root group on a non-qualification suite",
            stack_name: "device-advisor-empty-root-group",
            configuration: json!({
                "SuiteDefinitionName": "Refused",
                "DevicePermissionRoleArn": DEVICE_PERMISSION_ROLE_ARN,
                "RootGroup": ""
            }),
            names: "rootGroup",
        },
        Refused {
            case: "a device permission role that is no IAM ARN",
            stack_name: "device-advisor-bad-role",
            configuration: json!({
                "SuiteDefinitionName": "Refused",
                "DevicePermissionRoleArn": "arn:aws:s3:::advisor-not-a-role",
                "RootGroup": ROOT_GROUP
            }),
            names: "devicePermissionRoleArn",
        },
        Refused {
            case: "a device under test naming no device",
            stack_name: "device-advisor-nameless-device",
            configuration: json!({
                "SuiteDefinitionName": "Refused",
                "DevicePermissionRoleArn": DEVICE_PERMISSION_ROLE_ARN,
                "RootGroup": ROOT_GROUP,
                "Devices": [{}]
            }),
            names: "devices[0]",
        },
    ]
}

/// THE RULE, the refusal direction: a declaration the owning service refuses fails the
/// RESOURCE and rolls the stack back with that service's own words — it is never a
/// template-format error, and nothing is left behind in that service.
#[tokio::test]
async fn a_configuration_the_owning_service_refuses_rolls_the_stack_back() {
    for row in refused_configurations() {
        let router = router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "Suite": {
                    "Type": IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION,
                    "Properties": {"SuiteDefinitionConfiguration": row.configuration}
                }
            }
        })
        .to_string();

        // The body is ADMITTED: the type is one a template may declare, so the fault is the
        // declared VALUE and not the resource type.
        submitted(&router, case, row.stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, row.stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the refused create to roll back, got {reached} with \
             reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack_name).await
        );

        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack_name,
            "Suite",
            "CREATE_FAILED",
        )
        .await;
        let (quoted, _) = quoted_and_class(case, &reason);
        assert!(
            !quoted.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected the owning service's refusal, got {reason:?}"
        );
        assert!(
            quoted.contains(row.names),
            "case {case:?}: expected the refusal to name {:?}, got {reason:?}",
            row.names
        );

        // Nothing was created: the account holds no suite the stack could have made.
        let listed = listed_suite_definition_ids(&router).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected the refused create to leave no suite, got {listed:?}"
        );
    }
}

/// The catalogue entries the rule rests on: the type is published and recognised, a near-miss
/// spelling of it is not, it is identified by the id its owning service mints, and it reports
/// the return values its own section publishes.
#[test]
fn the_catalogue_publishes_the_type_and_what_it_returns() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION),
        "expected {IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION:?} in the published catalogue, it \
         is absent"
    );
    assert!(
        is_published(IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION)
            && is_recognized(IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION),
        "expected {IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION:?} to be a type a template may \
         declare, it is not"
    );
    for near_miss in [
        "AWS::IoTDeviceAdvisor::SuiteDefinition",
        "AWS::IoTCoreDeviceAdvisor::SuiteDefinitions",
        "AWS::IoTCoreDeviceAdvisor::Suite",
    ] {
        assert!(
            !is_recognized(near_miss),
            "expected {near_miss:?} to be unrecognized, it is recognized"
        );
    }
    assert!(
        identity_is_service_assigned(IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION),
        "expected {IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION:?} to take the identity its owning \
         service assigns, it does not"
    );
    assert_eq!(
        published_attribute_names(IOT_CORE_DEVICE_ADVISOR_SUITE_DEFINITION),
        &[
            "SuiteDefinitionArn",
            "SuiteDefinitionId",
            "SuiteDefinitionVersion"
        ],
        "expected the three return values the type's own section publishes"
    );
}

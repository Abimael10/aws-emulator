// SPDX-License-Identifier: CC0-1.0

//! What a refused `ScanSbom` call CLASSIFIES the failure as, end-to-end over
//! HTTP.
//!
//! RULE: a refusal is answered through the operation's own modeled error shape
//! with every member the model marks required on the wire — so this operation's
//! `ValidationException` (HTTP 400) carries, beside `message`, the required
//! `reason` naming the class of validation failure from the model's own
//! `ValidationExceptionReason` enum, and the optional `fields` naming the
//! member that failed whenever the failure is a member's value.
//!
//! The classes, and the reason each states:
//!
//! * nothing the request carries reads as a BOM document at all ->
//!   `CANNOT_PARSE`, with no field named: no member of a document was read;
//! * the document STATES a BOM format or revision this operation does not scan
//!   -> `UNSUPPORTED_SBOM_TYPE`, naming the member that states it;
//! * a member of the document is absent where the specification requires it,
//!   carries a value it cannot hold, or holds more entries than it may ->
//!   `FIELD_VALIDATION_FAILED`, naming that member.
//!
//! The rows differ along what the rule generalises over: the class of defect,
//! the JSON type the request sent, the member that failed (the document's own
//! and a component's), the nesting level it failed at, and — for the component
//! ceiling — the count. The enum values are checked against the model-derived
//! `ops::types::VALIDATION_EXCEPTION_REASON_VALUES`, so a reason this service
//! states cannot drift from the type the model declares.
//!
//! Shares the HTTP helpers of `scan_sbom.rs` through the `super` path the test
//! crate root gives it.

use super::scan_sbom::{bom, component, emulator, post_scan};
use serde_json::{json, Value};
use service_com_amazonaws_inspectorscan_inspectorscan::ops::types::VALIDATION_EXCEPTION_REASON_VALUES;

/// The reason a request carrying no readable BOM document is refused with.
const CANNOT_PARSE: &str = "CANNOT_PARSE";

/// The reason a member-level defect of the submitted document is refused with.
const FIELD_VALIDATION_FAILED: &str = "FIELD_VALIDATION_FAILED";

/// The reason a document stating a BOM format or revision no scan reads is
/// refused with.
const UNSUPPORTED_SBOM_TYPE: &str = "UNSUPPORTED_SBOM_TYPE";

/// The component ceiling the handler enforces, as `scan_sbom_refusals.rs`
/// states it: one past it is refused for the `components` member.
const COMPONENT_LIMIT: usize = 5000;

/// One row: a `sbom` value, the class the refusal must state for it, and the
/// member its `fields` must name (`None` when no member was read).
#[derive(Debug)]
struct Case {
    what: &'static str,
    sbom: Value,
    reason: &'static str,
    field: Option<&'static str>,
}

/// A component whose every member is readable, so a row's defect is the only
/// one in the document.
fn clean_component() -> Value {
    component("ref-clean", "left-pad", "1.0.0", "pkg:npm/left-pad@1.0.0")
}

/// A JSON document nested `levels` arrays deep: more than this operation can
/// take in, so nothing of a BOM document is read out of it.
fn deeply_nested(levels: usize) -> Value {
    let mut value = json!({ "bomFormat": "CycloneDX", "specVersion": "1.5" });
    for _ in 0..levels {
        value = Value::Array(vec![value]);
    }
    value
}

/// `count` distinct clean components.
fn many_components(count: usize) -> Vec<Value> {
    (0..count)
        .map(|i| {
            component(
                &format!("ref-{i}"),
                "left-pad",
                "1.0.0",
                "pkg:npm/left-pad@1.0.0",
            )
        })
        .collect()
}

fn cases() -> Vec<Case> {
    vec![
        // Nothing a BOM document can be read out of: whatever JSON type was
        // sent instead, and JSON too deep to be taken in.
        Case {
            what: "a string document",
            sbom: json!("this-is-not-a-cyclonedx-sbom-just-a-string"),
            reason: CANNOT_PARSE,
            field: None,
        },
        Case {
            what: "an array document",
            sbom: json!([1, 2, 3]),
            reason: CANNOT_PARSE,
            field: None,
        },
        Case {
            what: "a number document",
            sbom: json!(7),
            reason: CANNOT_PARSE,
            field: None,
        },
        Case {
            what: "a boolean document",
            sbom: json!(true),
            reason: CANNOT_PARSE,
            field: None,
        },
        Case {
            what: "a document nested deeper than it can be parsed",
            sbom: deeply_nested(96),
            reason: CANNOT_PARSE,
            field: None,
        },
        // A document STATING a BOM type this operation does not scan.
        Case {
            what: "a bomFormat naming another SBOM standard",
            sbom: json!({ "bomFormat": "SPDX", "specVersion": "1.5", "components": [] }),
            reason: UNSUPPORTED_SBOM_TYPE,
            field: Some("bomFormat"),
        },
        Case {
            what: "a bomFormat in the wrong case",
            sbom: json!({ "bomFormat": "cyclonedx", "specVersion": "1.5", "components": [] }),
            reason: UNSUPPORTED_SBOM_TYPE,
            field: Some("bomFormat"),
        },
        Case {
            what: "an earlier CycloneDX revision",
            sbom: json!({ "bomFormat": "CycloneDX", "specVersion": "1.4", "components": [] }),
            reason: UNSUPPORTED_SBOM_TYPE,
            field: Some("specVersion"),
        },
        Case {
            what: "a later CycloneDX revision",
            sbom: json!({ "bomFormat": "CycloneDX", "specVersion": "1.6", "components": [] }),
            reason: UNSUPPORTED_SBOM_TYPE,
            field: Some("specVersion"),
        },
        // A member of the document that is absent where it is required.
        Case {
            what: "an empty document, stating no bomFormat",
            sbom: json!({}),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("bomFormat"),
        },
        Case {
            what: "a document stating no specVersion",
            sbom: json!({ "bomFormat": "CycloneDX", "components": [] }),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("specVersion"),
        },
        // A member carrying a value it cannot hold: the document's own members
        // and a component's, at the top level and one level down.
        Case {
            what: "a bomFormat sent as a number",
            sbom: json!({ "bomFormat": 1, "specVersion": "1.5", "components": [] }),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("bomFormat"),
        },
        Case {
            what: "a specVersion sent as a number",
            sbom: json!({ "bomFormat": "CycloneDX", "specVersion": 1.5, "components": [] }),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("specVersion"),
        },
        Case {
            what: "components sent as an object",
            sbom: json!({ "bomFormat": "CycloneDX", "specVersion": "1.5", "components": {} }),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("components"),
        },
        Case {
            what: "a component entry that is not an object",
            sbom: bom(vec![json!("left-pad@1.0.0")]),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("components"),
        },
        Case {
            what: "a nested components member that is not an array",
            sbom: bom(vec![json!({
                "type": "application",
                "bom-ref": "ref-app",
                "name": "app",
                "version": "1.0.0",
                "components": { "name": "left-pad" },
            })]),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("components"),
        },
        Case {
            what: "a component stating no type",
            sbom: bom(vec![json!({ "name": "left-pad", "version": "1.0.0" })]),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("type"),
        },
        Case {
            what: "a component whose type is not a CycloneDX component type",
            sbom: bom(vec![json!({
                "type": "package",
                "name": "left-pad",
                "version": "1.0.0",
            })]),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("type"),
        },
        Case {
            what: "a component whose name is empty",
            sbom: bom(vec![json!({ "type": "library", "name": "", "version": "1.0.0" })]),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("name"),
        },
        Case {
            what: "a purl that is not a package URL",
            sbom: bom(vec![json!({
                "type": "library",
                "name": "left-pad",
                "version": "1.0.0",
                "purl": "left-pad@1.0.0",
            })]),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("purl"),
        },
        Case {
            what: "two components sharing one bom-ref",
            sbom: bom(vec![
                component("same", "left-pad", "1.0.0", "pkg:npm/left-pad@1.0.0"),
                component("same", "minimist", "1.2.5", "pkg:npm/minimist@1.2.5"),
            ]),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("bom-ref"),
        },
        Case {
            what: "a metadata member sent as a string",
            sbom: {
                let mut sbom = bom(vec![clean_component()]);
                sbom["metadata"] = json!("generated by acme");
                sbom
            },
            reason: FIELD_VALIDATION_FAILED,
            field: Some("metadata"),
        },
        Case {
            what: "a serialNumber sent as a number",
            sbom: {
                let mut sbom = bom(vec![clean_component()]);
                sbom["serialNumber"] = json!(42);
                sbom
            },
            reason: FIELD_VALIDATION_FAILED,
            field: Some("serialNumber"),
        },
        // More entries than the `components` member may hold: the ceiling is a
        // bound on that member, so the member it names is `components`.
        Case {
            what: "one component more than a scan accepts",
            sbom: bom(many_components(COMPONENT_LIMIT + 1)),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("components"),
        },
        Case {
            what: "an assembly whose tree is one component over the ceiling",
            sbom: bom(vec![json!({
                "type": "application",
                "bom-ref": "ref-assembly",
                "name": "assembly",
                "version": "1.0.0",
                "components": many_components(COMPONENT_LIMIT),
            })]),
            reason: FIELD_VALIDATION_FAILED,
            field: Some("components"),
        },
    ]
}

#[tokio::test]
async fn every_refusal_states_the_modeled_reason_for_its_class_of_defect() {
    let (_engine, router) = emulator();
    for case in cases() {
        let (status, response) = post_scan(&router, &json!({ "sbom": case.sbom })).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status}: {response}",
            case.what
        );
        assert_eq!(
            response["__type"].as_str(),
            Some("ValidationException"),
            "case {:?}: expected the ValidationException error code, got {response}",
            case.what
        );
        assert_eq!(
            response["reason"].as_str(),
            Some(case.reason),
            "case {:?}: expected reason {:?}, got {response}",
            case.what,
            case.reason
        );
        match case.field {
            None => assert!(
                response.get("fields").is_none(),
                "case {:?}: no member of a document was read, so the refusal names no \
                 field, got {response}",
                case.what
            ),
            Some(member) => {
                let fields = response["fields"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let named: Vec<&str> = fields
                    .iter()
                    .filter_map(|field| field["name"].as_str())
                    .collect();
                assert_eq!(
                    named,
                    vec![member],
                    "case {:?}: expected the refusal to name the field {member:?}, got \
                     {response}",
                    case.what
                );
                // Every entry carries the two members the model requires of
                // it, and its sentence is the condition the refusal reports.
                let message = response["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                for field in &fields {
                    let stated = field["message"].as_str().unwrap_or_default();
                    assert!(
                        !stated.is_empty() && message.ends_with(stated),
                        "case {:?}: expected the field's message to state the condition \
                         the refusal reports ({message:?}), got {response}",
                        case.what
                    );
                }
            }
        }
    }
}

/// The reason a refusal states is a value of the type the model declares for
/// that member: a client switching on `ValidationExceptionReason` cannot be
/// handed a value outside it.
#[tokio::test]
async fn every_reason_stated_is_a_value_the_model_declares() {
    let (_engine, router) = emulator();
    for case in cases() {
        let (_status, response) = post_scan(&router, &json!({ "sbom": case.sbom })).await;
        let stated = response["reason"].as_str().unwrap_or_default();
        assert!(
            VALIDATION_EXCEPTION_REASON_VALUES.contains(&stated),
            "case {:?}: expected a declared ValidationExceptionReason value, got \
             {stated:?} in {response}",
            case.what
        );
    }
}

/// The three reasons this operation can state are themselves values of the
/// declared type, so the table above cannot pin a value the model does not
/// have.
#[test]
fn the_reasons_this_operation_states_are_declared_values() {
    for reason in [CANNOT_PARSE, FIELD_VALIDATION_FAILED, UNSUPPORTED_SBOM_TYPE] {
        assert!(
            VALIDATION_EXCEPTION_REASON_VALUES.contains(&reason),
            "expected {reason:?} to be one of the model's declared values \
             {VALIDATION_EXCEPTION_REASON_VALUES:?}"
        );
    }
}

/// A document that IS a CycloneDX 1.5 BOM is still scanned: the classified
/// refusal above is reached for defective input only.
#[tokio::test]
async fn a_scanned_document_carries_no_reason() {
    let (_engine, router) = emulator();
    let request = json!({ "sbom": bom(vec![clean_component()]) });
    let (status, response) = post_scan(&router, &request).await;
    assert_eq!(
        status, 200,
        "a minimal valid CycloneDX 1.5 document is scanned, got {status}: {response}"
    );
    assert!(
        response.get("reason").is_none(),
        "a scanned document carries no refusal classification, got {response}"
    );
}

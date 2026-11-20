// SPDX-License-Identifier: CC0-1.0

//! The wire KEY a refusal of this service carries its explanatory text under.
//!
//! The rule under test, asserted over a TABLE of refusals rather than the one a recording
//! happened to carry:
//!
//! 1. A `ValidationException` of this service carries its text under the lower-cased
//!    `message` key, and under that spelling ALONE — whatever operation refused, whatever
//!    the condition, and whether the refusal came from the modelled-constraint walk at the
//!    boundary or from a handler. The model spells that member `Message`
//!    (`smithy_query(query="shape", shape="ValidationException")` reports
//!    `{"Message": {"target": "String"}, "ReasonCode": {"target": "String"}}`), so this is
//!    the one shape of this service whose wire key is not its member name: it is the
//!    refusal the request-validation front end words, and that envelope keys its text
//!    `message`.
//! 2. Every OTHER error shape carries its text under the key the model spells for it —
//!    `Message` for most, already lower case where the model itself spells it lower case
//!    (`InvalidInstanceInformationFilterValue$message`) — so the rule above is about one
//!    shape and not about the service's whole error surface.
//! 3. A served call's body is untouched by any of this: a 200 carries no message member
//!    under either spelling.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`]) dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>` and an `awsJson1_1` body through the
//! multi-service router — so what is asserted is the key on the wire.

use serde_json::{json, Value};

use crate::just_in_time_node_access::target_forms;
use crate::parameter_store::{call, error_code, router};

/// The key this service keys a `ValidationException`'s text under.
const SERVICE_VALIDATION_KEY: &str = "message";

/// The key the model spells the message member of this service's error shapes with, which
/// every shape but the one above is keyed by.
const MODELLED_KEY: &str = "Message";

/// The sentence a just-in-time node access call against an account that has not turned the
/// workflow on is refused with. Fixed text for that error class, with no variable part —
/// spelled out here rather than read from the crate, so the wording itself is asserted.
const NOT_ENABLED: &str = "Just-In-Time Node Access service is not enabled in this account.";

/// The sentence a well-formed `AccessRequestId` that names no request the Region issued is
/// refused with.
const ACCESS_REQUEST_NOT_FOUND: &str =
    "The specified access request ID doesn't exist. Verify the ID and try again.";

/// The sentence an operation that previews an Automation runbook refuses a name no runbook
/// of the Region carries with.
const RUNBOOK_NOT_FOUND: &str =
    "Invalid input for field document name, expected valid document in account.";

/// The sentence a `NextToken` an operation never issued is refused with.
const INVALID_NEXT_TOKEN: &str = "The NextToken value is not valid";

/// The sentence a request naming both managed-node filter dialects is refused with.
const BOTH_FILTER_DIALECTS: &str = "Cannot use both legacy and new filter simultaneously.";

/// One row: the call to make, and the text its answer must state (`None` where another
/// suite owns the wording and only the KEY is under test here).
struct Case {
    name: String,
    operation: &'static str,
    body: Value,
    message: Option<String>,
}

/// The text one error body carries, read under `expected_key` ALONE — the rule under test
/// — asserting the body serves no other spelling of it.
fn message_under(case: &str, body: &Value, expected_key: &str, other_key: &str) -> String {
    assert!(
        body.get(other_key).is_none(),
        "case {case:?}: the body must carry its text under {expected_key:?} alone, but it \
         also serves {other_key:?}: got {body}"
    );
    body.get(expected_key)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the body carried no {expected_key:?} at all: got {body}")
        })
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {expected_key:?} is not a string: got {body}"))
        .to_string()
}

/// Drive one row and assert its status, its wire error code, the key its text travels
/// under and — where the row states one — the text itself.
async fn assert_keyed(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &Case,
    expected_code: &str,
    expected_key: &str,
    other_key: &str,
) {
    let name = case.name.as_str();
    let (status, answered) = call(router, case.operation, &case.body).await;
    assert_eq!(
        status, 400,
        "case {name:?} ({}): expected HTTP 400, got {status} {answered}",
        case.body
    );
    assert_eq!(
        error_code(&answered),
        expected_code,
        "case {name:?} ({}): expected {expected_code}, got {answered}",
        case.body
    );
    let stated = message_under(name, &answered, expected_key, other_key);
    match &case.message {
        Some(expected) => assert_eq!(
            &stated, expected,
            "case {name:?} ({}): expected {expected:?}, got {stated:?}",
            case.body
        ),
        None => assert!(
            !stated.trim().is_empty(),
            "case {name:?} ({}): expected a non-empty text, got {stated:?}",
            case.body
        ),
    }
}

/// RULE 1: every `ValidationException` of this service — from five operations, from both
/// the boundary walk and the handlers, and from every condition each of them refuses —
/// states its text under the lower-cased `message` key and under nothing else.
#[tokio::test]
async fn every_validation_exception_keys_its_text_as_the_service_serves_it() {
    let router = router();
    let mut cases: Vec<Case> = Vec::new();

    // The just-in-time node access account gate, which this account has not turned on:
    // the token read, over well-formed ids spanning the modelled pattern's digits …
    for id in ["oi-000000000000", "oi-0123456789ab", "oi-ffffffffffff"] {
        cases.push(Case {
            name: format!("GetAccessToken {id} while the account has no approval policy"),
            operation: "GetAccessToken",
            body: json!({ "AccessRequestId": id }),
            message: Some(NOT_ENABLED.to_string()),
        });
    }
    // … and the start, over every form a target may take, including an invalid node id and
    // no target at all: the gate is reached before any target is looked at.
    for (form, targets) in target_forms() {
        cases.push(Case {
            name: format!("StartAccessRequest for {form} while the account has no approval policy"),
            operation: "StartAccessRequest",
            body: json!({ "Reason": "a reason", "Targets": targets }),
            message: Some(NOT_ENABLED.to_string()),
        });
    }
    cases.push(Case {
        name: "StartAccessRequest for a node id outside the EC2 id form".to_string(),
        operation: "StartAccessRequest",
        body: json!({
            "Reason": "a reason",
            "Targets": [{ "Key": "InstanceIds", "Values": ["not-an-instance-id"] }],
        }),
        message: Some(NOT_ENABLED.to_string()),
    });

    // The SAME shape raised by the modelled-constraint walk at the boundary rather than by
    // a handler: `AccessRequestId`'s `^(oi)-[0-9a-f]{12}$` pattern. The wording belongs to
    // the boundary suite; the key is what is under test.
    for id in ["", "oi-", "oi-0123456789AB", "not-an-access-request-id"] {
        cases.push(Case {
            name: format!("GetAccessToken with the malformed id {id:?}"),
            operation: "GetAccessToken",
            body: json!({ "AccessRequestId": id }),
            message: None,
        });
    }

    // Another operation, another condition: a runbook name this Region holds no Automation
    // document for.
    for name in ["NoSuchRunbook", "Another_Missing.Runbook"] {
        cases.push(Case {
            name: format!("StartExecutionPreview of the unknown runbook {name}"),
            operation: "StartExecutionPreview",
            body: json!({ "DocumentName": name }),
            message: Some(RUNBOOK_NOT_FOUND.to_string()),
        });
    }

    // A property an operating system does not publish, over three operating-system
    // families (`smithy_query(query="doc", shape="DescribePatchProperties")` lists each
    // one's valid properties).
    for (operating_system, property) in [
        ("UBUNTU", "CLASSIFICATION"),
        ("DEBIAN", "MSRC_SEVERITY"),
        ("MACOS", "SEVERITY"),
    ] {
        cases.push(Case {
            name: format!("DescribePatchProperties {property} of {operating_system}"),
            operation: "DescribePatchProperties",
            body: json!({ "OperatingSystem": operating_system, "Property": property }),
            message: Some(format!(
                "Property {property} is not supported for operating system {operating_system}"
            )),
        });
    }
    // And a paging token the same read never issued.
    cases.push(Case {
        name: "DescribePatchProperties with a NextToken it never issued".to_string(),
        operation: "DescribePatchProperties",
        body: json!({
            "OperatingSystem": "WINDOWS",
            "Property": "PRODUCT",
            "NextToken": "not-a-token-from-here",
        }),
        message: Some(INVALID_NEXT_TOKEN.to_string()),
    });

    // A combination of members the request may not express.
    cases.push(Case {
        name: "DescribeInstanceInformation naming both filter dialects".to_string(),
        operation: "DescribeInstanceInformation",
        body: json!({
            "Filters": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
            "InstanceInformationFilterList": [
                { "key": "InstanceIds", "valueSet": ["i-0123456789abcdef0"] }
            ],
        }),
        message: Some(BOTH_FILTER_DIALECTS.to_string()),
    });

    for case in &cases {
        assert_keyed(
            &router,
            case,
            "ValidationException",
            SERVICE_VALIDATION_KEY,
            MODELLED_KEY,
        )
        .await;
    }
}

/// RULE 1, in a Region that HAS the workflow: the refusal of a well-formed id no start
/// issued is keyed the same way as the gate refusal above — and RULE 3: a served call's
/// body carries no message member under either spelling.
#[tokio::test]
async fn an_enabled_region_keys_its_access_request_refusals_the_same_way() {
    let router = router();
    // An account holds just-in-time node access in a Region once it holds an access-request
    // approval policy there, created through this service's own `CreateDocument`.
    let (status, created) = call(
        &router,
        "CreateDocument",
        &json!({
            "Name": "JitnaManualApproval",
            "DocumentType": "ManualApprovalPolicy",
            "Content": "{\"schemaVersion\": \"1.0\", \"description\": \"who may approve\"}",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the approval policy must be created, got {status} {created}"
    );

    let issued = {
        let (status, started) = call(
            &router,
            "StartAccessRequest",
            &json!({
                "Reason": "a reason",
                "Targets": [{ "Key": "InstanceIds", "Values": ["i-0123456789abcdef0"] }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "a start in the enabled Region must succeed, got {status} {started}"
        );
        assert!(
            started.get(MODELLED_KEY).is_none() && started.get(SERVICE_VALIDATION_KEY).is_none(),
            "a served start states no message under either spelling, got {started}"
        );
        started["AccessRequestId"]
            .as_str()
            .unwrap_or_else(|| panic!("a start answers an AccessRequestId, got {started}"))
            .to_string()
    };

    for id in ["oi-000000000000", "oi-ffffffffffff", "oi-abcdefabcdef"] {
        assert_ne!(
            id, issued,
            "the row must name an id no start minted, and a start minted {issued:?}"
        );
        assert_keyed(
            &router,
            &Case {
                name: format!("GetAccessToken of the unissued {id}"),
                operation: "GetAccessToken",
                body: json!({ "AccessRequestId": id }),
                message: Some(ACCESS_REQUEST_NOT_FOUND.to_string()),
            },
            "ValidationException",
            SERVICE_VALIDATION_KEY,
            MODELLED_KEY,
        )
        .await;
    }

    // The request that WAS issued still reads back, and its served body states no message
    // either.
    let (status, read) = call(
        &router,
        "GetAccessToken",
        &json!({ "AccessRequestId": issued }),
    )
    .await;
    assert_eq!(
        (status, read["AccessRequestStatus"].as_str()),
        (200, Some("Pending")),
        "the request that was raised must read back, got {status} {read}"
    );
    assert!(
        read.get(MODELLED_KEY).is_none() && read.get(SERVICE_VALIDATION_KEY).is_none(),
        "a served token read states no message under either spelling, got {read}"
    );
}

/// RULE 2: a refusal that is NOT a `ValidationException` keys its text exactly as the model
/// spells that shape's message member — `Message` for a shape the model spells with a
/// capital, `message` for one the model itself spells lower case — so the re-keying above
/// is about one shape rather than about this service's error surface.
#[tokio::test]
async fn a_refusal_that_is_not_a_validation_exception_keys_its_text_as_the_model_spells_it() {
    let router = router();

    // (case, operation, body, code, the key the model spells for that shape)
    let rows: Vec<(&str, &'static str, Value, &str, &str)> = vec![
        (
            "GetDocument of a document this Region does not hold",
            "GetDocument",
            json!({ "Name": "NoSuchDocument" }),
            // `InvalidDocument$Message` (`smithy_query(query="shape", shape="InvalidDocument")`).
            "InvalidDocument",
            MODELLED_KEY,
        ),
        (
            "GetExecutionPreview of a preview id no start issued",
            "GetExecutionPreview",
            json!({ "ExecutionPreviewId": "00000000-0000-0000-0000-000000000000" }),
            // `ResourceNotFoundException$Message`.
            "ResourceNotFoundException",
            MODELLED_KEY,
        ),
        (
            "DescribeInstanceInformation filtering on a value no node can hold",
            "DescribeInstanceInformation",
            json!({ "Filters": [{ "Key": "PingStatus", "Values": ["NotAPublishedValue"] }] }),
            // `InvalidInstanceInformationFilterValue$message` — the model's own lower case.
            "InvalidInstanceInformationFilterValue",
            "message",
        ),
    ];

    for (name, operation, body, code, key) in rows {
        let other = if key == MODELLED_KEY {
            SERVICE_VALIDATION_KEY
        } else {
            MODELLED_KEY
        };
        assert_keyed(
            &router,
            &Case {
                name: name.to_string(),
                operation,
                body,
                message: None,
            },
            code,
            key,
            other,
        )
        .await;
    }
}

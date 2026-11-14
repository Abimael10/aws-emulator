// SPDX-License-Identifier: CC0-1.0

//! Which error SHAPE a refusal of this service is serialized from.
//!
//! The rule under test: a refusal a handler of this service answers is built from an error
//! shape THIS SERVICE'S MODEL declares, so the detail it carries reaches the wire under
//! that shape's own modelled member — `Message`, the single member of every exception in
//! `com.amazonaws.costandusagereportservice` (`smithy_query(shape="ValidationException")`)
//! — and never under a member of a shape outside the model. One service answers one
//! spelling, so a client reading a refusal's detail reads it the same way whichever
//! operation refused.
//!
//! The table below spans what that rule generalises over: the operation that refuses
//! (`DescribeReportDefinitions`, `ModifyReportDefinition`, `PutReportDefinition`,
//! `TagResource`), the condition (a continuation token the service never issued, a report
//! name the account does not hold, a definition whose Parquet preference is unusable, more
//! tags than one report may hold), and — for the token condition — the form of the token.
//! The recorded instance is one row.

use serde_json::{json, Value};

use service_com_amazonaws_costandusagereportservice_awsorigamiservicegatewayservice::state::MAX_TAGS_PER_REPORT;

use super::report_definition_lifecycle::{error_code, minimal_definition, Cur};

/// The member this service's model declares on every one of its exceptions
/// (`smithy_query(shape="ValidationException,ResourceNotFoundException")`), and so the
/// only key a refusal's detail may arrive under.
const MODELLED_DETAIL_MEMBER: &str = "Message";

/// The member the synthetic framework exception declares. No handler-built refusal of this
/// service may land its detail here: that shape is in no service's model, so a client
/// reading the modelled member would find nothing.
const UNMODELLED_DETAIL_MEMBER: &str = "message";

/// One refusal: the calls that build the account state, the call that must be refused, and
/// the error code the model names for that condition.
struct Refusal {
    case: &'static str,
    setup: Vec<(&'static str, Value)>,
    refused: (&'static str, Value),
    code: &'static str,
}

fn put(definition: &Value) -> (&'static str, Value) {
    (
        "PutReportDefinition",
        json!({ "ReportDefinition": definition }),
    )
}

/// A definition whose Parquet preference cannot hold together: `Format` states Parquet
/// while `Compression` does not, which is the condition both the create and the update
/// path refuse.
fn half_parquet_definition(name: &str) -> Value {
    let mut definition = minimal_definition(name);
    definition["Format"] = json!("Parquet");
    definition["Compression"] = json!("GZIP");
    definition
}

fn refusal_table() -> Vec<Refusal> {
    let over_cap: Vec<Value> = (0..=MAX_TAGS_PER_REPORT)
        .map(|index| json!({ "Key": format!("key-{index}"), "Value": "v" }))
        .collect();
    // Token forms spanning the ways a value can satisfy the modelled `NextToken` pattern
    // `^[A-Za-z0-9_\.\-=]*$` without being a token this store minted: not hex at all, an
    // odd number of hex digits, hex too short to carry an authenticator, and hex long
    // enough but authenticated by nothing.
    let tokens = [
        ("not hex at all", "zzz".to_string()),
        ("an odd number of hex digits", "6669727".to_string()),
        ("hex too short to carry an authenticator", "00".to_string()),
        ("hex of a report name", hex::encode("ghost-report")),
        ("hex long enough but authenticated by nothing", "ab".repeat(48)),
    ];

    let mut table = vec![
        Refusal {
            case: "the update names a report the account does not hold",
            setup: vec![],
            refused: (
                "ModifyReportDefinition",
                json!({
                    "ReportName": "never-created",
                    "ReportDefinition": minimal_definition("never-created"),
                }),
            ),
            code: "ValidationException",
        },
        Refusal {
            case: "a created definition states Parquet in one member only",
            setup: vec![],
            refused: put(&half_parquet_definition("half-parquet")),
            code: "ValidationException",
        },
        Refusal {
            case: "an updated definition states Parquet in one member only",
            setup: vec![put(&minimal_definition("held-report"))],
            refused: (
                "ModifyReportDefinition",
                json!({
                    "ReportName": "held-report",
                    "ReportDefinition": half_parquet_definition("held-report"),
                }),
            ),
            code: "ValidationException",
        },
        Refusal {
            case: "more tags than one report may hold, supplied beside the definition",
            setup: vec![],
            refused: (
                "PutReportDefinition",
                json!({
                    "ReportDefinition": minimal_definition("over-tagged"),
                    "Tags": over_cap.clone(),
                }),
            ),
            code: "ValidationException",
        },
        Refusal {
            case: "more tags than one report may hold, merged onto a held report",
            setup: vec![put(&minimal_definition("tag-target"))],
            refused: (
                "TagResource",
                json!({ "ReportName": "tag-target", "Tags": over_cap }),
            ),
            code: "ValidationException",
        },
    ];

    for (form, token) in tokens {
        table.push(Refusal {
            case: form,
            setup: vec![put(&minimal_definition("paged-report"))],
            refused: (
                "DescribeReportDefinitions",
                json!({ "NextToken": token }),
            ),
            code: "ValidationException",
        });
    }

    table
}

#[tokio::test]
async fn every_handler_refusal_carries_its_detail_under_the_modelled_member() {
    for refusal in refusal_table() {
        let Refusal {
            case,
            setup,
            refused,
            code,
        } = refusal;
        let cur = Cur::new();
        for (operation, body) in &setup {
            let (status, response) = cur.call(operation, body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: setup call {operation} got {status} {response:?}"
            );
        }

        let (operation, body) = &refused;
        let (status, response) = cur.call(operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {operation}, got {status} {response:?}"
        );
        assert_eq!(
            error_code(&response),
            code,
            "case {case:?}: expected error code {code}, got {:?}",
            error_code(&response)
        );

        let detail = response
            .get(MODELLED_DETAIL_MEMBER)
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            !detail.is_empty(),
            "case {case:?}: expected the detail under the modelled member \
             {MODELLED_DETAIL_MEMBER:?}, got {response:?}"
        );
        assert!(
            response.get(UNMODELLED_DETAIL_MEMBER).is_none(),
            "case {case:?}: the detail must not arrive under {UNMODELLED_DETAIL_MEMBER:?}, \
             got {response:?}"
        );
    }
}

#[tokio::test]
async fn one_service_answers_one_detail_member() {
    // The rule stated as the invariant across operations: whatever each refusal says, the
    // key its detail arrives under is the same for every operation of this service, so no
    // single operation can drift onto a shape of its own.
    let mut keys: Vec<(&str, Vec<String>)> = Vec::new();
    for refusal in refusal_table() {
        let Refusal {
            case,
            setup,
            refused,
            ..
        } = refusal;
        let cur = Cur::new();
        for (operation, body) in &setup {
            let (status, response) = cur.call(operation, body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: setup call {operation} got {status} {response:?}"
            );
        }

        let (operation, body) = &refused;
        let (status, response) = cur.call(operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {operation}, got {status} {response:?}"
        );
        let members: Vec<String> = response
            .as_object()
            .map(|body| {
                body.keys()
                    .filter(|key| key.as_str() != "__type")
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        keys.push((case, members));
    }

    let (first_case, expected) = keys
        .first()
        .cloned()
        .expect("the refusal table is not empty");
    assert_eq!(
        expected,
        vec![MODELLED_DETAIL_MEMBER.to_string()],
        "case {first_case:?}: expected only the modelled detail member, got {expected:?}"
    );
    for (case, members) in &keys {
        assert_eq!(
            members, &expected,
            "case {case:?}: expected the same members as case {first_case:?} ({expected:?}), \
             got {members:?}"
        );
    }
}

#[tokio::test]
async fn a_refused_page_token_is_not_echoed_back() {
    // The detail says what the caller must pass instead; it is not a quotation of what the
    // caller passed, so the rejected token appears nowhere in the body.
    let probes = [
        "deadbeefdeadbeef".to_string(),
        hex::encode("echo-probe-token"),
        "zz-echo-probe".to_string(),
    ];

    for token in probes {
        let cur = Cur::new();
        let (status, response) = cur.describe(&json!({ "NextToken": token })).await;
        assert_eq!(
            status, 400,
            "case {token:?}: expected HTTP 400, got {status} {response:?}"
        );
        let rendered = response.to_string();
        assert!(
            !rendered.contains(token.as_str()),
            "case {token:?}: the refusal must not echo the rejected token, got {rendered}"
        );
    }
}

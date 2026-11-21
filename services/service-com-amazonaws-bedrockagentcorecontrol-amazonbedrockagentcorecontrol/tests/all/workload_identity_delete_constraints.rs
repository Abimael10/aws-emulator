// SPDX-License-Identifier: CC0-1.0

//! What `DeleteWorkloadIdentity` answers for a `name` its modelled constraints do not
//! admit, and what such a request leaves behind.
//!
//! The rule under test, in one sentence: a delete request whose `name` falls outside
//! `WorkloadIdentityNameType` — `@length(min: 3, max: 255)` and
//! `@pattern("^[A-Za-z0-9_.-]+$")`, from `smithy_query(query="operation",
//! operation="DeleteWorkloadIdentity")` — is refused at the boundary with the modelled 400
//! `ValidationException` naming the member and the requirement alone, removes nothing, and
//! leaves every identity this account holds to be read, listed and deleted afterwards.
//!
//! The suites beside this one drive the delete only with names INSIDE those bounds, so
//! their refusals come from the handler. This one covers the other side of the same
//! operation: both published length bounds, values past each of them, and every character
//! class the pattern excludes — then proves the directory the refusals could not touch is
//! still the one the creates left.
//!
//! The wording of a refusal clause is the rule `validation_message_rendering.rs` states for
//! this service's `/identities` family: the member and the requirement, with no value
//! quoted back.

use serde_json::{json, Value};

use super::harness::{
    carries_no_member, create_identity, delete_identity, error_code, get_identity, router,
    stored_names, text, NAME_MAX_LENGTH, NAME_MIN_LENGTH,
};

/// `DeleteWorkloadIdentity`'s modelled success status, and the status every refusal of
/// input outside a modelled constraint carries
/// (`smithy_query(query="operation", operation="DeleteWorkloadIdentity")`: `http.code`, and
/// `ValidationException`'s `httpError`).
const DELETE_STATUS: u16 = 204;
const REFUSAL_STATUS: u16 = 400;

/// `WorkloadIdentityNameType`'s modelled `@pattern`, as the model spells it (anchored).
const NAME_PATTERN: &str = "^[A-Za-z0-9_.-]+$";

/// One refused delete: the name it carries and every requirement its clause must state —
/// a value can break more than one of the member's constraints at once.
struct Row {
    case: &'static str,
    name: String,
    requirements: Vec<String>,
}

/// How a `@length` minimum reads as a requirement.
fn min_length_requirement() -> String {
    format!("Member must have length greater than or equal to {NAME_MIN_LENGTH}")
}

/// How a `@length` maximum reads as a requirement.
fn max_length_requirement() -> String {
    format!("Member must have length less than or equal to {NAME_MAX_LENGTH}")
}

/// How a `@pattern` reads as a requirement: the expression WITHOUT the whole-value anchors
/// the model spells it with. The row carries the modelled form, so a clause quoting the
/// model's spelling fails here rather than passing.
fn pattern_requirement() -> String {
    let unanchored = NAME_PATTERN
        .strip_prefix('^')
        .and_then(|rest| rest.strip_suffix('$'))
        .expect("the modelled pattern is anchored at both ends");
    format!("Member must satisfy regular expression pattern: {unanchored}")
}

/// The clause a refusal of this service's `/identities` family answers with: a count, then
/// one `Value at '<member>' failed to satisfy constraint: <requirement>` per broken
/// constraint, in the order the member declares them.
fn expected_clause(requirements: &[String]) -> String {
    let detected = if requirements.len() == 1 {
        "1 validation error detected".to_string()
    } else {
        format!("{} validation errors detected", requirements.len())
    };
    let clauses: Vec<String> = requirements
        .iter()
        .map(|requirement| {
            format!("Value at 'name' failed to satisfy constraint: {requirement}")
        })
        .collect();
    format!("{detected}: {}", clauses.join("; "))
}

/// A delete naming a value outside `WorkloadIdentityNameType` is refused with the modelled
/// 400 `ValidationException`, and the account's workload-identity directory is exactly what
/// the creates left: nothing was removed, and the identities at both published length
/// bounds still read back and still delete.
///
/// Rows differ along which constraint the name breaks and by how much: the empty value,
/// values below the published minimum (one short, and shorter still), values above the
/// published maximum (one over, and far over), and one name per character class the
/// pattern excludes — whitespace, punctuation outside `_.-`, and a character outside ASCII.
#[tokio::test]
async fn a_delete_outside_the_modelled_name_constraints_is_refused_and_removes_nothing() {
    let router = router();

    // The identities the refusals are measured against: both published length bounds and
    // every character class the pattern DOES admit.
    let held: [String; 3] = [
        "a".repeat(NAME_MIN_LENGTH),
        "payments.service_v1-east".to_string(),
        "Z9".repeat(NAME_MAX_LENGTH / 2) + "Z",
    ];
    for name in &held {
        let (status, created) = create_identity(&router, &json!({"name": name})).await;
        assert_eq!(
            status, 201,
            "case \"the identity a refusal is measured against\" ({} characters): expected HTTP \
             201, got {status} ({created})",
            name.len()
        );
    }
    let mut expected_directory = held.to_vec();
    expected_directory.sort();
    assert_eq!(
        stored_names(&router).await,
        expected_directory,
        "case \"the directory the refusals start from\": expected the created identities, got \
         {:?}",
        stored_names(&router).await
    );

    let rows: [Row; 9] = [
        Row {
            // The pattern admits no empty value either, so this one value breaks both of
            // the member's constraints and the clause states both.
            case: "the empty name",
            name: String::new(),
            requirements: vec![min_length_requirement(), pattern_requirement()],
        },
        Row {
            case: "one character below the published minimum",
            name: "a".repeat(NAME_MIN_LENGTH - 1),
            requirements: vec![min_length_requirement()],
        },
        Row {
            case: "a single character, further below the minimum",
            name: "a".to_string(),
            requirements: vec![min_length_requirement()],
        },
        Row {
            case: "one character above the published maximum",
            name: "z".repeat(NAME_MAX_LENGTH + 1),
            requirements: vec![max_length_requirement()],
        },
        Row {
            case: "far above the published maximum",
            name: "z".repeat(NAME_MAX_LENGTH * 4),
            requirements: vec![max_length_requirement()],
        },
        Row {
            case: "whitespace between characters, which the pattern excludes",
            name: "payments service".to_string(),
            requirements: vec![pattern_requirement()],
        },
        Row {
            case: "a path separator, which the pattern excludes",
            name: "payments/service".to_string(),
            requirements: vec![pattern_requirement()],
        },
        Row {
            case: "punctuation outside the admitted _.-, which the pattern excludes",
            name: "payments@service!".to_string(),
            requirements: vec![pattern_requirement()],
        },
        Row {
            case: "a character outside ASCII, which the pattern excludes",
            name: "idéntité".to_string(),
            requirements: vec![pattern_requirement()],
        },
    ];

    for row in &rows {
        let Row {
            case,
            name,
            requirements,
        } = row;

        let (status, answer) = delete_identity(&router, name).await;
        assert_eq!(
            (status, error_code(&answer)),
            (REFUSAL_STATUS, "ValidationException".to_string()),
            "case {case:?}: expected HTTP {REFUSAL_STATUS} ValidationException, got {status} {} \
             ({answer})",
            error_code(&answer)
        );
        let clause = expected_clause(requirements);
        assert_eq!(
            text(&answer, "message"),
            clause,
            "case {case:?}: expected the clause {clause:?}, got {:?}",
            text(&answer, "message")
        );
        // The no-echo half of the family's wording rule, asserted on the rows whose name
        // could not turn up inside the clause's own prose by chance (a name of fewer than
        // four characters can).
        if name.chars().count() >= 4 {
            assert!(
                !text(&answer, "message").contains(name.as_str()),
                "case {case:?}: this family's clause quotes no value, yet {:?} carries the \
                 submitted name",
                text(&answer, "message")
            );
        }

        // The refusal removed nothing: the directory still reports every held identity.
        assert_eq!(
            stored_names(&router).await,
            expected_directory,
            "case {case:?}: a refused delete removes nothing, got {:?}",
            stored_names(&router).await
        );
    }

    // Every held identity is still readable member for member after the whole table, and
    // still deletes: the refusals left the operation itself working.
    for name in &held {
        let (status, read) = get_identity(&router, name).await;
        assert_eq!(
            (status, text(&read, "name")),
            (200, name.clone()),
            "case \"a held identity after every refusal\": expected HTTP 200 for {name:?}, got \
             {status} ({read})"
        );

        let (status, deleted) = delete_identity(&router, name).await;
        assert_eq!(
            status, DELETE_STATUS,
            "case \"a delete inside the constraints after every refusal\": expected HTTP \
             {DELETE_STATUS} for {name:?}, got {status} ({deleted})"
        );
        assert!(
            carries_no_member(&deleted),
            "case \"a delete inside the constraints after every refusal\": a success carries no \
             member, got {deleted}"
        );
    }
    assert_eq!(
        stored_names(&router).await,
        Vec::<String>::new(),
        "case \"after deleting every held identity\": expected an empty directory, got {:?}",
        stored_names(&router).await
    );
    let (status, read) = get_identity(&router, &held[0]).await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (404, "ResourceNotFoundException"),
        "case \"a read of a deleted identity\": expected HTTP 404 ResourceNotFoundException, got \
         {status} ({read})"
    );
    assert_eq!(
        read.get("name").cloned().unwrap_or(Value::Null),
        Value::Null,
        "case \"a read of a deleted identity\": a refusal reports no identity, got {read}"
    );
}

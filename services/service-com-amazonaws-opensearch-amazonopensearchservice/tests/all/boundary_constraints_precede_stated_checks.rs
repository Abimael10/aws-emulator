// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH refusal a request carrying more than one bad value is
//! answered with, where one of them breaks a constraint the request boundary enforces and
//! the other breaks a rule the service applies to a value itself.
//!
//! The rule, one level above the recorded case (an attach whose application `id` was a
//! GUID and whose `dataSourceArn` named an Amazon S3 bucket, answered with the ARN's
//! clause instead of the id's):
//!
//! > A request is refused by the CONSTRAINT BOUNDARY first: the report states the clauses
//! > of the members whose declared constraints the request broke — wherever a member is
//! > bound, a URI label included — and a check the SERVICE makes on the value of a member
//! > whose own constraints hold (the grammar an ARN has to satisfy, which this model
//! > declares as `@pattern(".*")`) is reached only for a request the boundary accepted. So
//! > one request never states both, and the clause a declared constraint earns is never
//! > spoken over.
//!
//! The rows span what the rule generalises over: WHY the `id` is outside the expression an
//! application id satisfies (a GUID, a leading hyphen, below the minimum length, past the
//! maximum, an upper-case letter, an underscore), crossed with WHAT the body carries — an
//! ARN the service's own grammar refuses, and one it accepts — so a row cannot pass by
//! refusing whatever came last. The recorded case is ONE row.
//!
//! Two control rows hold the other side of the rule: a well-formed `id` with an ARN the
//! grammar refuses IS answered with that member's clause, and a well-formed `id` with an
//! ARN the grammar admits reaches the application lookup.

use serde_json::json;

use super::constraint_refusal_messages::message;
use super::data_source_attachments::{application, attach};
use super::domain_lifecycle::{error_code, Emulator, RECORDED_NOT_FOUND_STATUS};

/// The expression an application `id` satisfies, as this service PUBLISHES it: hyphens are
/// admitted inside the id and a leading one is refused. It is the expression this surface's
/// refusals quote (`crate::service::validation`'s `STATED` table records why it is not the
/// model's `Id` `@pattern`, `^[a-z0-9]{3,30}$`).
const ID_EXPRESSION: &str = r"^(?!-)[a-z0-9\-]{3,30}$";

/// The status and code a refused input is answered with: `ValidationException` is
/// `@httpError(400)` (`smithy_query(query="operation",operation="AttachDataSource")`).
const REFUSED: (u16, &str) = (400, "ValidationException");

/// An ARN this service's own grammar refuses as a data source: an Amazon S3 bucket, which
/// names no kind of resource an application attaches.
const UNSUPPORTED_ARN: &str = "arn:aws:s3:::probe-bucket-that-is-no-data-source";

/// An ARN of the right kind and owner: a domain of this account, which the grammar admits
/// whether or not the domain exists — the attachment carries that outcome in its status.
fn supported_arn() -> String {
    format!(
        "arn:aws:es:us-east-1:{}:domain/boundary-first-domain",
        ::engine::emulator_account_id()
    )
}

/// The clause a member's `@pattern` earns, under the envelope one refused member states.
fn id_clause(id: &str) -> String {
    format!(
        "1 validation error detected: Value '{id}' at 'id' failed to satisfy constraint: \
         Member must satisfy regular expression pattern: {ID_EXPRESSION}"
    )
}

/// Every `id` outside the expression, and why each is.
fn ids_outside_the_expression() -> Vec<(&'static str, String)> {
    vec![
        (
            "the recorded case: a GUID, which carries hyphens and is longer than the maximum",
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        ("a leading hyphen", "-leading-hyphen".to_string()),
        ("one character below the minimum", "ab".to_string()),
        ("one character past the maximum", "a".repeat(31)),
        ("an upper-case letter", "Application".to_string()),
        ("an underscore", "under_score".to_string()),
    ]
}

/// An `id` the boundary refuses is answered with that label's clause ALONE, whatever the
/// body carries — the ARN the service's own grammar would refuse included.
#[tokio::test]
async fn the_label_s_own_clause_is_the_whole_refusal() {
    let emulator = Emulator::new();
    let supported = supported_arn();

    for (why, id) in ids_outside_the_expression() {
        for (carries, arn) in [
            ("an ARN the service's grammar refuses", UNSUPPORTED_ARN),
            ("an ARN the service's grammar admits", supported.as_str()),
        ] {
            let case = format!("the id {id:?} ({why}) with {carries}");
            let answer = attach(&emulator, &id, json!({ "dataSourceArn": arn })).await;
            assert_eq!(
                (answer.status, error_code(&answer).as_str()),
                REFUSED,
                "case {case}: expected the modelled validation error, got {} with {:?}",
                answer.status,
                answer.body
            );
            let expected = id_clause(&id);
            let stated = message(&answer, &case);
            assert_eq!(
                stated, expected,
                "case {case}: expected {expected:?}, got {stated:?}"
            );
            assert!(
                !stated.contains("dataSourceArn"),
                "case {case}: the boundary's report states the refused label alone, got \
                 {stated:?}"
            );
        }
    }
}

/// A well-formed `id` carries the request past the boundary, so the service's own check of
/// the ARN's grammar is the one that speaks — at the `dataSourceArn` member, and about no
/// other.
#[tokio::test]
async fn a_request_the_boundary_accepts_reaches_the_service_s_own_check() {
    let emulator = Emulator::new();
    let held = application(&emulator, "boundary-first-app").await;

    // Every well-formed id: the one an application was minted under, and two more inside
    // the expression (the minimum length, and a longer lower-case token) that name no
    // application — the grammar check runs before the application is resolved, so all three
    // state it.
    for id in [held.as_str(), "absentapplication", "abc"] {
        let case = format!("the well-formed id {id:?} with an ARN the grammar refuses");
        let answer = attach(
            &emulator,
            id,
            json!({ "dataSourceArn": UNSUPPORTED_ARN }),
        )
        .await;
        assert_eq!(
            (answer.status, error_code(&answer).as_str()),
            REFUSED,
            "case {case}: expected the modelled validation error, got {} with {:?}",
            answer.status,
            answer.body
        );
        let stated = message(&answer, &case);
        assert!(
            stated.contains("at 'dataSourceArn'") && !stated.contains("at 'id'"),
            "case {case}: expected the dataSourceArn member's clause, got {stated:?}"
        );
    }

    // And an ARN the grammar admits leaves nothing for either check to state: the request
    // reaches the application the label names, which is what answers it.
    let case = "the well-formed id of an application the account holds";
    let answer = attach(&emulator, &held, json!({ "dataSourceArn": supported_arn() })).await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected the attach to be served, got {} with {:?}",
        answer.status, answer.body
    );

    let case = "a well-formed id no application was minted under";
    let answer = attach(
        &emulator,
        "absentapplication",
        json!({ "dataSourceArn": supported_arn() }),
    )
    .await;
    assert_eq!(
        (answer.status, error_code(&answer).as_str()),
        (RECORDED_NOT_FOUND_STATUS, "ResourceNotFoundException"),
        "case {case}: expected the modelled not-found, got {} with {:?}",
        answer.status,
        answer.body
    );
}

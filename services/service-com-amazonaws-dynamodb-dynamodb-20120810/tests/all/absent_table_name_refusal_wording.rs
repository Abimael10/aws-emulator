// SPDX-License-Identifier: CC0-1.0

//! The FORMS a request states no value for the table-name member in, over the readers whose
//! refusal of that absence is the validating walk's clause.
//!
//! The rule, driven over those forms rather than over one recorded request: a request that
//! states no value for a member its input marks `@required` is refused HTTP 400
//! `ValidationException` whose whole message is that walk's absent-member clause inside the
//! envelope that COUNTS the violations — `<N> validation error(s) detected: Value null at
//! '<member path>' failed to satisfy constraint: Member must not be null` — with the member
//! named by the lowerCamel input path the walk reports it under and never by the PascalCase
//! name the shape spells. The count here is one, because the member is the one member each
//! of these readers' inputs requires (`smithy_query(query="operation",
//! operation="DescribeTable,DeleteTable,Scan")` gives `TableName`, target `TableArn`,
//! `@length` 1..1024, as the one required member of each).
//!
//! What this suite adds to the two beside it: `crate::required_member_refusals`'s rule 1
//! pins WHICH operations state the clause and which state this service's own
//! parameter-required sentence, and `crate::required_null_member_reports` pins the absence
//! over the modelled types and places a member can stand in. Neither drives the one member
//! this service's own request code requires over the forms its absence takes, which is what
//! the rows below do: the member left out of an empty body, left out of the rest of the
//! reader's own well-formed request, or carried as an explicit `null`. The boundary of the
//! same rule is here too — a member the request DOES carry a value for is not an absent
//! member, so it earns the bound that value broke, and a request that states the member is
//! served and read back.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The whole message such an absence earns, as AWS words it: the counting envelope over the
/// walk's clause about the member, named by the path that walk reports it under.
const ABSENT_TABLE_NAME: &str = "1 validation error detected: Value null at 'tableName' failed \
                                 to satisfy constraint: Member must not be null";

/// The refusal `operation` answers `request` with: its code and its message, over a call
/// that must not succeed.
async fn refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: Value,
) -> (String, String) {
    let (status, answered) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?} ({operation}): expected HTTP 400, got {status} with {answered}"
    );
    (error_code(&answered), message(&answered))
}

/// The readers observed to state the walk's clause for an absent table name, each with a
/// well-formed request of its own the absence is taken out of — so the forms are driven over
/// the control plane and the data plane, over a reader that answers a description, one that
/// answers a page of items and one that destroys the table.
fn clause_readers(table: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("DescribeTable", json!({"TableName": table})),
        (
            "Scan",
            json!({"TableName": table, "ConsistentRead": false, "Limit": 2}),
        ),
        ("DeleteTable", json!({"TableName": table})),
    ]
}

#[tokio::test]
async fn the_clause_is_the_answer_whichever_form_the_absence_takes() {
    let router = emulator();

    // The clause is about the member carrying no VALUE, so the same answer is owed whichever
    // way the body states that — and an explicit `null` is the same absence as the member
    // left out, because a `null` is how a client marshals a member it is not stating.
    for (operation, well_formed) in clause_readers("Readers") {
        let mut nulled = well_formed.clone();
        nulled["TableName"] = Value::Null;
        let mut stripped = well_formed.clone();
        if let Some(members) = stripped.as_object_mut() {
            members.remove("TableName");
        }
        for (form, request) in [
            ("an empty body", json!({})),
            ("its other members and no table name", stripped),
            ("the table name stated null", nulled),
        ] {
            let case = format!("{operation} with {form}");
            let (code, stated) = refusal(&router, &case, operation, request).await;
            assert_eq!(
                (code.as_str(), stated.as_str()),
                ("ValidationException", ABSENT_TABLE_NAME),
                "case {case:?}: expected the walk's counted clause over 'tableName', got {code} \
                 with {stated:?}"
            );
            // The path a client reads out of the message is the lowerCamel one, so the
            // PascalCase name the shape spells appears nowhere in it, and neither does the
            // parameter-required sentence the other operations state.
            assert!(
                !stated.contains("TableName") && !stated.contains("is required but was not"),
                "case {case:?}: expected the lowerCamel path alone, got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_table_name_the_request_states_is_not_an_absent_member() {
    let router = emulator();
    create_active_table(&router, "Readers", &[("pk", "HASH", "S")]).await;
    ok(
        &router,
        "PutItem",
        json!({"TableName": "Readers", "Item": {"pk": {"S": "one"}}}),
    )
    .await;

    // The boundary of the same rule: a value the request DID state is not an absence, so
    // what it earns is the bound that value broke — the `@length` minimum the member
    // declares — and never the absent-member clause.
    for (operation, _) in clause_readers("Readers") {
        let case = format!("{operation} with an empty table name");
        let (code, stated) = refusal(&router, &case, operation, json!({"TableName": ""})).await;
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected the constraint class, got {code} with {stated:?}"
        );
        assert!(
            stated.contains(
                "failed to satisfy constraint: Member must have length greater than or equal to 1"
            ) && !stated.contains("must not be null"),
            "case {case:?}: expected the bound the stated value broke, got {stated:?}"
        );
    }

    // And a request that STATES the member is served, so the refusals above are about the
    // absence and nothing else: the description reports that table, the walk reports the item
    // written into it, and the destruction reports the table it removed.
    let described = ok(&router, "DescribeTable", json!({"TableName": "Readers"})).await;
    assert_eq!(
        described["Table"]["TableName"],
        json!("Readers"),
        "case \"a stated table name\": expected the description of that table, got {described}"
    );
    let walked = ok(
        &router,
        "Scan",
        json!({"TableName": "Readers", "ConsistentRead": false, "Limit": 2}),
    )
    .await;
    assert_eq!(
        (walked["Count"].clone(), walked["Items"][0]["pk"]["S"].clone()),
        (json!(1), json!("one")),
        "case \"a stated table name\": expected the item the write stored, got {walked}"
    );
    let deleted = ok(&router, "DeleteTable", json!({"TableName": "Readers"})).await;
    assert_eq!(
        deleted["TableDescription"]["TableName"],
        json!("Readers"),
        "case \"a stated table name\": expected the destruction of that table, got {deleted}"
    );
}

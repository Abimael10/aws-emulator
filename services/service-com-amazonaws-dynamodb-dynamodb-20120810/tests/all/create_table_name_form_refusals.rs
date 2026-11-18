// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FORM a `CreateTable` holds its `TableName` to.
//!
//! The rules pinned here:
//!
//! 1. `TableName` targets the shape that also carries a table's ARN — "You can also provide
//!    the Amazon Resource Name (ARN) of the table in this parameter"
//!    (`CreateTable$TableName`) — so the bound the model declares on the member is wider
//!    than the form a NAME takes. A request CREATING a table states the name it will be
//!    addressed by from then on, and that name is held to its own form: 3 to 255
//!    characters of `[a-zA-Z0-9_.-]`;
//! 2. a name is held to EVERY rule of that form rather than refused by the first: one
//!    submitted value can break two at once — a value both outside the length and outside
//!    the pattern — and each is a clause of the one counted envelope, in the order a name
//!    is read (its length, then the characters it is spelt with);
//! 3. such a refusal states nothing of any OTHER member: the name is the whole selector the
//!    created table will be addressed by. A request whose name takes the form keeps the
//!    clause for whatever member IS at fault;
//! 4. a name at either bound of the form is served.
//!
//! Every test drives `CreateTable` over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.CreateTable`) through the same `MultiServiceRouter` the
//! emulator's front doors use, and each rule is asserted over a SET of inputs — how the
//! name breaks the form, how many rules it breaks at once, and what else the request gets
//! wrong — rather than one example.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// The form's own bounds, as the service states them: a table name is 3 to 255 characters
/// of this expression (`crate::emulator`'s recorded refusals of that member carry all
/// three, and the model declares none of them — `smithy_query(query="shape",
/// shape="TableArn")` gives `@length` 1..1024 and no pattern).
const MIN_NAME: usize = 3;
const MAX_NAME: usize = 255;
const NAME_PATTERN: &str = "[a-zA-Z0-9_.-]+";

/// The clause one broken rule of the form earns, over the name the request sent.
fn clause(name: &str, requirement: &str) -> String {
    format!("Value '{name}' at 'tableName' failed to satisfy constraint: {requirement}")
}

/// The whole envelope over `clauses`, counted as this service counts them.
fn envelope(clauses: &[String]) -> String {
    let plural = if clauses.len() == 1 { "error" } else { "errors" };
    format!(
        "{} validation {plural} detected: {}",
        clauses.len(),
        clauses.join("; ")
    )
}

/// The requirement phrase for the form's minimum length.
fn too_short() -> String {
    format!("Member must have length greater than or equal to {MIN_NAME}")
}

/// The requirement phrase for its maximum length.
fn too_long() -> String {
    format!("Member must have length less than or equal to {MAX_NAME}")
}

/// The requirement phrase for the characters a name is spelt with.
fn bad_characters() -> String {
    format!("Member must satisfy regular expression pattern: {NAME_PATTERN}")
}

/// A `CreateTable` body naming `name`, otherwise well formed.
fn create_body(name: &str) -> Value {
    json!({
        "TableName": name,
        "BillingMode": "PAY_PER_REQUEST",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
    })
}

/// Drive `CreateTable` with `body` and answer the (status, error code, message) triple a
/// refusal is read as.
async fn refusal(router: &MultiServiceRouter, body: Value) -> (u16, String, String) {
    let (status, parsed) = call(router, "CreateTable", body).await;
    (status, error_code(&parsed), message(&parsed))
}

/// RULES 1 and 2: rows differ in which rules of the form the name breaks and how many at
/// once.
#[tokio::test]
async fn a_created_tables_name_is_refused_by_every_rule_of_its_form_it_breaks() {
    let router = emulator();
    let shortest = "x".repeat(MIN_NAME - 1);
    let longest = "x".repeat(MAX_NAME + 1);
    // A name past the maximum that is ALSO outside the pattern: two independent rules of
    // one value.
    let long_and_spelt_badly = format!("{} oops!", "x".repeat(MAX_NAME));
    let cases: [(&str, &str, Vec<String>); 5] = [
        (
            "no name at all",
            "",
            // The empty value is shorter than the minimum AND satisfies the expression in
            // no way, because the expression admits one or more characters.
            vec![too_short(), bad_characters()],
        ),
        (
            "one character short of the minimum",
            &shortest,
            vec![too_short()],
        ),
        (
            "one character past the maximum",
            &longest,
            vec![too_long()],
        ),
        (
            "a character the form does not admit",
            "bad name!",
            vec![bad_characters()],
        ),
        (
            "past the maximum and outside the pattern at once",
            &long_and_spelt_badly,
            vec![too_long(), bad_characters()],
        ),
    ];

    for (case, name, requirements) in cases {
        let expected: Vec<String> = requirements
            .iter()
            .map(|requirement| clause(name, requirement))
            .collect();
        let answered = refusal(&router, create_body(name)).await;
        assert_eq!(
            answered,
            (
                400,
                "ValidationException".to_string(),
                envelope(&expected)
            ),
            "case {case:?}: expected {} clause(s) of the name's form, got {answered:?}",
            expected.len()
        );
    }
}

/// RULE 3: a name at fault states its own clauses alone, while a name that takes the form
/// leaves the clause to whichever member IS at fault.
#[tokio::test]
async fn a_name_outside_its_form_states_nothing_of_any_other_member() {
    let router = emulator();
    // The same second defect in both rows — a capacity mode outside the set the model
    // declares — so what differs is only whether the NAME takes its form.
    let mut malformed_name = create_body("");
    malformed_name["BillingMode"] = json!("BOGUS_MODE");
    let mut malformed_mode = create_body("NameThatTakesTheForm");
    malformed_mode["BillingMode"] = json!("BOGUS_MODE");

    let answered = refusal(&router, malformed_name).await;
    assert_eq!(
        answered,
        (
            400,
            "ValidationException".to_string(),
            envelope(&[clause("", &too_short()), clause("", &bad_characters())])
        ),
        "case \"a name at fault beside another member\": expected the name's clauses alone, got \
         {answered:?}"
    );

    let (status, code, stated) = refusal(&router, malformed_mode).await;
    assert_eq!(
        (status, code.as_str()),
        (400, "ValidationException"),
        "case \"a name that takes the form\": expected a 400 ValidationException, got {status} \
         with {stated}"
    );
    assert_eq!(
        stated,
        envelope(&[
            "Value 'BOGUS_MODE' at 'billingMode' failed to satisfy constraint: Member must \
             satisfy enum value set: [PROVISIONED, PAY_PER_REQUEST]"
                .to_string()
        ]),
        "case \"a name that takes the form\": expected the capacity mode's own clause, got \
         {stated:?}"
    );
}

/// RULE 4: either bound of the form is a name a table is created under.
#[tokio::test]
async fn a_name_at_either_bound_of_the_form_is_served() {
    let router = emulator();
    for (case, name) in [
        ("the shortest name the form admits", "x".repeat(MIN_NAME)),
        ("the longest it admits", "x".repeat(MAX_NAME)),
        ("every character class it admits", "aZ9_.-name".to_string()),
    ] {
        let created = ok(&router, "CreateTable", create_body(&name)).await;
        assert_eq!(
            created["TableDescription"]["TableName"],
            json!(name),
            "case {case:?}: expected the table to be created under the name it was given, got \
             {created}"
        );
        let described = describe_when_active(&router, case, &name).await;
        assert_eq!(
            described["TableName"],
            json!(name),
            "case {case:?}: expected the created table to be read back, got {described}"
        );
    }
}

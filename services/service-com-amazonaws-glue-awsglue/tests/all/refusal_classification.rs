// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW a refused Glue request is CLASSIFIED and RENDERED: which error
//! code answers it, and what its message says.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rules under test, each over a table of inputs rather than one request:
//!
//! * a value outside a MODELLED constraint is the constraint refusal — HTTP 400
//!   `ValidationException`, whose lowercase `message` carries AWS's
//!   `N validation error(s) detected: …` envelope — and that envelope names the member by
//!   the path Glue's own API spells (a member carrying a resource DEFINITION is named for
//!   the resource it carries, `database.name` and not `databaseInput.name`), quotes the
//!   value the caller submitted, and quotes a `@pattern` WITHOUT the anchoring pair the
//!   model wraps it in;
//! * a value Glue's OWN input validation rejects is that operation's modelled
//!   `InvalidInputException` instead, carrying the sentence this service words that check
//!   with — the catalog surface's plain sentence, or the business-metadata surface's JSON
//!   document;
//! * a create for a resource that already exists is `AlreadyExistsException` whose message
//!   names the record that already exists, read off the store.
//!
//! The rows differ along what each rule generalises over: which member carries the value,
//! which operation read it, which constraint class the value broke, how long the value is
//! (including past the framework walk's own echo ceiling), and how many resources a refusal
//! has to name.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::wire::{call, constraint_message, emulator, error_code, error_message, ok, ACCOUNT_ID};

/// `NameString`'s modelled `@length` maximum ("length 1..255",
/// `smithy_query(query="shape",shape="NameString")`), which every database-name and
/// table-name member below targets. Written here independently of the handler, so the suite
/// pins the bound and not the code.
const MAX_NAME_CHARS: usize = 255;

/// `DescriptionString`'s modelled `@length` maximum ("length 0..2048",
/// `smithy_query(query="shape",shape="DescriptionString")`), which `DatabaseInput$Description`
/// targets.
const MAX_DESCRIPTION_CHARS: usize = 2048;

/// `UUIDv4`'s modelled `@pattern`, as the model writes it
/// (`smithy_query(query="operation",operation="GetMaterializedViewRefreshTaskRun")`). A
/// refusal quotes it without the anchoring pair, which is what [`stated_pattern`] states.
const RUN_ID_PATTERN: &str =
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$";

/// `GlueVersionString`'s modelled `@pattern`, as the model writes it
/// (`smithy_query(query="shape",shape="GlueVersionString")`) — a second member, in another
/// shape, whose refusal the same rule covers.
const GLUE_VERSION_PATTERN: &str = r"^((\w+\.)+\w+|auto)$";

/// `FormTypeName`'s modelled `@pattern`, as the model writes it
/// (`smithy_query(query="shape",shape="FormTypeName")`). The business-metadata surface
/// quotes it as the model declares it, anchors included.
const FORM_TYPE_NAME_PATTERN: &str = r"^[A-Z]\w*$";

/// The IAM role a column statistics settings create names.
const ROLE: &str = "arn:aws:iam::000000000000:role/glue-column-stats-role";

/// A `@pattern` as a constraint clause quotes it: the expression the service advertises,
/// which is the model's text without the whole-value anchoring pair.
fn stated_pattern(pattern: &str) -> String {
    ::service_common::validation::unanchored(pattern)
}

/// The largest value a clause SERVED STRAIGHT OFF THE ROUTING TABLE quotes in full; past it
/// the request walk quotes a prefix of that many bytes plus `... [<n> bytes]`, which
/// `service_common::validation::quoted_echo_up_to` renders exactly when handed this ceiling.
/// The operations this module covers do NOT see it — their refusals are rebuilt by the
/// service's own catalog-name validation, which restores a submitted value whole (the clauses
/// below assert that) — so the two ceilings are not in conflict: this one is what an
/// operation refused by the walk alone carries, which is what
/// [`crate::integration_resource_property_delete_misses`] asserts. Declared beside
/// [`one_clause`] so the envelope and the bound on what it quotes live together.
pub(crate) const MAX_ECHOED_VALUE: usize = 256;

/// The whole message a constraint refusal of ONE member carries: AWS's envelope over the
/// one clause that fired, naming the value submitted, the member path the API spells and the
/// requirement the value broke. Shared with the suites that assert one such refusal
/// (`crate::integration_resource_property_delete_misses`), so the envelope has one spelling.
pub(crate) fn one_clause(value: &str, member_path: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member_path}' failed to satisfy \
         constraint: {requirement}"
    )
}

/// Assert that `body` is the constraint refusal carrying exactly `expected`.
fn assert_constraint_refusal(case: &str, status: u16, body: &Value, expected: &str) {
    assert_eq!(
        status, 400,
        "case {case}: a value outside a modelled constraint is refused with 400, got \
         {status} with {body}"
    );
    assert!(
        error_code(body).ends_with("ValidationException"),
        "case {case}: expected the validator's ValidationException, got {} in {body}",
        error_code(body)
    );
    assert_eq!(
        constraint_message(body),
        expected,
        "case {case}: unexpected constraint wording"
    );
}

// ---------------------------------------------------------------------------
// The member path a constraint clause names, and the value it quotes
// ---------------------------------------------------------------------------

/// A value past its member's `@length` maximum is refused in AWS's constraint envelope,
/// naming the member by the path Glue's own API spells and quoting the value the caller
/// submitted whole — at any length, the framework walk's own echo ceiling included.
///
/// The rows differ along which operation carried the value, whether the member carries a
/// resource DEFINITION (so the path is the resource, not the wire member name) or addresses
/// a resource, which bound was broken, and how long the submitted value is.
#[tokio::test]
async fn a_constraint_clause_names_the_api_request_path_and_quotes_the_whole_value() {
    let router = emulator();

    // One row: the operation, how its request carries the offending value, the member path
    // the clause must name, and the bound the value must break.
    struct Row {
        case: &'static str,
        operation: &'static str,
        /// The request, built around the offending value.
        request: fn(&str) -> Value,
        member_path: &'static str,
        bound: usize,
    }

    let rows = vec![
        Row {
            case: "CreateDatabase's DatabaseInput.Name",
            operation: "CreateDatabase",
            request: |value| json!({ "DatabaseInput": { "Name": value } }),
            member_path: "database.name",
            bound: MAX_NAME_CHARS,
        },
        Row {
            case: "UpdateDatabase's DatabaseInput.Name",
            operation: "UpdateDatabase",
            request: |value| {
                json!({ "Name": "constraint_db", "DatabaseInput": { "Name": value } })
            },
            member_path: "database.name",
            bound: MAX_NAME_CHARS,
        },
        Row {
            case: "CreateDatabase's DatabaseInput.Description",
            operation: "CreateDatabase",
            request: |value| {
                json!({ "DatabaseInput": { "Name": "constraint_db", "Description": value } })
            },
            member_path: "database.description",
            bound: MAX_DESCRIPTION_CHARS,
        },
        Row {
            case: "GetDatabase's Name",
            operation: "GetDatabase",
            request: |value| json!({ "Name": value }),
            member_path: "name",
            bound: MAX_NAME_CHARS,
        },
        Row {
            case: "DeleteDatabase's Name",
            operation: "DeleteDatabase",
            request: |value| json!({ "Name": value }),
            member_path: "name",
            bound: MAX_NAME_CHARS,
        },
        Row {
            case: "CreateTable's TableInput.Name",
            operation: "CreateTable",
            request: |value| {
                json!({ "DatabaseName": "constraint_db", "TableInput": { "Name": value } })
            },
            member_path: "table.name",
            bound: MAX_NAME_CHARS,
        },
        Row {
            case: "UpdateTable's TableInput.Name",
            operation: "UpdateTable",
            request: |value| {
                json!({ "DatabaseName": "constraint_db", "TableInput": { "Name": value } })
            },
            member_path: "table.name",
            bound: MAX_NAME_CHARS,
        },
        Row {
            case: "GetTable's Name",
            operation: "GetTable",
            request: |value| json!({ "DatabaseName": "constraint_db", "Name": value }),
            member_path: "name",
            bound: MAX_NAME_CHARS,
        },
        Row {
            case: "DeleteTable's Name",
            operation: "DeleteTable",
            request: |value| json!({ "DatabaseName": "constraint_db", "Name": value }),
            member_path: "name",
            bound: MAX_NAME_CHARS,
        },
        Row {
            case: "GetTables' DatabaseName",
            operation: "GetTables",
            request: |value| json!({ "DatabaseName": value }),
            member_path: "databaseName",
            bound: MAX_NAME_CHARS,
        },
    ];

    // A database the rows above address, so a row is refused for its over-long value and
    // not for a database that does not exist.
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "constraint_db" } }),
    )
    .await;

    for row in &rows {
        // One character past the bound, and two lengths well past the framework walk's own
        // 256-byte echo ceiling, which is where a summarised echo would show.
        for over in [1usize, 255, 765] {
            let value = "d".repeat(row.bound + over);
            let case = format!("{} at {} characters", row.case, value.chars().count());
            let (status, body) = call(&router, row.operation, &(row.request)(&value)).await;
            assert_constraint_refusal(
                &case,
                status,
                &body,
                &one_clause(
                    &value,
                    row.member_path,
                    &format!(
                        "Member must have length less than or equal to {}",
                        row.bound
                    ),
                ),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The expression a @pattern clause quotes
// ---------------------------------------------------------------------------

/// A value outside its member's `@pattern` is refused in the same envelope, and the
/// expression is quoted WITHOUT the anchoring pair the model wraps it in — the text AWS
/// publishes for that member.
///
/// The rows differ along which member and which expression: an identifier that has to be a
/// UUID, and the version string of a job definition, whose expression carries a top-level
/// alternation. Each is driven with several values that miss the expression in different
/// ways.
#[tokio::test]
async fn a_pattern_clause_quotes_the_expression_without_its_anchors() {
    let router = emulator();

    // (case, the id the request carries). Each value misses `UUIDv4`'s expression in its
    // own way: not a UUID at all (the recorded case), too few groups, a non-hex character,
    // a group of the wrong width, and one with the separators left out.
    let run_ids = [
        ("not a UUID at all", "not-a-valid-uuid"),
        ("too few groups", "0a0b0c0d-0e0f-0a0b-0c0d"),
        (
            "a non-hex character",
            "0a0b0c0g-0e0f-0a0b-0c0d-0e0f0a0b0c0d",
        ),
        (
            "a group of the wrong width",
            "0a0b0c0d-0e0f0-0a0b-0c0d-0e0f0a0b0c0d",
        ),
        (
            "the separators left out",
            "0a0b0c0d0e0f0a0b0c0d0e0f0a0b0c0d0e0f",
        ),
    ];
    for (case, run_id) in run_ids {
        let (status, body) = call(
            &router,
            "GetMaterializedViewRefreshTaskRun",
            &json!({ "CatalogId": ACCOUNT_ID, "MaterializedViewRefreshTaskRunId": run_id }),
        )
        .await;
        assert_constraint_refusal(
            case,
            status,
            &body,
            &one_clause(
                run_id,
                "materializedViewRefreshTaskRunId",
                &format!(
                    "Member must satisfy regular expression pattern: {}",
                    stated_pattern(RUN_ID_PATTERN)
                ),
            ),
        );
    }

    // The same rule on another member, whose expression alternates at the top level: the
    // pair is dropped and nothing else of the expression changes.
    for (case, version) in [
        ("no dot at all", "4"),
        ("a trailing dot", "4."),
        ("a word that only begins like the literal", "automatic"),
    ] {
        let (status, body) = call(
            &router,
            "CreateJob",
            &json!({
                "Name": "pattern-job",
                "Role": ROLE,
                "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl/x.py" },
                "GlueVersion": version,
            }),
        )
        .await;
        assert_constraint_refusal(
            case,
            status,
            &body,
            &one_clause(
                version,
                "glueVersion",
                &format!(
                    "Member must satisfy regular expression pattern: {}",
                    stated_pattern(GLUE_VERSION_PATTERN)
                ),
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// The members Glue's own validation judges
// ---------------------------------------------------------------------------

/// A catalog `Name` carrying a character the catalog surface forbids is refused with the
/// operation's own `InvalidInputException` and the catalog's fixed sentence for that check —
/// never with a constraint-violation report quoting a member path and an expression — and
/// nothing is stored. A name the surface admits is stored and read back.
///
/// The rows differ along which forbidden character the name carries and where it carries it.
#[tokio::test]
async fn a_catalog_name_the_surface_forbids_earns_the_catalogs_own_sentence() {
    let router = emulator();

    // Written here independently of the handler, so the suite pins the wording.
    const NOT_ALLOWED: &str =
        "Only lowercase letters, numbers, underscores and hyphens are allowed for catalog name";

    for (case, name, read_code) in [
        ("a dot inside the name", "my.catalog", "ValidationException"),
        ("a dot at the end", "catalog.", "ValidationException"),
        ("a dot at the start", ".catalog", "ValidationException"),
        // The one value of this table that is a well-formed catalog ADDRESS: a forward
        // slash nests one catalog name under another, so the read resolves it.
        (
            "a forward slash",
            "s3tablescatalog/analytics",
            "EntityNotFoundException",
        ),
        ("a backslash", "my\\catalog", "ValidationException"),
        ("the reserved prefix", "aws:catalog", "ValidationException"),
    ] {
        let (status, body) = call(
            &router,
            "CreateCatalog",
            &json!({
                "Name": name,
                "CatalogInput": {
                    "FederatedCatalog": { "ConnectionName": "aws:redshift", "Identifier": "x" },
                },
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400 for {name:?}, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case}: expected InvalidInputException for {name:?}, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            NOT_ALLOWED,
            "case {case}: unexpected wording for {name:?}"
        );

        // The same value as a catalog ID is an ADDRESS: each name between the nesting
        // separators is a catalog name, so a value that is one such name and is outside the
        // grammar is a malformed address (the constraint report `tests/all/
        // catalog_id_grammar.rs` pins), while the nested form resolves and reports the
        // entity the store does not hold.
        let (status, body) = call(&router, "GetCatalog", &json!({ "CatalogId": name })).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400 reading {name:?}, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with(read_code),
            "case {case}: expected {read_code} reading {name:?}, got {} in {body}",
            error_code(&body)
        );
    }

    // A name the surface admits still reaches the create, so the refusals above are
    // decisions and not a blanket.
    ok(
        &router,
        "CreateCatalog",
        &json!({
            "Name": "admitted-catalog_1",
            "CatalogInput": {
                "FederatedCatalog": { "ConnectionName": "aws:redshift", "Identifier": "x" },
            },
        }),
    )
    .await;
    let read = ok(
        &router,
        "GetCatalog",
        &json!({ "CatalogId": "admitted-catalog_1" }),
    )
    .await;
    assert_eq!(
        read["Catalog"]["Name"].as_str(),
        Some("admitted-catalog_1"),
        "a name the surface admits must be stored and read back, got {read}"
    );
}

/// A form type `Name` outside what `FormTypeName` admits is refused with `PutFormType`'s own
/// `InvalidInputException`, never with the cross-service `ValidationException`, and its
/// message is the business-metadata surface's JSON document carrying AWS's constraint report
/// — the value withheld and the expression quoted as the model declares it. A name the
/// member admits is stored and read back.
///
/// The rows differ along how the name misses the member's requirements: where the character
/// outside the expression sits, and a name past the member's length maximum.
#[tokio::test]
async fn a_form_type_name_outside_the_member_earns_the_surfaces_own_document() {
    let router = emulator();

    let too_long = format!("D{}", "e".repeat(128));
    // (case, the name sent, the requirements the report must state in order).
    let rows: Vec<(&str, String, Vec<String>)> = vec![
        (
            "a lower-case first letter",
            "lowercasename".to_string(),
            vec![pattern_requirement()],
        ),
        (
            "a digit first",
            "1Name".to_string(),
            vec![pattern_requirement()],
        ),
        (
            "an underscore first",
            "_Name".to_string(),
            vec![pattern_requirement()],
        ),
        (
            "a hyphen inside",
            "Form-Type".to_string(),
            vec![pattern_requirement()],
        ),
        (
            "a space inside",
            "Form Type".to_string(),
            vec![pattern_requirement()],
        ),
        (
            "the empty string",
            String::new(),
            vec![
                "Member must have length greater than or equal to 1".to_string(),
                pattern_requirement(),
            ],
        ),
        (
            "one character past the length maximum",
            too_long,
            vec!["Member must have length less than or equal to 128".to_string()],
        ),
    ];

    for (case, name, requirements) in &rows {
        let (status, body) = call(
            &router,
            "PutFormType",
            &json!({ "Name": name, "Schema": "structure S { a: String }" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400 for {name:?}, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case}: expected InvalidInputException for {name:?}, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            document(&constraint_report(requirements)),
            "case {case}: unexpected wording for {name:?}"
        );
    }

    // A name the member admits is stored, so the refusals above are decisions and not a
    // blanket.
    ok(
        &router,
        "PutFormType",
        &json!({ "Name": "Admitted_1", "Schema": "structure S { a: String }" }),
    )
    .await;
    let read = ok(
        &router,
        "GetFormType",
        &json!({ "Identifier": "Admitted_1" }),
    )
    .await;
    assert_eq!(
        read["Name"].as_str(),
        Some("Admitted_1"),
        "a name the member admits must be stored and read back, got {read}"
    );
}

/// The `@pattern` requirement a form type name report states: the expression as the model
/// declares it, anchors included, which is this surface's own quoting.
fn pattern_requirement() -> String {
    format!("Member must satisfy regular expression pattern: {FORM_TYPE_NAME_PATTERN}")
}

/// AWS's constraint report over the requirements one member broke, with the value WITHHELD:
/// the envelope counts the clauses and each clause names the member and its requirement.
fn constraint_report(requirements: &[String]) -> String {
    let clauses: Vec<String> = requirements
        .iter()
        .map(|requirement| format!("Value at 'name' failed to satisfy constraint: {requirement}"))
        .collect();
    let detected = if clauses.len() == 1 {
        "1 validation error detected".to_string()
    } else {
        format!("{} validation errors detected", clauses.len())
    };
    format!("{detected}: {}", clauses.join("; "))
}

/// A sentence as the business-metadata surface puts it on the wire: a JSON document carrying
/// it under a `Message` key. Written here independently of the service, so the suite pins
/// the shape.
fn document(sentence: &str) -> String {
    serde_json::json!({ "Message": sentence }).to_string()
}

/// The message a duplicate glossary TERM name is refused with: this surface's document
/// carrying a sentence that names the term's name and the glossary it was asked for in,
/// since a term's name identifies it within one glossary and is free in every other.
/// Written here independently of the service, so the suite pins the wording.
fn duplicate_term_message(glossary: &str, name: &str) -> String {
    document(&format!(
        "A glossary term with name '{name}' already exists in glossary '{glossary}'."
    ))
}

// ---------------------------------------------------------------------------
// What an already-exists refusal names
// ---------------------------------------------------------------------------

/// A second `CreateGlossaryTerm` for a name the addressed glossary already holds is refused
/// with `AlreadyExistsException` whose message is this surface's JSON document naming the
/// term the write asked for and the glossary it asked for it in — and the same refusal is
/// what the loser of a RACE between simultaneous creates is answered with, exactly one of
/// which wins.
///
/// The rows differ along the form and length of the term's name, and along how many creates
/// are in flight at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_duplicate_glossary_term_names_the_term_in_the_surfaces_document() {
    let router = Arc::new(emulator());
    let glossary = ok(&router, "CreateGlossary", &json!({ "Name": "terms" })).await;
    let glossary_id = glossary["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateGlossary must mint an Id, got {glossary}"))
        .to_string();

    // (case, the term name) — the rule generalises over the name, which the refusal quotes.
    let names = [
        ("a plain name", "revenue".to_string()),
        ("a single character", "x".to_string()),
        ("mixed case with punctuation", "Net.Revenue_2026".to_string()),
        ("at the modelled length maximum", "n".repeat(256)),
    ];

    for (case, name) in &names {
        ok(
            &router,
            "CreateGlossaryTerm",
            &json!({ "GlossaryIdentifier": &glossary_id, "Name": name }),
        )
        .await;
        let (status, body) = call(
            &router,
            "CreateGlossaryTerm",
            &json!({ "GlossaryIdentifier": &glossary_id, "Name": name }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a duplicate term name must be refused with 400, got {status} with \
             {body}"
        );
        assert!(
            error_code(&body).ends_with("AlreadyExistsException"),
            "case {case}: expected AlreadyExistsException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            duplicate_term_message(&glossary_id, name),
            "case {case}: unexpected wording for the duplicate of {name:?}"
        );
    }

    // The same name in ANOTHER glossary is a different term and is free, so the refusals
    // above are about the collision and not about the name.
    let other = ok(&router, "CreateGlossary", &json!({ "Name": "other-terms" })).await;
    let other_id = other["Id"].as_str().expect("the second glossary has an Id");
    ok(
        &router,
        "CreateGlossaryTerm",
        &json!({ "GlossaryIdentifier": other_id, "Name": "revenue" }),
    )
    .await;

    // Simultaneous creates of one name: exactly one wins, and every loser is answered with
    // the same refusal the serialized duplicate above earns.
    for racers in [2usize, 8] {
        let case = format!("{racers} creates of one term name at once");
        let name = format!("raced-{racers}");
        let mut racing = Vec::new();
        for index in 0..racers {
            let router = Arc::clone(&router);
            let glossary_id = glossary_id.clone();
            let name = name.clone();
            racing.push(tokio::spawn(async move {
                call(
                    &router,
                    "CreateGlossaryTerm",
                    &json!({
                        "GlossaryIdentifier": glossary_id,
                        "Name": name,
                        "ShortDescription": format!("racer-{index}"),
                    }),
                )
                .await
            }));
        }

        let mut winners = 0usize;
        for racer in racing {
            let (status, body) = racer.await.expect("a racing create must not panic");
            if status == 200 {
                winners += 1;
                continue;
            }
            assert_eq!(
                status, 400,
                "case {case}: a refused create answers 400, got {status} with {body}"
            );
            assert!(
                error_code(&body).ends_with("AlreadyExistsException"),
                "case {case}: a loser must be refused with AlreadyExistsException, got {} in \
                 {body}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                duplicate_term_message(&glossary_id, &name),
                "case {case}: unexpected wording for the loser"
            );
        }
        assert_eq!(
            winners, 1,
            "case {case}: exactly one create must win the race"
        );

        // And the state matches: the glossary holds ONE term under that name.
        let listed = ok(
            &router,
            "ListGlossaryTerms",
            &json!({ "GlossaryIdentifier": &glossary_id }),
        )
        .await;
        let held = listed["Items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter(|item| item["Name"].as_str() == Some(name.as_str()))
                    .count()
            })
            .unwrap_or_default();
        assert_eq!(
            held, 1,
            "case {case}: exactly one term must be stored under {name:?}, got {listed}"
        );
    }
}

/// A second `CreateColumnStatisticsTaskSettings` for a table whose settings a create already
/// wrote is refused with `AlreadyExistsException` whose message names the record that already
/// exists: the catalog holding it, and the database and table the settings were written for.
///
/// The rows differ along the names of the database and the table the settings belong to.
#[tokio::test]
async fn duplicate_column_statistics_settings_name_the_record_that_exists() {
    let router = emulator();

    for (case, database, table) in [
        ("plain names", "stats_db", "events"),
        ("names carrying underscores and digits", "stats_db_2", "ev_2"),
    ] {
        ok(
            &router,
            "CreateDatabase",
            &json!({ "DatabaseInput": { "Name": database } }),
        )
        .await;
        ok(
            &router,
            "CreateTable",
            &json!({
                "DatabaseName": database,
                "TableInput": { "Name": table, "TableType": "EXTERNAL_TABLE" },
            }),
        )
        .await;
        let settings = json!({
            "DatabaseName": database,
            "TableName": table,
            "Role": ROLE,
            "Schedule": "cron(0 1 * * ? *)",
        });
        ok(&router, "CreateColumnStatisticsTaskSettings", &settings).await;

        let (status, body) = call(&router, "CreateColumnStatisticsTaskSettings", &settings).await;
        assert_eq!(
            status, 400,
            "case {case}: a second create must be refused with 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("AlreadyExistsException"),
            "case {case}: expected AlreadyExistsException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            format!("{ACCOUNT_ID}:{database}-{table} already exists"),
            "case {case}: unexpected wording for the duplicate settings"
        );

        // The refused create left the stored settings exactly as the first create wrote
        // them.
        let read = ok(
            &router,
            "GetColumnStatisticsTaskSettings",
            &json!({ "DatabaseName": database, "TableName": table }),
        )
        .await;
        assert_eq!(
            read["ColumnStatisticsTaskSettings"]["Schedule"]["ScheduleExpression"].as_str(),
            Some("cron(0 1 * * ? *)"),
            "case {case}: the stored settings must be the first create's, got {read}"
        );
    }
}

/// Every refusal this suite pins keeps its own CODE: the constraint envelope travels with
/// `ValidationException`, Glue's own input validation with `InvalidInputException`, and a
/// collision with `AlreadyExistsException` — one table, so no two of them can drift onto one
/// code.
#[tokio::test]
async fn each_refusal_class_keeps_its_own_error_code() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "classes_db" } }),
    )
    .await;

    // (case, operation, request, the code the class answers with).
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "a name past its member's maximum",
            "GetDatabase",
            json!({ "Name": "d".repeat(MAX_NAME_CHARS + 1) }),
            "ValidationException",
        ),
        (
            "an id outside its member's pattern",
            "GetMaterializedViewRefreshTaskRun",
            json!({ "CatalogId": ACCOUNT_ID, "MaterializedViewRefreshTaskRunId": "nope" }),
            "ValidationException",
        ),
        (
            "a form type name Glue judges itself",
            "PutFormType",
            json!({ "Name": "lowercase", "Schema": "structure S { a: String }" }),
            "InvalidInputException",
        ),
        (
            "a ResourceArn that is no taggable Glue resource",
            "TagResource",
            json!({ "ResourceArn": "not-a-valid-arn", "TagsToAdd": { "k": "v" } }),
            "InvalidInputException",
        ),
        (
            "a catalog name the surface forbids",
            "CreateCatalog",
            json!({
                "Name": "classes.catalog",
                "CatalogInput": {
                    "FederatedCatalog": { "ConnectionName": "aws:redshift", "Identifier": "x" },
                },
            }),
            "InvalidInputException",
        ),
        (
            "a database name already taken",
            "CreateDatabase",
            json!({ "DatabaseInput": { "Name": "classes_db" } }),
            "AlreadyExistsException",
        ),
    ];

    let mut seen: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (case, operation, request, code) in &rows {
        let (status, body) = call(&router, operation, request).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with(code),
            "case {case}: expected {code}, got {} in {body}",
            error_code(&body)
        );
        seen.entry(code).or_default().push(case);
    }
    assert_eq!(
        seen.len(),
        3,
        "the table must exercise all three refusal classes, got {seen:?}"
    );
}

// ---------------------------------------------------------------------------
// A member the request supplied no value for
// ---------------------------------------------------------------------------

/// `HashString`/`NameString`/`CatalogIdString`'s modelled `@pattern`, as the model writes it
/// (`smithy_query(query="shape",shape="HashString")`) — the expression every Glue id and
/// name member below targets. Written here independently of the handler, so the suite pins
/// the expression and not the code.
const ID_PATTERN: &str = r"^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$";

/// The requirement clause a value outside [`ID_PATTERN`] breaks.
fn id_pattern_requirement() -> String {
    format!(
        "Member must satisfy regular expression pattern: {}",
        stated_pattern(ID_PATTERN)
    )
}

/// A `TableOptimizerConfiguration` every table-optimizer request below carries, so the one
/// member a row leaves out is the only one missing.
fn optimizer_configuration() -> Value {
    json!({ "roleArn": "arn:aws:iam::000000000000:role/glue-table-optimizer", "enabled": true })
}

/// A `DataSource` naming a Glue table, for the data quality run start below.
fn evaluation_data_source() -> Value {
    json!({ "GlueTable": { "DatabaseName": "dq_db", "TableName": "dq_table" } })
}

/// A request that supplies NO VALUE for a member whose requirement Glue states itself — the
/// member absent, or a string supplied empty, or a list supplied empty — is refused with
/// that operation's modelled `InvalidInputException` carrying Glue's own sentence for the
/// check, never with the boundary's `ValidationException`.
///
/// The rows differ along what the rule generalises over: which operation read the request,
/// which of its required members carried no value, and HOW it carried none (absent, the
/// empty string, the empty list).
#[tokio::test]
async fn a_member_the_request_supplies_no_value_for_is_glues_own_requirement() {
    let router = emulator();

    // (case, operation, request, the sentence the refusal must carry).
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "an absent enum member of the catalog's export configuration",
            "PutDataCatalogExportConfiguration",
            json!({}),
            r#"{"Message":"ExportSetting is required."}"#.to_string(),
        ),
        (
            "an absent list member of a data quality run start",
            "StartDataQualityRulesetEvaluationRun",
            json!({ "DataSource": evaluation_data_source(), "Role": "dq-role" }),
            "At least one ruleset name must be provided.".to_string(),
        ),
        (
            "the same list member supplied EMPTY",
            "StartDataQualityRulesetEvaluationRun",
            json!({
                "DataSource": evaluation_data_source(),
                "Role": "dq-role",
                "RulesetNames": [],
            }),
            "At least one ruleset name must be provided.".to_string(),
        ),
        (
            "an absent CatalogId of an optimizer update",
            "UpdateTableOptimizer",
            json!({
                "DatabaseName": "opt_db",
                "TableName": "opt_table",
                "Type": "compaction",
                "TableOptimizerConfiguration": optimizer_configuration(),
            }),
            "CatalogId is required for UpdateTableOptimizer".to_string(),
        ),
        (
            "the same CatalogId supplied as the empty string",
            "UpdateTableOptimizer",
            json!({
                "CatalogId": "",
                "DatabaseName": "opt_db",
                "TableName": "opt_table",
                "Type": "compaction",
                "TableOptimizerConfiguration": optimizer_configuration(),
            }),
            "CatalogId is required for UpdateTableOptimizer".to_string(),
        ),
        (
            "an absent DatabaseName of an optimizer run listing",
            "ListTableOptimizerRuns",
            json!({ "CatalogId": ACCOUNT_ID, "TableName": "opt_table", "Type": "compaction" }),
            "DatabaseName is required for ListTableOptimizerRuns".to_string(),
        ),
        (
            "an absent TableName of the same listing",
            "ListTableOptimizerRuns",
            json!({ "CatalogId": ACCOUNT_ID, "DatabaseName": "opt_db", "Type": "compaction" }),
            "TableName is required for ListTableOptimizerRuns".to_string(),
        ),
        (
            "an absent Type of the same listing",
            "ListTableOptimizerRuns",
            json!({
                "CatalogId": ACCOUNT_ID,
                "DatabaseName": "opt_db",
                "TableName": "opt_table",
            }),
            "Type is required for ListTableOptimizerRuns".to_string(),
        ),
        (
            "an absent CatalogId of an optimizer read",
            "GetTableOptimizer",
            json!({ "DatabaseName": "opt_db", "TableName": "opt_table", "Type": "retention" }),
            "CatalogId is required for GetTableOptimizer".to_string(),
        ),
        (
            "an absent Type of an optimizer delete",
            "DeleteTableOptimizer",
            json!({
                "CatalogId": ACCOUNT_ID,
                "DatabaseName": "opt_db",
                "TableName": "opt_table",
            }),
            "Type is required for DeleteTableOptimizer".to_string(),
        ),
        (
            "an absent DatabaseName of an optimizer create",
            "CreateTableOptimizer",
            json!({
                "CatalogId": ACCOUNT_ID,
                "TableName": "opt_table",
                "Type": "orphan_file_deletion",
                "TableOptimizerConfiguration": optimizer_configuration(),
            }),
            "DatabaseName is required for CreateTableOptimizer".to_string(),
        ),
    ];

    for (case, operation, request, expected) in &rows {
        let (status, body) = call(&router, operation, request).await;
        assert_eq!(
            status, 400,
            "case {case}: a member with no value is refused with 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case}: expected Glue's own InvalidInputException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            *expected,
            "case {case}: unexpected wording for the missing member"
        );
    }
}

// ---------------------------------------------------------------------------
// The form a clause quotes the submitted value in
// ---------------------------------------------------------------------------

/// A constraint clause quotes the value the request carried in the form the member's
/// modelled TYPE is written in: a FLOATING-POINT member's value always carries a fractional
/// part or an exponent, where a string member's value is quoted as the text it is.
///
/// The rows differ along which bound was broken, the magnitude and sign of the submitted
/// value, whether the caller wrote it with a fractional part at all, and — on the other side
/// of the rule — a member of another type whose value is quoted unchanged.
#[tokio::test]
async fn a_clause_quotes_the_submitted_value_in_the_members_modelled_form() {
    let router = emulator();

    // `SampleSizePercentage`'s modelled bounds ("range 0..100", type double,
    // `smithy_query(query="shape",shape="SampleSizePercentage")`).
    let above = "Member must have value less than or equal to 100";
    let below = "Member must have value greater than or equal to 0";
    let over_long = "d".repeat(MAX_NAME_CHARS + 1);

    // (case, the request, the member path, the value the clause must quote, the requirement).
    let rows: Vec<(&str, Value, &str, String, &str)> = vec![
        (
            "a whole value above the maximum, written without a point",
            json!({ "DatabaseName": "s_db", "TableName": "s_t", "SampleSize": 150 }),
            "sampleSize",
            "150.0".to_string(),
            above,
        ),
        (
            "a fractional value above the maximum",
            json!({ "DatabaseName": "s_db", "TableName": "s_t", "SampleSize": 150.5 }),
            "sampleSize",
            "150.5".to_string(),
            above,
        ),
        (
            "a whole negative value below the minimum",
            json!({ "DatabaseName": "s_db", "TableName": "s_t", "SampleSize": -3 }),
            "sampleSize",
            "-3.0".to_string(),
            below,
        ),
        (
            "a magnitude small enough to be written with an exponent",
            json!({ "DatabaseName": "s_db", "TableName": "s_t", "SampleSize": -0.0001 }),
            "sampleSize",
            "-1.0E-4".to_string(),
            below,
        ),
        (
            "a STRING member, whose value is quoted as the text it is",
            json!({ "DatabaseName": over_long.clone(), "TableName": "s_t" }),
            "databaseName",
            over_long.clone(),
            "Member must have length less than or equal to 255",
        ),
    ];

    for (case, request, member_path, quoted, requirement) in &rows {
        let (status, body) = call(&router, "UpdateColumnStatisticsTaskSettings", request).await;
        assert_constraint_refusal(
            case,
            status,
            &body,
            &one_clause(quoted, member_path, requirement),
        );
    }
}

/// A constraint clause about a COLLECTION member quotes the collection the request carried —
/// the elements in order, comma-separated, in brackets — and not the walk's own count
/// summary of it.
///
/// The rows differ along how many elements the list carried past its modelled maximum and
/// how the elements themselves are spelled.
#[tokio::test]
async fn a_clause_about_a_collection_member_quotes_the_collection_submitted() {
    let router = emulator();

    // `RulesetNames`'s modelled maximum ("length 1..10",
    // `smithy_query(query="shape",shape="RulesetNames")`).
    let requirement = "Member must have length less than or equal to 10";

    // (case, the ruleset names the start carries).
    let rows: Vec<(&str, Vec<String>)> = vec![
        (
            "one name past the maximum",
            (1..=11).map(|index| format!("r{index}")).collect(),
        ),
        (
            "two names past the maximum, spelled differently",
            (1..=12)
                .map(|index| format!("ruleset-{index:02}"))
                .collect(),
        ),
        (
            "five names past the maximum, carrying dots and underscores",
            (1..=15)
                .map(|index| format!("dq.set_{index}"))
                .collect(),
        ),
    ];

    for (case, names) in &rows {
        let (status, body) = call(
            &router,
            "StartDataQualityRulesetEvaluationRun",
            &json!({
                "DataSource": evaluation_data_source(),
                "Role": "dq-role",
                "RulesetNames": names,
            }),
        )
        .await;
        assert_constraint_refusal(
            case,
            status,
            &body,
            &one_clause(
                &format!("[{}]", names.join(", ")),
                "rulesetNames",
                requirement,
            ),
        );
    }
}

/// An enum constraint clause lists the values the member admits in the order the layer in
/// front of this service enumerates them — which is neither the order the model declares
/// them in nor alphabetical order.
///
/// The rows differ along which enumerated type the member targets and which operation
/// carried it, and the three types' sets are listed in three different orders relative to
/// their declarations: one that happens to agree with alphabetical order, one that agrees
/// with neither, and one that reverses a two-value declaration.
#[tokio::test]
async fn an_enum_clause_lists_the_value_set_in_the_order_the_front_door_states_it() {
    let router = emulator();

    // (case, operation, request, the member path, the submitted value, the stated set). The
    // declared sets, as the model writes them
    // (`smithy_query(query="shape",shape="TableOptimizerType,SourceControlProvider,
    // SourceControlAuthStrategy")`), are: `compaction, retention, orphan_file_deletion`;
    // `GITHUB, GITLAB, BITBUCKET, AWS_CODE_COMMIT`; `PERSONAL_ACCESS_TOKEN,
    // AWS_SECRETS_MANAGER`.
    let rows: Vec<(&str, &str, Value, &str, &str, &str)> = vec![
        (
            "the optimizer type of a run listing",
            "ListTableOptimizerRuns",
            json!({
                "CatalogId": ACCOUNT_ID,
                "DatabaseName": "opt_db",
                "TableName": "opt_table",
                "Type": "nonsense-optimizer-type",
            }),
            "type",
            "nonsense-optimizer-type",
            "[compaction, orphan_file_deletion, retention]",
        ),
        (
            "the same type on another operation of the family",
            "GetTableOptimizer",
            json!({
                "CatalogId": ACCOUNT_ID,
                "DatabaseName": "opt_db",
                "TableName": "opt_table",
                "Type": "COMPACTION",
            }),
            "type",
            "COMPACTION",
            "[compaction, orphan_file_deletion, retention]",
        ),
        (
            "the source control provider of a job update",
            "UpdateJobFromSourceControl",
            json!({ "JobName": "sourced-job", "Provider": "NOT_A_REAL_PROVIDER" }),
            "provider",
            "NOT_A_REAL_PROVIDER",
            "[GITHUB, AWS_CODE_COMMIT, BITBUCKET, GITLAB]",
        ),
        (
            "the source control auth strategy of the same update",
            "UpdateJobFromSourceControl",
            json!({ "JobName": "sourced-job", "AuthStrategy": "NO_SUCH_STRATEGY" }),
            "authStrategy",
            "NO_SUCH_STRATEGY",
            "[AWS_SECRETS_MANAGER, PERSONAL_ACCESS_TOKEN]",
        ),
    ];

    for (case, operation, request, member_path, submitted, stated) in &rows {
        let (status, body) = call(&router, operation, request).await;
        assert_constraint_refusal(
            case,
            status,
            &body,
            &one_clause(
                submitted,
                member_path,
                &format!("Member must satisfy enum value set: {stated}"),
            ),
        );
    }
}

// ---------------------------------------------------------------------------
// A malformed address, told apart from a miss
// ---------------------------------------------------------------------------

/// A `ResultId` outside its member's `@pattern` is a MALFORMED request — the operation's
/// modelled `InvalidInputException` carrying AWS's constraint envelope — and it is refused
/// BEFORE the store is consulted, so it is never reported as a result that could not be
/// found. A value the pattern admits is a well-formed address and does reach the lookup.
///
/// The rows differ along which character outside the pattern the value carries and where it
/// carries it, and — on the other side of the rule — which characters INSIDE the pattern a
/// value carries.
#[tokio::test]
async fn a_result_id_outside_the_members_pattern_is_refused_before_the_lookup() {
    let router = emulator();

    // (case, the id the request carries). Every row carries a character below U+0020 that is
    // not a tab, or one of the two non-characters the pattern's ranges stop short of.
    let malformed: Vec<(&str, String)> = vec![
        ("newlines inside the id", "bad\nresult\nid".to_string()),
        ("a trailing carriage return", "result-id\r".to_string()),
        ("a leading newline", "\nresult-id".to_string()),
        ("a vertical tab", "result\u{000B}id".to_string()),
        ("a NUL", "result\u{0000}id".to_string()),
        (
            "the non-character just past the pattern's last admitted character",
            "result\u{FFFE}id".to_string(),
        ),
    ];

    for (case, result_id) in &malformed {
        let (status, body) = call(
            &router,
            "GetDataQualityResult",
            &json!({ "ResultId": result_id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a malformed id must be refused, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case}: expected InvalidInputException for {result_id:?}, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            one_clause(result_id, "resultId", &id_pattern_requirement()),
            "case {case}: unexpected wording for {result_id:?}"
        );
    }

    // A value the pattern admits is well formed, so it reaches the lookup and misses there.
    for (case, result_id) in [
        ("a plain id", "dqresult-0123456789"),
        ("an id carrying a tab", "dqresult\t0123"),
        ("the shortest id the model admits", "r"),
    ] {
        let (status, body) = call(
            &router,
            "GetDataQualityResult",
            &json!({ "ResultId": result_id }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: an id no run published misses, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {case}: a well-formed id must reach the lookup, got {} in {body}",
            error_code(&body)
        );
    }
}

/// An operation whose surface reports every refusal the pre-handler walk words for it as its
/// OWN modelled `InvalidInputException` answers both layers that way: the requirement this
/// service states and the modelled constraint the validator states, each inside the one JSON
/// envelope that surface wraps a refusal in, and each with the submitted value withheld.
///
/// The rows differ along which layer refused the request and which constraint class fired.
#[tokio::test]
async fn an_operations_own_refusal_envelope_covers_both_refusal_layers() {
    let router = emulator();

    // (case, the request, the refusal text the envelope must carry).
    let rows: Vec<(&str, Value, String)> = vec![
        (
            "the requirement this service states",
            json!({}),
            "ExportSetting is required.".to_string(),
        ),
        (
            "a value outside the member's enum value set",
            json!({ "ExportSetting": "MAYBE" }),
            "1 validation error detected: Value at 'exportSetting' failed to satisfy \
             constraint: Member must satisfy enum value set: [ENABLED, DISABLED]"
                .to_string(),
        ),
        (
            "a value below another member's length minimum",
            json!({ "ExportSetting": "ENABLED", "ClientToken": "" }),
            "1 validation error detected: Value at 'clientToken' failed to satisfy \
             constraint: Member must have length greater than or equal to 1"
                .to_string(),
        ),
        (
            "a value past that member's length maximum",
            json!({ "ExportSetting": "ENABLED", "ClientToken": "t".repeat(MAX_NAME_CHARS + 1) }),
            "1 validation error detected: Value at 'clientToken' failed to satisfy \
             constraint: Member must have length less than or equal to 255"
                .to_string(),
        ),
    ];

    for (case, request, text) in &rows {
        let (status, body) = call(&router, "PutDataCatalogExportConfiguration", request).await;
        assert_eq!(
            status, 400,
            "case {case}: the request is refused with 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("InvalidInputException"),
            "case {case}: expected the operation's own InvalidInputException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            json!({ "Message": text }).to_string(),
            "case {case}: unexpected refusal envelope"
        );
    }
}

/// A data quality run `RunId` that is no run id of this service is a MALFORMED request, not
/// a miss: it is refused with the operation's modelled `InvalidInputException` stating the
/// grammar a run id has, before the store is consulted. An id OF that grammar is a
/// well-formed address, so it reaches the lookup, and the id a start minted has that grammar
/// and serves its run.
///
/// The rows differ along which operation addressed the run and how the id departs from the
/// grammar.
#[tokio::test]
async fn a_run_id_outside_the_data_quality_grammar_is_refused_before_the_lookup() {
    let router = emulator();
    let stated_grammar = "Invalid RunId: must start with 'dqrun-'";

    for operation in [
        "GetDataQualityRuleRecommendationRun",
        "CancelDataQualityRuleRecommendationRun",
    ] {
        for (case, run_id) in [
            ("an id of no recognisable form", "not-a-run-id"),
            ("the prefix in the wrong case", "DQRUN-0123456789abcdef"),
            ("a prefix of the prefix", "dqru"),
            ("the prefix carried in the middle", "run-dqrun-0123"),
            ("a bare hexadecimal id", "ffffffffffffffffffffffffffffffff"),
        ] {
            let (status, body) = call(&router, operation, &json!({ "RunId": run_id })).await;
            assert_eq!(
                status, 400,
                "case {operation}/{case}: a malformed run id is refused, got {status} with {body}"
            );
            assert!(
                error_code(&body).ends_with("InvalidInputException"),
                "case {operation}/{case}: expected InvalidInputException for {run_id:?}, got {} \
                 in {body}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                stated_grammar,
                "case {operation}/{case}: unexpected wording for {run_id:?}"
            );
        }

        // A value the member's own `@pattern` rules out is the CONSTRAINT refusal instead,
        // because it is not even a value the member admits.
        let (status, body) = call(&router, operation, &json!({ "RunId": "dqrun-\nbad" })).await;
        assert_eq!(
            status, 400,
            "case {operation}: an id outside the member's pattern is refused, got {status} with \
             {body}"
        );
        assert_eq!(
            error_message(&body),
            one_clause("dqrun-\nbad", "runId", &id_pattern_requirement()),
            "case {operation}: a value outside the member's pattern states the constraint"
        );

        // An id OF the grammar is well formed, so it reaches the lookup and misses there.
        let well_formed = format!("dqrun-{}", "f".repeat(32));
        let (status, body) = call(&router, operation, &json!({ "RunId": &well_formed })).await;
        assert_eq!(
            status, 400,
            "case {operation}: an id nothing minted misses, got {status} with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {operation}: a well-formed id must reach the lookup, got {} in {body}",
            error_code(&body)
        );
    }

    // And the id a start mints HAS that grammar, so a run this service created is readable
    // through the same check.
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "grammar_db" } }),
    )
    .await;
    ok(
        &router,
        "CreateTable",
        &json!({
            "DatabaseName": "grammar_db",
            "TableInput": {
                "Name": "grammar_table",
                "TableType": "EXTERNAL_TABLE",
                "StorageDescriptor": { "Columns": [{ "Name": "a", "Type": "string" }] },
            },
        }),
    )
    .await;
    let started = ok(
        &router,
        "StartDataQualityRuleRecommendationRun",
        &json!({
            "DataSource": {
                "GlueTable": { "DatabaseName": "grammar_db", "TableName": "grammar_table" },
            },
            "Role": "arn:aws:iam::000000000000:role/glue-dq-role",
        }),
    )
    .await;
    let minted = started["RunId"]
        .as_str()
        .expect("a started run reports its RunId")
        .to_string();
    assert!(
        minted.starts_with("dqrun-"),
        "a minted run id must carry the grammar the reads require, got {minted:?}"
    );
    let read = ok(
        &router,
        "GetDataQualityRuleRecommendationRun",
        &json!({ "RunId": &minted }),
    )
    .await;
    assert_eq!(
        read["RunId"].as_str(),
        Some(minted.as_str()),
        "the minted id must serve its own run, got {read}"
    );
}

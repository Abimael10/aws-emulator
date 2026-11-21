// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateGlossaryTerm` and `UpdateUsageProfile`, the update half of the
//! glossary-term and usage-profile lifecycles.
//!
//! RULE: an update addresses the ONE resource its identifier resolves to and applies what the
//! request states — the optional members a glossary-term update NAMES, and the whole
//! `Configuration` a usage-profile update is `@required` to carry — so a later read reports
//! what the update wrote, and an identifier the account holds no such resource under (one
//! never created, or one a delete has since removed) is refused with the
//! `EntityNotFoundException` each operation models (`smithy_query
//! operation=UpdateGlossaryTerm,UpdateUsageProfile`), writing nothing.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour over state the store really holds.
//!
//! The rows differ along what the rule generalises over: which members the update names, the
//! length and form of the values it sends, and how the identifier misses (never created,
//! deleted, differing only in case).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, emulator, error_code, error_message, given_usage_profile, ok};

/// The longest `Name` an `UpdateGlossaryTerm` accepts: "len 1..256" on `GlossaryTermName`
/// (`smithy_query operation=UpdateGlossaryTerm`).
const MAX_TERM_NAME_LENGTH: usize = 256;

/// The longest `ShortDescription` it accepts: "len 1..1024" on `GlossaryShortDescription`
/// (`smithy_query operation=UpdateGlossaryTerm`).
const MAX_SHORT_DESCRIPTION_LENGTH: usize = 1024;

/// The longest `LongDescription` it accepts: "len 1..4096" on `GlossaryLongDescription`
/// (`smithy_query operation=UpdateGlossaryTerm`).
const MAX_LONG_DESCRIPTION_LENGTH: usize = 4096;

/// Create a glossary and return the `GlossaryId` the catalog minted for it.
async fn create_glossary(router: &MultiServiceRouter, name: &str) -> String {
    let created = ok(router, "CreateGlossary", &json!({ "Name": name })).await;
    created
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateGlossary must report an Id, got {created}"))
        .to_string()
}

/// Create a term in `glossary` and return the `GlossaryTermId` the catalog minted for it.
async fn create_term(router: &MultiServiceRouter, glossary: &str, name: &str) -> String {
    let created = ok(
        router,
        "CreateGlossaryTerm",
        &json!({ "GlossaryIdentifier": glossary, "Name": name }),
    )
    .await;
    created
        .get("Id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CreateGlossaryTerm must report an Id, got {created}"))
        .to_string()
}

/// The `GetGlossaryTerm` body for `identifier`, which must still resolve.
async fn get_term(router: &MultiServiceRouter, identifier: &str) -> Value {
    ok(
        router,
        "GetGlossaryTerm",
        &json!({ "Identifier": identifier }),
    )
    .await
}

/// The members a read and an update of a term both report, as the modelled output names them.
fn term_members(body: &Value) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
    (
        body.get("Id").and_then(Value::as_str),
        body.get("Name").and_then(Value::as_str),
        body.get("ShortDescription").and_then(Value::as_str),
        body.get("LongDescription").and_then(Value::as_str),
    )
}

// ---------------------------------------------------------------------------
// UpdateGlossaryTerm applies what the request names
// ---------------------------------------------------------------------------

/// An update writes the members it NAMES onto the term its `Identifier` resolves to and
/// leaves every member it omits where the create put it; the minted id and the glossary the
/// term belongs to are no members of the request, so both stand, and the body the update
/// answers with is what a later read reports.
#[tokio::test]
async fn an_update_applies_the_members_it_names() {
    let router = emulator();
    let glossary = create_glossary(&router, "term-update-glossary").await;

    let long_name = "n".repeat(MAX_TERM_NAME_LENGTH);
    let long_short = "s".repeat(MAX_SHORT_DESCRIPTION_LENGTH);
    let long_long = "l".repeat(MAX_LONG_DESCRIPTION_LENGTH);

    // One row of the glossary-term update table.
    struct TermUpdate {
        /// What this row varies, named in every assertion it drives.
        case: &'static str,
        /// The members the update sends beside the `Identifier`.
        members: Value,
        /// The `Name` the term must carry afterwards; `"Created"` stands for the name the row
        /// created the term under, which the rows that do not rename it expect back.
        name: String,
        /// The `ShortDescription` and `LongDescription` it must carry afterwards.
        short_description: Option<String>,
        long_description: Option<String>,
    }

    let cases: Vec<TermUpdate> = vec![
        TermUpdate {
            case: "the name alone",
            members: json!({ "Name": "RenamedTerm" }),
            name: "RenamedTerm".to_string(),
            short_description: None,
            long_description: None,
        },
        TermUpdate {
            case: "the short description alone",
            members: json!({ "ShortDescription": "one line" }),
            name: "Created".to_string(),
            short_description: Some("one line".to_string()),
            long_description: None,
        },
        TermUpdate {
            case: "the long description alone",
            members: json!({ "LongDescription": "several lines of context" }),
            name: "Created".to_string(),
            short_description: None,
            long_description: Some("several lines of context".to_string()),
        },
        TermUpdate {
            case: "every updatable member at once",
            members: json!({
                "Name": "FullyUpdated",
                "ShortDescription": "short",
                "LongDescription": "long",
            }),
            name: "FullyUpdated".to_string(),
            short_description: Some("short".to_string()),
            long_description: Some("long".to_string()),
        },
        TermUpdate {
            case: "values at the modelled length bounds",
            members: json!({
                "Name": long_name.clone(),
                "ShortDescription": long_short.clone(),
                "LongDescription": long_long.clone(),
            }),
            name: long_name.clone(),
            short_description: Some(long_short.clone()),
            long_description: Some(long_long.clone()),
        },
        TermUpdate {
            case: "no updatable member at all, which changes nothing",
            members: json!({}),
            name: "Created".to_string(),
            short_description: None,
            long_description: None,
        },
    ];

    for (index, row) in cases.iter().enumerate() {
        let case = row.case;
        let created = create_term(&router, &glossary, &format!("Created{index}")).await;
        // Every row starts from a term named `Created<index>`; the rows that do not rename it
        // expect that name back, so the expectation's name is read with the index applied.
        let expected_name = if row.name == "Created" {
            format!("Created{index}")
        } else {
            row.name.clone()
        };

        let mut request = row.members.clone();
        request["Identifier"] = json!(created);
        let answered = ok(&router, "UpdateGlossaryTerm", &request).await;
        assert_eq!(
            term_members(&answered),
            (
                Some(created.as_str()),
                Some(expected_name.as_str()),
                row.short_description.as_deref(),
                row.long_description.as_deref(),
            ),
            "case {case}: the update must answer with the record it wrote, got {answered}"
        );
        assert_eq!(
            answered.get("GlossaryId").and_then(Value::as_str),
            Some(glossary.as_str()),
            "case {case}: the term must keep the glossary it was created in, got {answered}"
        );

        let read = get_term(&router, &created).await;
        assert_eq!(
            term_members(&read),
            term_members(&answered),
            "case {case}: a later read must report what the update answered, got {read}"
        );
    }
}

/// A rename onto a name ANOTHER term of the same glossary carries is refused with the
/// collision the create refuses, while restating the term's own name — and taking a name a
/// term of a DIFFERENT glossary carries — is served.
#[tokio::test]
async fn a_rename_onto_a_taken_name_is_refused() {
    let router = emulator();
    let glossary = create_glossary(&router, "rename-glossary").await;
    let other_glossary = create_glossary(&router, "rename-other-glossary").await;
    let term = create_term(&router, &glossary, "Mine").await;
    create_term(&router, &glossary, "Taken").await;
    create_term(&router, &other_glossary, "Elsewhere").await;

    let (status, body) = call(
        &router,
        "UpdateGlossaryTerm",
        &json!({ "Identifier": &term, "Name": "Taken" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a name another term of this glossary carries: expected 400, got {status} with \
         {body}"
    );
    assert!(
        error_code(&body).contains("AlreadyExistsException"),
        "case a name another term of this glossary carries: expected AlreadyExistsException, \
         got {body}"
    );

    for (case, name) in [
        ("the term's own name restated", "Mine"),
        ("a name a term of another glossary carries", "Elsewhere"),
    ] {
        let answered = ok(
            &router,
            "UpdateGlossaryTerm",
            &json!({ "Identifier": &term, "Name": name }),
        )
        .await;
        assert_eq!(
            answered.get("Name").and_then(Value::as_str),
            Some(name),
            "case {case}: the update must be served, got {answered}"
        );
    }
}

/// An identifier the catalog holds no live term under is refused with the modelled miss and
/// writes nothing: the rows span an id never minted, an id a delete has removed, and an id
/// that differs from a stored one only in case.
#[tokio::test]
async fn an_update_of_an_absent_term_is_the_modelled_miss() {
    let router = emulator();
    let glossary = create_glossary(&router, "term-miss-glossary").await;
    let deleted = create_term(&router, &glossary, "Deleted").await;
    ok(
        &router,
        "DeleteGlossaryTerm",
        &json!({ "Identifier": &deleted }),
    )
    .await;
    let held = create_term(&router, &glossary, "Held").await;

    for (case, identifier) in [
        ("an id nothing minted", "term-that-never-existed".to_string()),
        ("an id a delete removed", deleted.clone()),
        ("a stored id in upper case", held.to_uppercase()),
    ] {
        let (status, body) = call(
            &router,
            "UpdateGlossaryTerm",
            &json!({ "Identifier": &identifier, "Name": "Whatever" }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {body}"
        );
    }

    // The refused updates changed nothing: the term that IS held still reports its own name.
    let read = get_term(&router, &held).await;
    assert_eq!(
        read.get("Name").and_then(Value::as_str),
        Some("Held"),
        "a refused update must write nothing, got {read}"
    );
}

// ---------------------------------------------------------------------------
// UpdateUsageProfile
// ---------------------------------------------------------------------------

/// The `Configuration` of a usage profile, as a read reports it.
async fn profile_configuration(router: &MultiServiceRouter, name: &str) -> Value {
    let read = ok(router, "GetUsageProfile", &json!({ "Name": name })).await;
    read.get("Configuration")
        .cloned()
        .unwrap_or_else(|| panic!("GetUsageProfile must report a Configuration, got {read}"))
}

/// An update REPLACES the stored configuration with the one it carries — the member is
/// `@required`, so the request states the whole parameter set — and writes the `Description`
/// only when it names one; a later read reports what it wrote.
#[tokio::test]
async fn an_update_replaces_the_stored_configuration() {
    let router = emulator();

    // One row of the usage-profile update table.
    struct ProfileUpdate {
        /// What this row varies, named in every assertion it drives.
        case: &'static str,
        /// The members the update sends beside the `Name`.
        members: Value,
        /// The `Configuration` the profile must report afterwards.
        configuration: Value,
        /// The `Description` it must report afterwards.
        description: &'static str,
    }

    let cases: Vec<ProfileUpdate> = vec![
        ProfileUpdate {
            case: "another job parameter, with no description",
            members: json!({
                "Configuration": {
                    "JobConfiguration": {
                        "maxCapacity": { "DefaultValue": "4", "MinValue": "2", "MaxValue": "8" },
                    },
                },
            }),
            configuration: json!({
                "JobConfiguration": {
                    "maxCapacity": { "DefaultValue": "4", "MinValue": "2", "MaxValue": "8" },
                },
            }),
            description: "as created",
        },
        ProfileUpdate {
            case: "a session parameter and a new description",
            members: json!({
                "Description": "sessions only",
                "Configuration": {
                    "SessionConfiguration": {
                        "idleTimeout": { "DefaultValue": "30", "MinValue": "1", "MaxValue": "60" },
                    },
                },
            }),
            configuration: json!({
                "SessionConfiguration": {
                    "idleTimeout": { "DefaultValue": "30", "MinValue": "1", "MaxValue": "60" },
                },
            }),
            description: "sessions only",
        },
        ProfileUpdate {
            case: "both sections at once",
            members: json!({
                "Configuration": {
                    "JobConfiguration": {
                        "numberOfWorkers": { "DefaultValue": "9" },
                    },
                    "SessionConfiguration": {
                        "idleTimeout": { "DefaultValue": "15" },
                    },
                },
            }),
            configuration: json!({
                "JobConfiguration": { "numberOfWorkers": { "DefaultValue": "9" } },
                "SessionConfiguration": { "idleTimeout": { "DefaultValue": "15" } },
            }),
            description: "as created",
        },
    ];

    for (index, row) in cases.iter().enumerate() {
        let case = row.case;
        let name = format!("profile-update-{index}");
        given_usage_profile(&router, &name, Some("as created")).await;

        let mut request = row.members.clone();
        request["Name"] = json!(name);
        let answered = ok(&router, "UpdateUsageProfile", &request).await;
        assert_eq!(
            answered.get("Name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case}: the update must answer with the profile's name, got {answered}"
        );

        let read = ok(&router, "GetUsageProfile", &json!({ "Name": &name })).await;
        assert_eq!(
            read.get("Configuration"),
            Some(&row.configuration),
            "case {case}: expected the stored configuration to be replaced, got {read}"
        );
        assert_eq!(
            read.get("Description").and_then(Value::as_str),
            Some(row.description),
            "case {case}: expected the description {:?}, got {read}",
            row.description
        );
    }
}

/// A name the account holds no usage profile under is refused with the modelled miss, and a
/// configuration no value satisfies is refused as invalid input — in both cases the stored
/// profile is left exactly as it was.
#[tokio::test]
async fn an_update_of_an_absent_profile_is_the_modelled_miss() {
    let router = emulator();
    given_usage_profile(&router, "held-profile", None).await;
    given_usage_profile(&router, "deleted-profile", None).await;
    ok(
        &router,
        "DeleteUsageProfile",
        &json!({ "Name": "deleted-profile" }),
    )
    .await;
    let before = profile_configuration(&router, "held-profile").await;

    let configuration = json!({
        "JobConfiguration": {
            "numberOfWorkers": { "DefaultValue": "2", "MinValue": "1", "MaxValue": "4" },
        },
    });

    // (case, the name the update addresses, the error code it must answer with)
    let misses: Vec<(&str, &str, &str)> = vec![
        (
            "a name nothing created",
            "profile-that-never-existed",
            "EntityNotFoundException",
        ),
        (
            "a name a delete removed",
            "deleted-profile",
            "EntityNotFoundException",
        ),
        (
            "a stored name in upper case",
            "HELD-PROFILE",
            "EntityNotFoundException",
        ),
    ];
    for (case, name, expected) in &misses {
        let (status, body) = call(
            &router,
            "UpdateUsageProfile",
            &json!({ "Name": name, "Configuration": configuration }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert!(
            error_code(&body).contains(expected),
            "case {case}: expected {expected}, got {body}"
        );
        // RECORDED wording: the update closes the sentence with a period, where a READ of the
        // same missing name does not — the two operations word the one miss differently, so
        // both are pinned here.
        assert_eq!(
            error_message(&body),
            format!("UsageProfile {name} does not exists."),
            "case {case}: expected the update's own miss wording, got {body}"
        );
        let (_, read) = call(&router, "GetUsageProfile", &json!({ "Name": name })).await;
        assert_eq!(
            error_message(&read),
            format!("UsageProfile {name} does not exists"),
            "case {case}: expected the read's own miss wording, got {read}"
        );
    }

    // A configuration whose default sits outside its own range configures a parameter no
    // value satisfies, which the update refuses exactly as the create does.
    let (status, body) = call(
        &router,
        "UpdateUsageProfile",
        &json!({
            "Name": "held-profile",
            "Configuration": {
                "JobConfiguration": {
                    "numberOfWorkers": { "DefaultValue": "99", "MinValue": "1", "MaxValue": "4" },
                },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case a default outside its own range: expected 400, got {status} with {body}"
    );
    assert!(
        error_code(&body).contains("InvalidInputException"),
        "case a default outside its own range: expected InvalidInputException, got {body}"
    );

    let after = profile_configuration(&router, "held-profile").await;
    assert_eq!(
        after, before,
        "a refused update must write nothing, expected {before}, got {after}"
    );
}

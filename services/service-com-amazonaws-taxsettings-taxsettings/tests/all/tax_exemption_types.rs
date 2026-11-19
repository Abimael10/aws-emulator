// SPDX-License-Identifier: CC0-1.0

//! `GetTaxExemptionTypes`: the tax exemption type catalog this service publishes,
//! end to end over the restJson1 wire.
//!
//! The rule under test: the call reports the service's published catalog —
//! EVERY entry, in the catalog's order, each with its display name, its
//! description and the jurisdictions it applies in, every one of them inside the
//! constraints the model puts on `TaxExemptionType` and `Authority` — and it
//! reports the same collection whatever the request's form and whatever an
//! account has stored, because the collection belongs to the service and not to a
//! caller. Reading it is a report: it changes nothing a later caller sees.
//!
//! The tables below therefore span the whole catalog (pinned LITERALLY in
//! [`PINNED_CATALOG`], and cross-checked against the published
//! [`TAX_EXEMPTION_TYPE_CATALOG`] so neither can drift from the other), the forms
//! a request for a member-less input shape can take, and — since the same
//! collection is what `PutTaxExemption` admits a jurisdiction against — the states
//! one entry names against the states another one names.
//!
//! The harness (`emulator`, `emulator_with_engine`, `call`,
//! `hold_validations_open`) lives in `tax_registration_lifecycle.rs`; nothing
//! here builds a second one.

use serde_json::{json, Value};
use service_com_amazonaws_taxsettings_taxsettings::ops::types::TaxExemptionType;
use service_com_amazonaws_taxsettings_taxsettings::state::{
    tax_exemption_type_catalog, ERROR_CODE_FIELD_VALIDATION_FAILED,
    EXEMPTION_AUTHORITY_NOT_SUPPORTED, TAX_EXEMPTION_TYPE_CATALOG,
};

use crate::tax_registration_lifecycle::{
    call, emulator, emulator_with_engine, field_list, hold_validations_open, Answer,
};

/// The `taxExemptionTypes` a call reported.
fn listed(answer: &Answer) -> Vec<Value> {
    answer
        .get("taxExemptionTypes")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// One catalog entry as the restJson1 wire carries it: every member the entry
/// sets, and no member it does not.
pub(crate) fn wire_form(entry: &TaxExemptionType) -> Value {
    let jurisdictions: Vec<Value> = entry
        .applicable_jurisdictions
        .as_ref()
        .map(|authorities| {
            authorities
                .iter()
                .map(|authority| {
                    let mut rendered = json!({ "country": authority.country });
                    if let Some(state) = &authority.state {
                        rendered["state"] = json!(state);
                    }
                    rendered
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "displayName": entry.display_name,
        "description": entry.description,
        "applicableJurisdictions": jurisdictions,
    })
}

/// The states the `501C` entry applies in, by their two-letter postal codes: that
/// entry's own list, as a recorded `GetTaxExemptionTypes` response publishes it.
const CHARITABLE_ORGANIZATION_STATES: &[&str] = &[
    "AZ", "CO", "CT", "DC", "FL", "GA", "ID", "IL", "IN", "KS", "KY", "LA", "MA", "MD", "MI", "MN",
    "MO", "MS", "ND", "NE", "NJ", "NV", "NY", "OH", "OK", "PA", "RI", "SC", "SD", "TN", "TX", "UT",
    "VA", "VT", "WI", "WV", "WY",
];

/// The states the `Agriculture Production` entry applies in: that entry's own
/// list, as a recorded `GetTaxExemptionTypes` response publishes it — a different
/// set of states from the entry above (it carries PR, and carries neither CA nor
/// DC), which is why no entry may be given the list another one carries.
const AGRICULTURE_PRODUCTION_STATES: &[&str] = &[
    "AL", "AR", "AZ", "CO", "CT", "FL", "GA", "HI", "IA", "ID", "IL", "IN", "KS", "KY", "LA", "MA",
    "MD", "ME", "MI", "MN", "MO", "MS", "NC", "NE", "NJ", "NV", "NY", "OH", "OK", "PA", "PR", "RI",
    "SC", "TN", "TX", "UT", "VA", "VT", "WA", "WI", "WV", "WY",
];

/// The catalog this service publishes, pinned LITERALLY:
/// `(displayName, description, the states it applies in)`.
///
/// Written out here rather than read from `state::TAX_EXEMPTION_TYPE_CATALOG`, so a
/// regression in the published collection — an emptied catalog, a dropped entry, a
/// reworded or duplicated description, a jurisdiction that moved, an entry given a
/// country-wide default list rather than its own — fails this test instead of being
/// restated by it.
///
/// Every row is an entry of the collection the service publishes, taken member for
/// member from a recorded `GetTaxExemptionTypes` response: a type is named by one
/// short token carried identically in `displayName` and `description`, and each
/// entry carries its own per-state `applicableJurisdictions`.
const PINNED_CATALOG: &[(&str, &str, &[&str])] = &[
    ("501C", "501C", CHARITABLE_ORGANIZATION_STATES),
    (
        "Agriculture Production",
        "Agriculture Production",
        AGRICULTURE_PRODUCTION_STATES,
    ),
];

/// One pinned entry as the restJson1 wire must carry it.
fn pinned_wire_form(
    (display_name, description, states): &(&str, &str, &[&str]),
) -> Value {
    let authorities: Vec<Value> = states
        .iter()
        .map(|state| json!({ "country": "US", "state": state }))
        .collect();
    json!({
        "displayName": display_name,
        "description": description,
        "applicableJurisdictions": authorities,
    })
}

/// The entry the pinned catalog publishes under `display_name`, as the restJson1
/// wire carries it.
///
/// `pub(crate)` so a suite asserting what a read reports for a PUBLISHED type
/// compares against the literally pinned collection above rather than against
/// the table the handler projects.
pub(crate) fn pinned_entry(display_name: &str) -> Value {
    let entry = PINNED_CATALOG
        .iter()
        .find(|(pinned, _, _)| *pinned == display_name)
        .unwrap_or_else(|| panic!("the pinned catalog publishes {display_name:?}"));
    pinned_wire_form(entry)
}

/// Read the catalog over the wire from a fresh emulator.
async fn read_catalog(body: &Value) -> Answer {
    let router = emulator();
    call(&router, "GetTaxExemptionTypes", body).await
}

/// RULE: the call reports every entry of the service's published catalog, in the
/// catalog's order, member for member — against the literally pinned collection.
#[tokio::test]
async fn the_whole_catalog_is_reported_entry_for_entry() {
    let answer = read_catalog(&json!({})).await;
    assert_eq!(
        (answer.status, answer.error_code.as_str()),
        (200, ""),
        "case \"an empty request\": expected 200, got {answer:?}"
    );

    let reported = listed(&answer);
    assert!(
        !reported.is_empty(),
        "case \"an empty request\": the supported exemption types must be a populated \
         catalog, got {reported:?}"
    );

    // The expectation is the LITERAL table above, so this test fails on a change to
    // the published catalog rather than following it.
    let expected: Vec<Value> = PINNED_CATALOG.iter().map(pinned_wire_form).collect();
    assert_eq!(
        reported.len(),
        expected.len(),
        "case \"an empty request\": expected {} entries, got {reported:?}",
        expected.len()
    );
    for (index, want) in expected.iter().enumerate() {
        let case = format!("entry {index}");
        assert_eq!(
            &reported[index], want,
            "case {case:?}: expected {want}, got {}",
            reported[index]
        );
    }

    // And the handler's own projection of the published table renders to exactly
    // that, so the two cannot drift apart silently.
    let projected: Vec<Value> = tax_exemption_type_catalog().iter().map(wire_form).collect();
    assert_eq!(
        projected, expected,
        "case \"the published table\": expected the pinned catalog, got {projected:?}"
    );
}

/// RULE: every reported entry sits inside the constraints the model puts on
/// `TaxExemptionType` and `Authority`, and identifies itself: `displayName`
/// length 0..50, `description` length 1..200
/// (`smithy_query(query="shape", shape="TaxExemptionType")`), `Authority$country`
/// a required two-letter alphabetic code and `Authority$state`, where present, a
/// non-blank name (`smithy_query(query="shape", shape="Authority")`).
#[tokio::test]
async fn every_entry_satisfies_the_modelled_constraints() {
    let answer = read_catalog(&json!({})).await;
    let reported = listed(&answer);

    let mut display_names: Vec<&str> = Vec::new();
    let mut descriptions: Vec<&str> = Vec::new();
    for (index, entry) in reported.iter().enumerate() {
        let case = format!("entry {index}");

        let display_name = entry
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a displayName, got {entry}"));
        assert!(
            display_name.chars().count() <= 50 && !display_name.trim().is_empty(),
            "case {case:?}: displayName is a 0..50 string naming the type, got {display_name:?}"
        );

        let description = entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a description, got {entry}"));
        let description_length = description.chars().count();
        assert!(
            (1..=200).contains(&description_length) && !description.trim().is_empty(),
            "case {case:?}: description is a 1..200 string, got {description_length} characters \
             in {description:?}"
        );

        let jurisdictions = entry
            .get("applicableJurisdictions")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected applicableJurisdictions, got {entry}")
            });
        assert!(
            !jurisdictions.is_empty(),
            "case {case:?}: a published type applies somewhere, got {jurisdictions:?}"
        );
        for authority in jurisdictions {
            let country = authority
                .get("country")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: expected a country, got {authority}"));
            assert!(
                country.len() == 2 && country.chars().all(|letter| letter.is_ascii_alphabetic()),
                "case {case:?}: country is a two-letter code, got {country:?}"
            );
            if let Some(state) = authority.get("state") {
                let state = state
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: state is a string, got {state}"));
                let state_length = state.chars().count();
                assert!(
                    (1..=50).contains(&state_length) && !state.trim().is_empty(),
                    "case {case:?}: state is a non-blank 1..50 name, got {state:?}"
                );
            }
        }

        // The description is the token `PutTaxExemption$exemptionType` names a type
        // by ("Use the supported tax exemption type description"), so two entries
        // may not share one — nor a display name, which is how a client tells them
        // apart on screen.
        assert!(
            !descriptions.contains(&description),
            "case {case:?}: {description:?} is published twice, so it names no one type"
        );
        assert!(
            !display_names.contains(&display_name),
            "case {case:?}: {display_name:?} is published twice"
        );
        descriptions.push(description);
        display_names.push(display_name);
    }
}

/// RULE: a published entry names its type by ONE short catalog token, carried
/// identically in `displayName` and `description` — the published form of both
/// members, and the token `PutTaxExemptionRequest$exemptionType` names a type by
/// ("Use the supported tax exemption type description",
/// `smithy_query(query="doc", shape="PutTaxExemptionRequest",
/// member="exemptionType")`).
#[tokio::test]
async fn every_entry_names_its_type_by_one_token() {
    let answer = read_catalog(&json!({})).await;
    let reported = listed(&answer);
    assert!(
        !reported.is_empty(),
        "case \"the published catalog\": expected a populated collection, got {reported:?}"
    );

    for (index, entry) in reported.iter().enumerate() {
        let case = format!("entry {index}");
        assert_eq!(
            entry.get("displayName").and_then(Value::as_str),
            entry.get("description").and_then(Value::as_str),
            "case {case:?}: expected one token carried by both members, got {entry}"
        );
    }
}

/// RULE: each entry's `applicableJurisdictions` is its OWN per-state list — one
/// `Authority` per state whose taxing authority grants the category, never a
/// country-wide wildcard and never the list another entry carries.
#[tokio::test]
async fn every_entry_carries_its_own_per_state_jurisdictions() {
    let reported = listed(&read_catalog(&json!({})).await);
    assert!(
        reported.len() > 1,
        "case \"the published catalog\": expected the published collection, got {reported:?}"
    );

    let mut seen: Vec<(String, Vec<Value>)> = Vec::new();
    for (index, entry) in reported.iter().enumerate() {
        let name = entry
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let case = format!("entry {index} ({name})");
        let held = entry
            .get("applicableJurisdictions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            !held.is_empty(),
            "case {case:?}: a published type applies somewhere, got {entry}"
        );
        for authority in &held {
            assert!(
                authority.get("state").and_then(Value::as_str).is_some(),
                "case {case:?}: a sales-and-use-tax exemption is granted by a state's own \
                 authority, so every jurisdiction names a state, got {authority}"
            );
        }
        for (other, other_held) in &seen {
            assert_ne!(
                &held, other_held,
                "case {case:?}: expected this entry's own jurisdictions, got the list {other:?} \
                 carries"
            );
        }
        seen.push((name, held));
    }
}

/// One admission row: the type a submission names, the state it claims it in, and
/// whether the published collection grants that type there.
///
/// The states are written out rather than read from the catalog, so a row fails if
/// an entry's published list moves — and they span a state both entries name, one
/// only the first names, one only the second names, one neither names though the
/// state levies the tax, and one no US sales tax is levied in. No row is the
/// `(type, state)` pair any issue cited.
const ADMISSION: &[(&str, &str, bool)] = &[
    ("501C", "AZ", true),
    ("Agriculture Production", "AZ", true),
    ("501C", "SD", true),
    ("Agriculture Production", "SD", false),
    ("501C", "DC", true),
    ("Agriculture Production", "DC", false),
    ("501C", "HI", false),
    ("Agriculture Production", "HI", true),
    ("501C", "WA", false),
    ("Agriculture Production", "WA", true),
    ("501C", "PR", false),
    ("Agriculture Production", "PR", true),
    ("501C", "CA", false),
    ("Agriculture Production", "CA", false),
    ("501C", "OR", false),
    ("Agriculture Production", "OR", false),
];

/// RULE's other side: the published collection is also what a submission's
/// jurisdiction is admitted against, so a published type is granted in the states
/// ITS entry names and refused everywhere else — and the record a granted
/// submission reads back as carries that entry's own jurisdictions.
#[tokio::test]
async fn a_published_type_is_granted_only_in_the_states_its_own_entry_names() {
    let account = engine::emulator_account_id();
    for (exemption_type, state, admitted) in ADMISSION.iter().copied() {
        let case = format!("{exemption_type} claimed in US/{state}");
        let router = emulator();
        let answer = call(
            &router,
            "PutTaxExemption",
            &json!({
                "accountIds": [account],
                "authority": { "country": "US", "state": state },
                "exemptionType": exemption_type,
                "exemptionCertificate": crate::tax_exemptions::certificate("certificate.pdf"),
            }),
        )
        .await;
        let held = crate::tax_exemptions::reported_exemptions(
            &call(&router, "ListTaxExemptions", &json!({})).await,
            account,
        );

        if admitted {
            assert_eq!(
                (answer.status, answer.error_code.as_str()),
                (200, ""),
                "case {case:?}: the entry names this state, so the claim stands, got {answer:?}"
            );
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: the submission must be recorded, got {held:?}"
            );
            // The record is held under the published entry, with that entry's own
            // jurisdictions rather than another entry's or a country-wide list.
            let recorded = held[0]
                .get("taxExemptionType")
                .cloned()
                .unwrap_or(Value::Null);
            assert_eq!(
                recorded,
                pinned_entry(exemption_type),
                "case {case:?}: expected the published entry {}, got {recorded}",
                pinned_entry(exemption_type)
            );
        } else {
            assert_eq!(
                (answer.status, answer.error_code.as_str()),
                (400, "ValidationException"),
                "case {case:?}: the entry does not name this state, so the claim is refused, got \
                 {answer:?}"
            );
            assert_eq!(
                (
                    answer.get("errorCode").as_str(),
                    answer.get("message").as_str(),
                    field_list(&answer),
                ),
                (
                    Some(ERROR_CODE_FIELD_VALIDATION_FAILED),
                    Some(EXEMPTION_AUTHORITY_NOT_SUPPORTED),
                    vec!["AUTHORITY".to_string()],
                ),
                "case {case:?}: expected the refusal class's own envelope, got {:?}",
                answer.body
            );
            assert!(
                held.is_empty(),
                "case {case:?}: a refused submission records nothing, got {held:?}"
            );
        }
    }
}

/// RULE: the collection belongs to the service, so the same catalog is reported
/// whatever form the member-less request takes and whatever an account has
/// stored.
#[tokio::test]
async fn the_same_catalog_is_reported_for_every_request_and_every_account() {
    // Rows differ along the form a request for a member-less input shape can
    // take: nothing at all, an empty object, and an object carrying a member the
    // input shape does not declare (restJson1 ignores one).
    let forms: &[(&str, Value)] = &[
        ("an empty request", json!({})),
        ("a member the input shape does not declare", json!({ "exemptionType": "Reseller" })),
        ("several undeclared members", json!({ "nextToken": "x", "maxResults": 1 })),
    ];

    let reference = listed(&read_catalog(&json!({})).await);
    for (case, body) in forms {
        let answer = read_catalog(body).await;
        assert_eq!(
            (answer.status, answer.error_code.as_str()),
            (200, ""),
            "case {case:?}: expected 200, got {answer:?}"
        );
        assert_eq!(
            listed(&answer),
            reference,
            "case {case:?}: expected the published catalog, got {:?}",
            listed(&answer)
        );
    }

    // And over accounts: an emulator where several accounts have stored a tax
    // registration and one where nothing was ever written answer the same
    // catalog, because no caller's records are in it.
    let (router, _engine) = emulator_with_engine();
    for account in ["000000000001", "000000000002"] {
        let stored = call(
            &router,
            "PutTaxRegistration",
            &json!({
                "accountId": account,
                "taxRegistrationEntry": {
                    "registrationId": "DE136695976",
                    "registrationType": "VAT",
                    "legalName": "Acme GmbH",
                    "legalAddress": { "postalCode": "10115", "countryCode": "DE" },
                },
            }),
        )
        .await;
        assert_eq!(
            stored.status, 200,
            "case {account:?}: the registration must store, got {stored:?}"
        );
    }
    let after_writes = call(&router, "GetTaxExemptionTypes", &json!({})).await;
    assert_eq!(
        listed(&after_writes),
        reference,
        "case \"an account with a stored registration\": expected the published catalog, got {:?}",
        listed(&after_writes)
    );
}

/// RULE: reading the catalog is a report — it changes nothing a later caller
/// sees, neither the catalog itself nor any record beside it.
#[tokio::test]
async fn reading_the_catalog_changes_nothing() {
    let (router, engine) = emulator_with_engine();
    let account = json!({ "accountId": "000000000007" });

    let stored = call(
        &router,
        "PutTaxRegistration",
        &json!({
            "accountId": "000000000007",
            "taxRegistrationEntry": {
                "registrationId": "29ABCDE1234F1Z5",
                "registrationType": "GST",
                "legalName": "Acme India Private Limited",
                "legalAddress": {
                    "addressLine1": "1 Residency Road",
                    "city": "Bengaluru",
                    "stateOrRegion": "Karnataka",
                    "postalCode": "560025",
                    "countryCode": "IN",
                },
            },
        }),
    )
    .await;
    assert_eq!(
        stored.status, 200,
        "case \"the registration a catalog read must not disturb\": expected 200, got {stored:?}"
    );
    // The TRN's validation is a transition the clock owes; holding it open keeps
    // the two registration reads below comparable without a sleep.
    hold_validations_open(&engine);

    let before = call(&router, "GetTaxRegistration", &account).await;
    let first = call(&router, "GetTaxExemptionTypes", &json!({})).await;
    let second = call(&router, "GetTaxExemptionTypes", &json!({})).await;
    let after = call(&router, "GetTaxRegistration", &account).await;
    let inheritance = call(&router, "GetTaxInheritance", &json!({})).await;

    assert_eq!(
        listed(&second),
        listed(&first),
        "case \"a second read\": the catalog must be identical on every call, got {:?}",
        listed(&second)
    );
    assert_eq!(
        after.body, before.body,
        "case \"a registration read across a catalog read\": expected {:?}, got {:?}",
        before.body, after.body
    );
    assert_eq!(
        (inheritance.status, inheritance.get("heritageStatus").clone()),
        (200, json!("OptOut")),
        "case \"an inheritance read across a catalog read\": expected the unchanged default, \
         got {inheritance:?}"
    );
}

/// The catalog the handler reports and the published table are one collection:
/// the projection sets every member of every row, so no entry reaches the wire
/// missing what the table says about it.
#[test]
fn the_projection_covers_every_published_row() {
    let projected = tax_exemption_type_catalog();
    assert_eq!(
        projected.len(),
        TAX_EXEMPTION_TYPE_CATALOG.len(),
        "the projection must cover every published row, got {} of {}",
        projected.len(),
        TAX_EXEMPTION_TYPE_CATALOG.len()
    );
    for (entry, (display_name, description, jurisdictions)) in
        projected.iter().zip(TAX_EXEMPTION_TYPE_CATALOG)
    {
        assert_eq!(
            entry.display_name.as_deref(),
            Some(*display_name),
            "case {display_name:?}: expected the published display name, got {:?}",
            entry.display_name
        );
        assert_eq!(
            entry.description.as_deref(),
            Some(*description),
            "case {display_name:?}: expected the published description, got {:?}",
            entry.description
        );
        let reported: Vec<(String, Option<String>)> = entry
            .applicable_jurisdictions
            .as_ref()
            .map(|authorities| {
                authorities
                    .iter()
                    .map(|authority| (authority.country.clone(), authority.state.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let published: Vec<(String, Option<String>)> = jurisdictions
            .iter()
            .map(|(country, state)| {
                ((*country).to_string(), state.map(std::string::ToString::to_string))
            })
            .collect();
        assert_eq!(
            reported, published,
            "case {display_name:?}: expected the published jurisdictions {published:?}, got \
             {reported:?}"
        );
    }
}

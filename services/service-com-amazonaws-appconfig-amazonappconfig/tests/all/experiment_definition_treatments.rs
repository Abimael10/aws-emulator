// SPDX-License-Identifier: CC0-1.0

//! The modelled maximum of an experiment definition's `Treatments` list, which AppConfig
//! does not apply on the wire.
//!
//! `TreatmentInputList` is modelled `@length(min: 1, max: 5)`
//! (`smithy_query(query="shape",shape="TreatmentInputList")`) and both members that target
//! it — `CreateExperimentDefinitionRequest$Treatments` and
//! `UpdateExperimentDefinitionRequest$Treatments` — are served with a LONGER list: the
//! maximum is a documentation bound on that list rather than a server-side check, so a
//! request whose only fault is breaking it must be served, persisted and read back.
//!
//! The rules under test:
//!
//! * a create carrying more than five treatments answers HTTP 201 with `Status` `IDLE`,
//!   persists every treatment it carried, is reported by `GetExperimentDefinition` and is
//!   listed by `ListExperimentDefinitions`;
//! * an update carrying more than five answers HTTP 200 with `Status` `IDLE`, and
//!   `GetExperimentDefinition` then reports exactly the list it carried;
//! * the service keys each treatment from its place in the list however long the list is —
//!   the control is `c` and the rest are `t1`, `t2`, … — so every key stays inside the
//!   model's `TreatmentKey` pattern;
//! * nothing ELSE about the list is relaxed: a list below the modelled minimum of one is
//!   still HTTP 400, and so is a list of any length one of whose ELEMENTS breaks a
//!   constraint its own member declares (`TreatmentInput$Weight`'s `@range`,
//!   `TreatmentInput$Description`'s length), with the refusal naming that element;
//! * relaxing the modelled bound MOVES the clamp rather than removing it: this service
//!   keeps at most `state::MAX_TREATMENTS_PER_EXPERIMENT_DEFINITION` treatments on one
//!   definition, serves a list at that cap and refuses one past it — with the quota error
//!   the create models and the bad request the update models — storing nothing either way.
//!
//! The table spans what the rule generalises over: the NUMBER of treatments, from the
//! modelled maximum (the positive control) to well past it, on both operations, and for the
//! refusals the position of the offending element inside an over-long list.

use serde_json::{json, Value};
use service_com_amazonaws_appconfig_amazonappconfig::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::Answer;
use crate::appconfig_workflow::router;
use crate::experiment_definitions::{
    body, create_definition, flag, get_definition, scope, treatment, Scope,
};
use crate::list_experiment_definitions::list;
use crate::update_experiment_definition::update_definition;

/// The modelled maximum of `TreatmentInputList`, which is the bound these cases exceed.
/// From `smithy_query(query="shape",shape="TreatmentInputList")`.
const MODELLED_MAXIMUM: usize = 5;

/// The treatment counts a row drives, spanning the modelled maximum (the positive control)
/// and lists past it: one past, two past, and an order of magnitude past.
const COUNTS: [usize; 5] = [MODELLED_MAXIMUM, MODELLED_MAXIMUM + 1, 7, 12, 50];

/// `n` treatments, each with a weight and a description of its own, so the position a
/// treatment was listed at is visible in the record that comes back.
fn treatments(n: usize) -> Vec<Value> {
    (0..n)
        .map(|index| {
            treatment(
                index as f64,
                Some(&format!("variation {index}")),
                flag(index % 2 == 0),
            )
        })
        .collect()
}

/// The keys the service must have minted for a definition with `n` treatments: the
/// control's, then one per treatment in the order the request listed them.
fn expected_keys(n: usize) -> Vec<String> {
    std::iter::once(state::CONTROL_TREATMENT_KEY.to_string())
        .chain((0..n).map(state::treatment_key))
        .collect()
}

/// The `Treatments` of a definition document, failing the case with the whole document when
/// it carries none.
fn reported(case: &str, document: &str, body: &Value) -> Vec<Value> {
    body.get("Treatments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: {document} must report Treatments, got {body}")
        })
}

/// Assert that `body` reports the `n` treatments the request carried, with the keys the
/// service mints and the weights and descriptions the request listed.
fn assert_treatments(case: &str, document: &str, body: &Value, requested: &[Value]) {
    let listed = reported(case, document, body);
    assert_eq!(
        listed.len(),
        requested.len(),
        "case {case:?}: {document} must report {} treatments, got {}",
        requested.len(),
        listed.len()
    );
    let keys: Vec<String> = std::iter::once(body.get("Control").cloned().unwrap_or(Value::Null))
        .chain(listed.iter().cloned())
        .map(|entry| {
            entry
                .get("Key")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        keys,
        expected_keys(requested.len()),
        "case {case:?}: {document} must key the control and each treatment from its place \
         in the list, got {keys:?}"
    );
    for (index, (want, got)) in requested.iter().zip(&listed).enumerate() {
        let wanted_weight = want.get("Weight").and_then(Value::as_f64);
        let got_weight = got.get("Weight").and_then(Value::as_f64);
        assert_eq!(
            got_weight, wanted_weight,
            "case {case:?}: {document} treatment {index} must carry the submitted weight \
             {wanted_weight:?}, got {got_weight:?}"
        );
        assert_eq!(
            got.get("Description"),
            want.get("Description"),
            "case {case:?}: {document} treatment {index} must carry the submitted \
             description {:?}, got {:?}",
            want.get("Description"),
            got.get("Description")
        );
    }
}

/// The `Status` a definition reports, or the document when it reports none.
fn status(case: &str, body: &Value) -> String {
    body.get("Status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no Status in {body}"))
        .to_string()
}

/// RULE: a create carrying more treatments than the model's maximum is SERVED — 201,
/// `IDLE`, every treatment persisted, reported by the read and present in the listing.
#[tokio::test]
async fn a_create_past_the_treatment_list_maximum_is_served() {
    let router = router();
    let scope = scope(&router, "treatments-create").await;

    for count in COUNTS {
        let case = format!("{count} treatments on the create");
        let name = format!("created-with-{count}");
        let listed = treatments(count);
        let request = body(
            &scope,
            &name,
            listed.clone(),
            treatment(100.0, None, flag(false)),
        );
        let created = create_definition(&router, &scope.application, &request).await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: CreateExperimentDefinition must answer 201, got {} / {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        let document = created.json();
        assert_eq!(
            status(&case, &document),
            state::EXPERIMENT_DEFINITION_IDLE,
            "case {case:?}: a definition nothing has run yet must report {}, got {}",
            state::EXPERIMENT_DEFINITION_IDLE,
            status(&case, &document)
        );
        assert_treatments(&case, "the create response", &document, &listed);

        // Persisted, not only answered: the read reports the same list.
        let id = document
            .get("Id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: the create answered no Id"))
            .to_string();
        let read = get_definition(&router, &scope.application, &id).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetExperimentDefinition must answer 200, got {} / {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_treatments(&case, "GetExperimentDefinition", &read.json(), &listed);

        // And the definition is addressable through the listing, by the name it was
        // created under.
        let listing = list(
            &router,
            &[("application_identifier", scope.application.clone())],
        )
        .await;
        assert_eq!(
            listing.status,
            200,
            "case {case:?}: ListExperimentDefinitions must answer 200, got {} / {}",
            listing.status,
            String::from_utf8_lossy(&listing.body)
        );
        let names: Vec<String> = listing
            .json()
            .get("Items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item.get("Name").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        assert!(
            names.contains(&name),
            "case {case:?}: the listing must report {name:?}, got {names:?}"
        );
    }
}

/// RULE: an update carrying more treatments than the model's maximum is SERVED — 200,
/// `IDLE`, and the read then reports exactly the list the update carried.
#[tokio::test]
async fn an_update_past_the_treatment_list_maximum_is_served() {
    let router = router();
    let scope = scope(&router, "treatments-update").await;

    for count in COUNTS {
        let case = format!("{count} treatments on the update");
        let name = format!("updated-to-{count}");
        // Created with ONE treatment, so the update is what grows the list past the bound.
        let created = create_definition(
            &router,
            &scope.application,
            &body(
                &scope,
                &name,
                vec![treatment(100.0, Some("the only variation"), flag(true))],
                treatment(0.0, None, flag(false)),
            ),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: the baseline create must answer 201, got {} / {}",
            created.status,
            String::from_utf8_lossy(&created.body)
        );
        let id = created
            .string("Id")
            .unwrap_or_else(|| panic!("case {case:?}: the baseline create answered no Id"));

        let listed = treatments(count);
        let updated = update_definition(
            &router,
            &scope.application,
            &id,
            &json!({ "Treatments": listed.clone() }),
        )
        .await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: UpdateExperimentDefinition must answer 200, got {} / {}",
            updated.status,
            String::from_utf8_lossy(&updated.body)
        );
        let document = updated.json();
        assert_eq!(
            status(&case, &document),
            state::EXPERIMENT_DEFINITION_IDLE,
            "case {case:?}: the updated definition must still report {}, got {}",
            state::EXPERIMENT_DEFINITION_IDLE,
            status(&case, &document)
        );
        assert_treatments(&case, "the update response", &document, &listed);

        let read = get_definition(&router, &scope.application, &id).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetExperimentDefinition must answer 200, got {} / {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_treatments(&case, "GetExperimentDefinition", &read.json(), &listed);
    }
}

/// How many definitions `application` holds, so a refused write can be shown to have
/// stored nothing.
async fn definition_count(router: &MultiServiceRouter, case: &str, application: &str) -> usize {
    let listing = list(router, &[("application_identifier", application.to_string())]).await;
    assert_eq!(
        listing.status,
        200,
        "case {case:?}: ListExperimentDefinitions must answer 200, got {} / {}",
        listing.status,
        String::from_utf8_lossy(&listing.body)
    );
    listing
        .json()
        .get("Items")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

/// Assert `answer` is the modelled `BadRequestException` whose clause names `member`.
fn assert_refused(case: &str, answer: &Answer, member: &str) {
    assert_eq!(
        answer.status,
        400,
        "case {case:?}: expected 400, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert!(
        answer.error_code().contains("BadRequestException"),
        "case {case:?}: expected BadRequestException, got {:?}",
        answer.error_code()
    );
    let message = answer
        .string("Message")
        .unwrap_or_else(|| String::from_utf8_lossy(&answer.body).to_string());
    assert!(
        message.contains(member),
        "case {case:?}: the refusal must name {member:?}, got {message:?}"
    );
}

/// RULE: only the list's MAXIMUM goes unenforced. A list below the modelled minimum, and a
/// list of any length carrying an element outside its own member's constraints, are still
/// refused — and the refusal names the member at fault.
#[tokio::test]
async fn every_other_requirement_of_the_treatment_list_still_refuses() {
    let router = router();
    let scope = scope(&router, "treatments-refused").await;

    // A definition to patch, so the same rows can be driven through the update too.
    let subject = create_definition(
        &router,
        &scope.application,
        &body(
            &scope,
            "refusal-subject",
            vec![treatment(100.0, None, flag(true))],
            treatment(0.0, None, flag(false)),
        ),
    )
    .await;
    assert_eq!(
        subject.status,
        201,
        "the definition under test must be created, got {} / {}",
        subject.status,
        String::from_utf8_lossy(&subject.body)
    );
    let subject_id = subject
        .string("Id")
        .expect("the definition under test must report an Id");

    // A weight below `TreatmentInput$Weight`'s `@range(min: 0.0)` and a description past
    // `Description`'s 1024-character maximum, each placed BEYOND the modelled list maximum
    // so the row also proves the elements of an over-long list are still walked.
    let over_long_with_bad_weight = {
        let mut listed = treatments(MODELLED_MAXIMUM + 2);
        listed[MODELLED_MAXIMUM + 1] = treatment(-1.0, None, flag(true));
        listed
    };
    let over_long_with_long_description = {
        let mut listed = treatments(MODELLED_MAXIMUM + 3);
        listed[MODELLED_MAXIMUM + 2] =
            treatment(1.0, Some(&"d".repeat(1025)), flag(true));
        listed
    };

    let rows: Vec<(&str, Vec<Value>, &str)> = vec![
        ("an empty treatment list", Vec::new(), "treatments"),
        (
            "a negative weight past the modelled list maximum",
            over_long_with_bad_weight,
            "weight",
        ),
        (
            "an over-long description past the modelled list maximum",
            over_long_with_long_description,
            "description",
        ),
    ];

    for (case, listed, member) in rows {
        let before = definition_count(&router, case, &scope.application).await;
        let created = create_definition(
            &router,
            &scope.application,
            &body(
                &scope,
                &format!("refused-{member}-{}", listed.len()),
                listed.clone(),
                treatment(0.0, None, flag(false)),
            ),
        )
        .await;
        assert_refused(case, &created, member);
        let after = definition_count(&router, case, &scope.application).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused create stores nothing, so the application must still \
             hold {before} definitions, got {after}"
        );

        let updated = update_definition(
            &router,
            &scope.application,
            &subject_id,
            &json!({ "Treatments": listed }),
        )
        .await;
        assert_refused(case, &updated, member);
        let read = get_definition(&router, &scope.application, &subject_id).await;
        let kept = reported(case, "GetExperimentDefinition", &read.json());
        assert_eq!(
            kept.len(),
            1,
            "case {case:?}: a refused update stores nothing, so the definition must still \
             hold its 1 treatment, got {}",
            kept.len()
        );
    }
}

/// The positive control for the scaffolding above: a list AT the modelled maximum is served
/// exactly as a longer one is, so the rows past the bound differ from it in nothing but
/// length.
#[tokio::test]
async fn a_list_at_the_modelled_maximum_is_served_like_a_longer_one() {
    let router = router();
    let scope: Scope = scope(&router, "treatments-control").await;
    let listed = treatments(MODELLED_MAXIMUM);
    let created = create_definition(
        &router,
        &scope.application,
        &body(
            &scope,
            "at-the-maximum",
            listed.clone(),
            treatment(0.0, None, flag(false)),
        ),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "a list at the modelled maximum must be served, got {} / {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    assert_treatments(
        "at the modelled maximum",
        "the create response",
        &created.json(),
        &listed,
    );
}

/// RULE: relaxing the modelled bound moves the clamp, it does not remove it — this service
/// keeps at most `state::MAX_TREATMENTS_PER_EXPERIMENT_DEFINITION` treatments on one
/// definition, serves a list AT that cap and refuses one past it with the error each
/// operation models, storing nothing.
#[tokio::test]
async fn the_services_own_treatment_cap_bounds_what_a_definition_keeps() {
    let router = router();
    let scope = scope(&router, "treatments-cap").await;
    let cap = state::MAX_TREATMENTS_PER_EXPERIMENT_DEFINITION;

    // At the cap: served and persisted, so the refusal below is for the number and
    // nothing else.
    let at_the_cap = treatments(cap);
    let created = create_definition(
        &router,
        &scope.application,
        &body(
            &scope,
            "at-the-cap",
            at_the_cap.clone(),
            treatment(0.0, None, flag(false)),
        ),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "a list at this service's cap must be served, got {} / {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    let id = created
        .string("Id")
        .expect("the create at the cap must report an Id");
    assert_treatments(
        "at the cap",
        "the create response",
        &created.json(),
        &at_the_cap,
    );

    // One past it: the create answers the quota error its model declares, the update the
    // bad request its own model declares (it declares no quota error), and both name the
    // numbers involved and store nothing.
    let past_the_cap = treatments(cap + 1);
    let expected = format!(
        "Cannot store {} treatments on an experiment definition: at most {cap} are kept.",
        cap + 1
    );
    let before = definition_count(&router, "one past the cap", &scope.application).await;
    let refused = create_definition(
        &router,
        &scope.application,
        &body(
            &scope,
            "past-the-cap",
            past_the_cap.clone(),
            treatment(0.0, None, flag(false)),
        ),
    )
    .await;
    assert_eq!(
        refused.status,
        402,
        "a create past the cap must answer the modelled quota error, got {} / {}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    assert!(
        refused.error_code().contains("ServiceQuotaExceededException"),
        "a create past the cap must answer ServiceQuotaExceededException, got {:?}",
        refused.error_code()
    );
    assert_eq!(
        refused.string("Message"),
        Some(expected.clone()),
        "a create past the cap must state the cap it broke, got {:?}",
        refused.string("Message")
    );
    let after = definition_count(&router, "one past the cap", &scope.application).await;
    assert_eq!(
        after, before,
        "a refused create stores nothing, so the application must still hold {before} \
         definitions, got {after}"
    );

    let refused = update_definition(
        &router,
        &scope.application,
        &id,
        &json!({ "Treatments": past_the_cap }),
    )
    .await;
    assert_eq!(
        refused.status,
        400,
        "an update past the cap must answer the modelled bad request, got {} / {}",
        refused.status,
        String::from_utf8_lossy(&refused.body)
    );
    assert!(
        refused.error_code().contains("BadRequestException"),
        "an update past the cap must answer BadRequestException, got {:?}",
        refused.error_code()
    );
    assert_eq!(
        refused.string("Message"),
        Some(expected),
        "an update past the cap must state the cap it broke, got {:?}",
        refused.string("Message")
    );
    let read = get_definition(&router, &scope.application, &id).await;
    assert_treatments(
        "one past the cap",
        "GetExperimentDefinition",
        &read.json(),
        &at_the_cap,
    );
}

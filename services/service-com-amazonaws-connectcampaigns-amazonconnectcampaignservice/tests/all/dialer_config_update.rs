// SPDX-License-Identifier: CC0-1.0

//! Updating the dialer config of a campaign, end to end over HTTP.
//!
//! One rule, asserted over tables whose rows differ along WHICH member of the
//! `DialerConfig` union a campaign dials with and what that member's parameters ask for,
//! rather than along the single request that exposed the gap:
//! `UpdateCampaignDialerConfig` replaces the whole dialer config of the campaign the path
//! addresses with the one the request carries and touches nothing else about the campaign
//! — not its id, ARN, name, Connect instance, outbound-call config, tags, run state or
//! place in the creation order — so each later read of that campaign reports the config
//! the last update asked for. The campaign's run state is not weighed at all, so a
//! campaign that is dialling takes an update exactly as an idle one does, and a request
//! asking for the config a campaign already carries is the no-op the operation's declared
//! idempotence makes it.
//!
//! What an update may NOT do is move a campaign between dialling WITH agents and
//! dialling without them: that is the kind of campaign it is, settled when it was
//! created, so a request crossing it is refused as the bad request it is — in the
//! service's own words, around the two dialer types — and writes nothing, in either
//! direction. Within the agent-based kind the dialer type moves freely, which is what the
//! recorded responses of this operation show. An id naming no stored campaign is refused
//! in the words this service answers a campaign-resource lookup that resolves to nothing
//! with, also storing nothing.

use serde_json::{json, Map, Value};
use service_com_amazonaws_connectcampaigns_amazonconnectcampaignservice::state::{
    campaign_not_found_message, ALL_DIALER_TYPES,
};

use super::common::{call, router, Answer, Harness};

/// The region every case here signs for.
const REGION: &str = "us-east-1";

/// The Connect instance every campaign below is created for.
const INSTANCE: &str = "dialer-config-instance";

/// Every member of the `DialerConfig` union, from `smithy_query(query="shape",
/// shape="DialerConfig")` — the dialers a campaign of this service can be created with,
/// and so the set every case below is driven over.
const DIALER_MEMBERS: [&str; 3] = [
    "progressiveDialerConfig",
    "predictiveDialerConfig",
    "agentlessDialerConfig",
];

/// The one member of the union that carries no `bandwidthAllocation`, from
/// `smithy_query(query="shape", shape="AgentlessDialerConfig")`.
const AGENTLESS_MEMBER: &str = "agentlessDialerConfig";

/// The two members of the union a campaign dialling THROUGH AGENTS carries, from
/// `smithy_query(query="shape", shape="DialerConfig")` — the dialers an update may move a
/// campaign between.
const AGENT_BASED_MEMBERS: [&str; 2] = ["progressiveDialerConfig", "predictiveDialerConfig"];

/// Every parameter the dialer members carry, from `smithy_query(query="shape",
/// shape="ProgressiveDialerConfig,PredictiveDialerConfig,AgentlessDialerConfig")`:
/// `bandwidthAllocation` (required of the two agent-based members) and the optional
/// `dialingCapacity`.
const DIALER_PARAMETERS: [&str; 2] = ["bandwidthAllocation", "dialingCapacity"];

/// `BandwidthAllocation`'s published range (`@range(min 0, max 1)`, from
/// `smithy_query(query="shape", shape="ProgressiveDialerConfig")`).
const BANDWIDTH_MIN: f64 = 0.0;
const BANDWIDTH_MAX: f64 = 1.0;

/// `DialingCapacity`'s published range (`@range(min 0.01, max 1)`, from
/// `smithy_query(query="shape", shape="AgentlessDialerConfig")`).
const CAPACITY_MIN: f64 = 0.01;
const CAPACITY_MAX: f64 = 1.0;

/// The `DialerConfig` naming `member` with `parameters`.
fn dialer(member: &str, parameters: Value) -> Value {
    let mut config = Map::new();
    config.insert(member.to_string(), parameters);
    Value::Object(config)
}

/// The config a campaign dialling with `member` is CREATED with here: the parameters that
/// member requires, so an update has something to move.
fn initial_config(member: &str) -> Value {
    if member == AGENTLESS_MEMBER {
        dialer(member, json!({"dialingCapacity": 0.9}))
    } else {
        dialer(member, json!({"bandwidthAllocation": 0.75}))
    }
}

/// The configs a campaign dialling with `member` is taken through in order: the parameter
/// combinations that member admits, the presence and absence of the optional
/// `dialingCapacity` among them, and each published bound of the parameters it carries.
fn parameter_rows(member: &str) -> Vec<(String, Value)> {
    let mut rows: Vec<(String, Value)> = vec![
        (
            format!("{member} with no optional parameter"),
            dialer(member, parameters(member, 0.6, None)),
        ),
        (
            format!("{member} with a dialing capacity at its published minimum"),
            dialer(member, parameters(member, 0.3, Some(CAPACITY_MIN))),
        ),
        (
            format!("{member} with a dialing capacity at its published maximum"),
            dialer(member, parameters(member, 0.3, Some(CAPACITY_MAX))),
        ),
        (
            format!("{member} with a dialing capacity between its bounds"),
            dialer(member, parameters(member, 0.45, Some(0.5))),
        ),
    ];
    if member != AGENTLESS_MEMBER {
        rows.push((
            format!("{member} with a bandwidth allocation at its published minimum"),
            dialer(member, parameters(member, BANDWIDTH_MIN, None)),
        ));
        rows.push((
            format!("{member} with a bandwidth allocation at its published maximum"),
            dialer(member, parameters(member, BANDWIDTH_MAX, Some(0.2))),
        ));
    }
    rows
}

/// The parameters of `member`: the agentless dialer carries only `dialingCapacity`, so
/// `bandwidth` is dropped for it, and `capacity` of `None` is the optional parameter left
/// out altogether.
fn parameters(member: &str, bandwidth: f64, capacity: Option<f64>) -> Value {
    let mut asked = Map::new();
    if member != AGENTLESS_MEMBER {
        asked.insert("bandwidthAllocation".to_string(), json!(bandwidth));
    }
    if let Some(capacity) = capacity {
        asked.insert("dialingCapacity".to_string(), json!(capacity));
    }
    Value::Object(asked)
}

/// The outbound-call config every campaign below is created with — one of the members an
/// update of the DIALER config must leave exactly where it is.
fn created_outbound_config() -> Value {
    json!({
        "connectContactFlowId": "flow-dialer-suite",
        "connectSourcePhoneNumber": "+15550123",
    })
}

/// A `CreateCampaign` body for `INSTANCE` dialling with `dialer_config`.
fn create_body(name: &str, dialer_config: &Value) -> Value {
    json!({
        "name": name,
        "connectInstanceId": INSTANCE,
        "dialerConfig": dialer_config,
        "outboundCallConfig": created_outbound_config(),
        "tags": {"owner": "dialer-config-suite"},
    })
}

/// Register `INSTANCE` with the account and create one campaign dialling with
/// `dialer_config`, handing back the minted campaign id.
async fn a_stored_campaign(router: &Harness, name: &str, dialer_config: &Value) -> String {
    router.register_connect_instance(REGION, INSTANCE);
    let created = call(
        router,
        REGION,
        "PUT",
        "/campaigns",
        &create_body(name, dialer_config),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: expected HTTP 200 creating the campaign to update, got {} {}",
        created.status, created.body
    );
    created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: no id in {}", created.body))
        .to_string()
}

/// Ask for `dialer_config` on the campaign `id` names.
async fn update(router: &Harness, id: &str, dialer_config: &Value) -> Answer {
    call(
        router,
        REGION,
        "POST",
        &format!("/campaigns/{id}/dialer-config"),
        &json!({"dialerConfig": dialer_config}),
    )
    .await
}

/// The campaign `id` names, as `DescribeCampaign` reports it.
async fn described(router: &Harness, id: &str) -> Value {
    let answer = call(
        router,
        REGION,
        "GET",
        &format!("/campaigns/{id}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case \"read the campaign back\": expected HTTP 200, got {} {}",
        answer.status, answer.body
    );
    answer.body["campaign"].clone()
}

/// The member of the `DialerConfig` union `config` names, with the parameters it carries.
/// Panics when `config` names no member, because a case that asks for no dialer at all is
/// a broken case.
fn named_member(config: &Value) -> (String, Value) {
    config
        .as_object()
        .and_then(|members| members.iter().next())
        .map(|(member, parameters)| (member.clone(), parameters.clone()))
        .unwrap_or_else(|| panic!("a dialer config names one union member: {config}"))
}

/// Assert `reported` is the dialer config `wanted`: the union member `wanted` names
/// carrying exactly the parameters it asks for, every parameter it omits reported as
/// absent, and no other member of the union reported at all — which is what makes the
/// update a replacement of the WHOLE config rather than a patch of the members it names.
fn assert_dialer_config(case: &str, reported: &Value, wanted: &Value) {
    let (member, asked) = named_member(wanted);
    for other in DIALER_MEMBERS.iter().filter(|other| **other != member) {
        let reported_other = reported.get(other).unwrap_or(&Value::Null);
        assert!(
            reported_other.is_null(),
            "case {case:?}: expected no {other} once {member} was asked for, got \
             {reported_other} in {reported}"
        );
    }
    for parameter in DIALER_PARAMETERS {
        let expected = asked.get(parameter).cloned().unwrap_or(Value::Null);
        let actual = reported
            .get(&member)
            .and_then(|parameters| parameters.get(parameter))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member}.{parameter} to be {expected}, got {actual} \
             in {reported}"
        );
    }
}

#[test]
fn every_dialer_member_the_model_declares_has_one_dialer_type_named_for_it() {
    assert_eq!(
        ALL_DIALER_TYPES.len(),
        DIALER_MEMBERS.len(),
        "expected one dialer type per modelled DialerConfig member, got {ALL_DIALER_TYPES:?} \
         against {DIALER_MEMBERS:?}"
    );
    for member in DIALER_MEMBERS {
        // Each member of the union is `<type>DialerConfig`, so the type the service names
        // it by is the member's own prefix.
        let named: Vec<&str> = ALL_DIALER_TYPES
            .iter()
            .map(|dialer| dialer.as_upper_wire())
            .filter(|name| member.starts_with(&name.to_lowercase()))
            .collect();
        assert_eq!(
            named.len(),
            1,
            "case {member:?}: expected exactly one dialer type named for this member, got \
             {named:?}"
        );
    }
}

#[tokio::test]
async fn an_update_replaces_the_parameters_of_the_dialer_the_campaign_was_created_with() {
    for member in DIALER_MEMBERS {
        let router = router();
        let created = initial_config(member);
        let id = a_stored_campaign(&router, member, &created).await;
        let before = described(&router, &id).await;
        assert_dialer_config(
            &format!("{member}: the config the create stored"),
            &before["dialerConfig"],
            &created,
        );

        // The campaign is put into a run state first, so an update is shown to be taken
        // by a campaign that is dialling and to leave the run state the lifecycle
        // operations own exactly where it was.
        let started = call(
            &router,
            REGION,
            "POST",
            &format!("/campaigns/{id}/start"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            started.status, 200,
            "case {member:?}: expected HTTP 200 starting the campaign before updating it, \
             got {} {}",
            started.status, started.body
        );

        for (case, wanted) in parameter_rows(member) {
            let updated = update(&router, &id, &wanted).await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: expected HTTP 200 from the update, got {} {}",
                updated.status, updated.body
            );

            let after = described(&router, &id).await;
            assert_dialer_config(&case, &after["dialerConfig"], &wanted);

            // Every other member of the campaign is the one the create stored.
            for stored_member in [
                "id",
                "arn",
                "name",
                "connectInstanceId",
                "outboundCallConfig",
                "tags",
            ] {
                assert_eq!(
                    after.get(stored_member),
                    before.get(stored_member),
                    "case {case:?}: expected {stored_member} to survive the update \
                     unchanged, got {after}"
                );
            }

            let state = call(
                &router,
                REGION,
                "GET",
                &format!("/campaigns/{id}/state"),
                &Value::Null,
            )
            .await;
            assert_eq!(
                (
                    state.status,
                    state
                        .body
                        .get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("<absent>")
                ),
                (200, "Running"),
                "case {case:?}: expected the run state to survive the update, got {} {}",
                state.status,
                state.body
            );
        }
    }
}

#[tokio::test]
async fn asking_for_the_config_a_campaign_already_carries_succeeds_and_changes_nothing() {
    for member in DIALER_MEMBERS {
        let router = router();
        let created = initial_config(member);
        let id = a_stored_campaign(&router, member, &created).await;

        // Rows differ along how a repeat is reached: the config the create stored, and
        // each config an update moves this campaign to in turn.
        let mut rows: Vec<(String, Value)> = vec![(
            format!("{member}: the config the create stored"),
            created.clone(),
        )];
        rows.extend(parameter_rows(member));

        for (case, wanted) in rows {
            for attempt in 1..=3 {
                let updated = update(&router, &id, &wanted).await;
                assert_eq!(
                    updated.status, 200,
                    "case {case:?} attempt {attempt}: expected HTTP 200, got {} {}",
                    updated.status, updated.body
                );
                let after = described(&router, &id).await;
                assert_dialer_config(&case, &after["dialerConfig"], &wanted);
            }
        }
    }
}

#[tokio::test]
async fn an_agent_based_campaign_takes_either_agent_based_dialer() {
    // Rows span both ordered pairs of the agent-based members: a campaign dialling
    // progressively takes the predictive dialer and the other way about, which is the
    // dialer-type move the recorded responses of this operation accept.
    for stored_member in AGENT_BASED_MEMBERS {
        for wanted_member in AGENT_BASED_MEMBERS
            .iter()
            .filter(|wanted| **wanted != stored_member)
        {
            let router = router();
            let case = format!("{stored_member} asked for {wanted_member}");
            let id = a_stored_campaign(&router, &case, &initial_config(stored_member)).await;

            for (case, wanted) in parameter_rows(wanted_member)
                .into_iter()
                .map(|(row, wanted)| (format!("{case}: {row}"), wanted))
            {
                let updated = update(&router, &id, &wanted).await;
                assert_eq!(
                    updated.status, 200,
                    "case {case:?}: expected HTTP 200 from the update, got {} {}",
                    updated.status, updated.body
                );
                let after = described(&router, &id).await;
                assert_dialer_config(&case, &after["dialerConfig"], &wanted);
            }
        }
    }
}

#[tokio::test]
async fn crossing_between_agentless_and_agent_based_dialling_is_refused_either_way() {
    let router = router();

    // One row per ORDERED pair that crosses between dialling with agents and dialling
    // without them — every way a request can cross it — each carrying the sentence this
    // service answers it with, in the service's own words (trailing space included).
    let rows: Vec<(&str, &str, &str)> = vec![
        (
            "progressiveDialerConfig",
            "agentlessDialerConfig",
            "Updating campaign dialer type from PROGRESSIVE to AGENTLESS is not allowed ",
        ),
        (
            "predictiveDialerConfig",
            "agentlessDialerConfig",
            "Updating campaign dialer type from PREDICTIVE to AGENTLESS is not allowed ",
        ),
        (
            "agentlessDialerConfig",
            "progressiveDialerConfig",
            "Updating campaign dialer type from AGENTLESS to PROGRESSIVE is not allowed ",
        ),
        (
            "agentlessDialerConfig",
            "predictiveDialerConfig",
            "Updating campaign dialer type from AGENTLESS to PREDICTIVE is not allowed ",
        ),
    ];

    for (stored_member, wanted_member, expected_message) in rows {
        let case = format!("{stored_member} asked for {wanted_member}");
        let created = initial_config(stored_member);
        let id = a_stored_campaign(&router, &case, &created).await;

        let refused = update(&router, &id, &initial_config(wanted_member)).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 for a crossing of the dialling kind, got {} {}",
            refused.status, refused.body
        );
        assert!(
            refused.error_code.ends_with("ValidationException"),
            "case {case:?}: expected ValidationException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            expected_message,
            "case {case:?}: expected this service's dialer-type refusal naming both types, \
             got {}",
            refused.body
        );

        // The refusal wrote nothing: the campaign still dials the way it was created.
        let after = described(&router, &id).await;
        assert_dialer_config(&case, &after["dialerConfig"], &created);
    }
}

#[tokio::test]
async fn updating_an_id_that_names_no_campaign_is_refused_and_stores_nothing() {
    let router = router();
    let created = initial_config("predictiveDialerConfig");
    let stored = a_stored_campaign(&router, "untouched-dialer", &created).await;

    // Rows span `CampaignId` (len 0..256, pattern `^[\S]*$`, from
    // `smithy_query(query="shape", shape="CampaignId")`): the uuid form, the shortest id a
    // path label can carry, punctuation the pattern admits, the longest id the bound
    // admits, the 32-hex form this emulator mints, and an id a delete has since dropped.
    let mut rows: Vec<(&str, String)> = vec![
        (
            "the uuid form",
            "3c6e1b58-0f2a-4c7d-9f11-6a2b4c8d0e35".to_string(),
        ),
        ("one character", "z".to_string()),
        ("punctuation the pattern admits", "a:b-D9.e_f".to_string()),
        ("256 characters (the length maximum)", "n".repeat(256)),
        (
            "the minted id form",
            "fedcba9876543210fedcba9876543210".to_string(),
        ),
    ];

    let dropped = a_stored_campaign(&router, "to-be-deleted-dialer", &created).await;
    let deleted = call(
        &router,
        REGION,
        "DELETE",
        &format!("/campaigns/{dropped}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case \"delete a campaign to update afterwards\": expected HTTP 200, got {} {}",
        deleted.status, deleted.body
    );
    rows.push(("an id a delete has dropped", dropped));

    for (case, id) in rows {
        let refused = update(&router, &id, &initial_config("progressiveDialerConfig")).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected HTTP 404 for an id no campaign carries, got {} {}",
            refused.status, refused.body
        );
        assert!(
            refused.error_code.ends_with("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            campaign_not_found_message(&id),
            "case {case:?}: expected this service's campaign-resource not-found wording \
             around the id the request carried, got {}",
            refused.body
        );
    }

    // None of those refusals touched the campaign that does exist.
    let after = described(&router, &stored).await;
    assert_dialer_config(
        "the stored campaign after every refusal",
        &after["dialerConfig"],
        &created,
    );
}

#[tokio::test]
async fn a_dialing_capacity_outside_its_published_range_is_refused_and_the_stored_config_stays() {
    let router = router();
    let created = dialer(
        "predictiveDialerConfig",
        json!({"bandwidthAllocation": 0.5, "dialingCapacity": 0.5}),
    );
    let id = a_stored_campaign(&router, "keeper-dialer", &created).await;

    // Rows span the members that carry a `dialingCapacity` and the ways a request can
    // miss what the model declares for it — just under its minimum, just over its
    // maximum, far over it — plus a member asked for without the parameter it requires.
    // The two agentless rows are refused for their parameter rather than for the dialling
    // kind they would cross: the model's own constraints are weighed before any handler
    // runs.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a predictive dialing capacity just under its minimum",
            json!({
                "predictiveDialerConfig": {
                    "bandwidthAllocation": 0.5,
                    "dialingCapacity": CAPACITY_MIN - 0.001,
                }
            }),
        ),
        (
            "a progressive dialing capacity just over its maximum",
            json!({
                "progressiveDialerConfig": {
                    "bandwidthAllocation": 0.5,
                    "dialingCapacity": CAPACITY_MAX + 0.001,
                }
            }),
        ),
        (
            "a predictive dialing capacity far over its maximum",
            json!({
                "predictiveDialerConfig": {
                    "bandwidthAllocation": 0.5,
                    "dialingCapacity": CAPACITY_MAX * 100.0,
                }
            }),
        ),
        (
            "an agentless dialing capacity just under its minimum",
            json!({"agentlessDialerConfig": {"dialingCapacity": CAPACITY_MIN - 0.001}}),
        ),
        (
            "an agentless dialing capacity far over its maximum",
            json!({"agentlessDialerConfig": {"dialingCapacity": CAPACITY_MAX * 100.0}}),
        ),
        (
            "the predictive member without its required bandwidth allocation",
            json!({"predictiveDialerConfig": {"dialingCapacity": 0.5}}),
        ),
        (
            "the progressive member without its required bandwidth allocation",
            json!({"progressiveDialerConfig": {"dialingCapacity": 0.5}}),
        ),
    ];

    for (case, wanted) in rows {
        let refused = update(&router, &id, &wanted).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400 for a value outside the published \
             constraints, got {} {}",
            refused.status, refused.body
        );
        assert!(
            refused.error_code.ends_with("ValidationException"),
            "case {case:?}: expected ValidationException, got {:?}",
            refused.error_code
        );
        let after = described(&router, &id).await;
        assert_dialer_config(case, &after["dialerConfig"], &created);
    }
}

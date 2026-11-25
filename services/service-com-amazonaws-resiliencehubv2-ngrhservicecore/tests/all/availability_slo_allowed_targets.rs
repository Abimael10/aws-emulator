// SPDX-License-Identifier: CC0-1.0

//! Which availability SLO target a policy may be written with, end to end over HTTP.
//!
//! The rule under test: the availability SLO target a resilience policy carries has to be
//! one of the discrete availability levels this service offers, so a `CreatePolicy` or
//! `UpdatePolicy` carrying one of them is served and one carrying any other value — even a
//! value inside `AvailabilitySlo$target`'s modelled `@range` 0..100 — is refused HTTP 400
//! `ValidationException`, naming the field and listing the offered levels, with nothing
//! written: no policy under the refused name, no `clientToken` consumed, and a stored
//! policy left exactly as it was.
//!
//! The suites are TABLES driving the real wire path (`MultiServiceRouter::dispatch`): rows
//! differ along the target value (each offered level; both ends of the modelled range; a
//! value just under, just over and between the offered levels; an integral value), along
//! which operation writes it (create, update) and along what else the refused request
//! carried (a `clientToken`, other target blocks).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::effective_policy_values::{call, emulator, get_policy};
use crate::policy_update_and_deletion::UPDATE_POLICY;

/// The modelled routes this suite drives.
const CREATE_POLICY: &str = "/v2/create-policy";
const LIST_POLICIES: &str = "/v2/list-policies";

/// The availability SLO targets the service offers, as percentages.
///
/// SOURCE: the service's own behaviour — `AvailabilitySlo$target`
/// (`smithy_query(query="shape", shape="AvailabilitySlo")`) is a `Double` bounded by
/// `@range min 0 max 100` and enumerates nothing, so the discrete levels are not in the
/// model. Written out here rather than imported from the crate, so this suite fails if the
/// service offers a different set.
const OFFERED_TARGETS: [f64; 3] = [99.9, 99.95, 99.99];

/// Values inside `AvailabilitySlo$target`'s modelled range that name no offered level.
///
/// Rows: both ends of the modelled range, the midpoint, a value just under the lowest
/// offered level, one between two offered levels, one just over the highest, and an
/// integral value a JSON body can carry for a `Double` member.
const UNOFFERED_TARGETS: [(&str, f64); 7] = [
    ("the modelled minimum", 0.0),
    ("the modelled maximum", 100.0),
    ("the middle of the modelled range", 50.0),
    ("just under the lowest offered level", 99.89),
    ("between two offered levels", 99.97),
    ("just over the highest offered level", 99.995),
    ("a whole-percent target", 99.0),
];

/// The refusal's sentence: fixed text naming the field the value arrived in, then the
/// offered levels separated by `", "`.
fn expected_message() -> String {
    let allowed = OFFERED_TARGETS
        .iter()
        .map(f64::to_string)
        .collect::<Vec<String>>()
        .join(", ");
    format!("Invalid input for field availabilitySlo.target. Allowed values are [{allowed}].")
}

/// The modelled error code of a refusal, from the restJson1 `__type` discriminator.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// Assert `answered` is the refusal this rule words, for `case`.
fn assert_refused(case: &str, status: u16, answered: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for a target the service does not offer, got \
         {status} {answered:?}"
    );
    let code = error_code(answered);
    assert_eq!(
        code, "ValidationException",
        "case {case:?}: expected ValidationException, got {code:?} in {answered:?}"
    );
    let message = answered["message"].as_str().unwrap_or("<no message>");
    assert_eq!(
        message,
        expected_message(),
        "case {case:?}: expected {:?}, got {message:?}",
        expected_message()
    );
}

async fn create_policy(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", CREATE_POLICY, Some(body)).await
}

async fn update_policy(
    router: &MultiServiceRouter,
    policy_arn: &str,
    patch: &Value,
) -> (u16, Value) {
    let mut body = patch.clone();
    body["policyArn"] = json!(policy_arn);
    call(router, "POST", UPDATE_POLICY, Some(body)).await
}

/// Every policy `ListPolicies` holds, as its `policySummaries` list.
async fn list_policy_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, answered) = call(router, "GET", LIST_POLICIES, None).await;
    assert_eq!(
        status, 200,
        "listing policies expected HTTP 200, got {status} {answered:?}"
    );
    answered["policySummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("no policySummaries in {answered:?}"))
        .iter()
        .filter_map(|summary| summary["name"].as_str().map(str::to_string))
        .collect()
}

/// RULE: a create carrying an offered availability level is served and the stored policy
/// reads that level back.
#[tokio::test]
async fn a_create_carrying_an_offered_availability_level_is_served() {
    let router = emulator();
    for (index, target) in OFFERED_TARGETS.into_iter().enumerate() {
        let case = format!("the offered level {target}");
        let name = format!("Offered-{index}");
        let (status, answered) = create_policy(
            &router,
            json!({"name": name, "availabilitySlo": {"target": target}}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an offered level must be served, got {status} {answered:?}"
        );
        assert_eq!(
            answered["policy"]["availabilitySlo"],
            json!({"target": target}),
            "case {case:?}: the create must echo the stored level, got {answered:?}"
        );
        let policy_arn = answered["policy"]["policyArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no policyArn in {answered:?}"))
            .to_string();
        let (read_status, read_back) = get_policy(&router, &policy_arn).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: reading the policy back expected HTTP 200, got {read_status} \
             {read_back:?}"
        );
        assert_eq!(
            read_back["policy"]["availabilitySlo"],
            json!({"target": target}),
            "case {case:?}: the read must answer the stored level, got {read_back:?}"
        );
    }
}

/// RULE: a create carrying a target the service does not offer is refused, and writes
/// nothing — the name is still free, the policy is in no listing, and a `clientToken` the
/// refused call carried is still unconsumed.
#[tokio::test]
async fn a_create_carrying_an_unoffered_target_is_refused_and_writes_nothing() {
    let router = emulator();
    for (index, (case, target)) in UNOFFERED_TARGETS.into_iter().enumerate() {
        let name = format!("Unoffered-{index}");
        let token = format!("token-{index}");
        let (status, answered) = create_policy(
            &router,
            json!({
                "name": name,
                "description": "a policy the service will not accept",
                "availabilitySlo": {"target": target},
                "multiAz": {"rtoInMinutes": 15, "rpoInMinutes": 5},
                "clientToken": token,
            }),
        )
        .await;
        assert_refused(case, status, &answered);

        let listed = list_policy_names(&router).await;
        assert!(
            !listed.contains(&name),
            "case {case:?}: the refused create must have stored nothing, but {name:?} is \
             listed in {listed:?}"
        );

        // The name is free and the token unconsumed: the SAME request with an offered
        // level is served, rather than meeting a conflict the refused call left behind.
        let (retry_status, retried) = create_policy(
            &router,
            json!({
                "name": name,
                "availabilitySlo": {"target": OFFERED_TARGETS[0]},
                "clientToken": token,
            }),
        )
        .await;
        assert_eq!(
            retry_status, 200,
            "case {case:?}: after the refusal the name and the token must still be free, \
             got {retry_status} {retried:?}"
        );
        assert_eq!(
            retried["policy"]["availabilitySlo"],
            json!({"target": OFFERED_TARGETS[0]}),
            "case {case:?}: the served create must store its own level, got {retried:?}"
        );
    }
}

/// RULE: the same rule holds for an update — a patch naming an offered level is applied,
/// and one naming any other value is refused and leaves the stored policy as it was.
#[tokio::test]
async fn an_update_is_held_to_the_same_offered_levels() {
    let router = emulator();
    let stored_target = OFFERED_TARGETS[1];
    let (status, created) = create_policy(
        &router,
        json!({
            "name": "Update-Slo-Rule",
            "availabilitySlo": {"target": stored_target},
            "dataRecovery": {"timeBetweenBackupsInMinutes": 60},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the policy under test must be created, got {status} {created:?}"
    );
    let policy_arn = created["policy"]["policyArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no policyArn in {created:?}"))
        .to_string();

    for (case, target) in UNOFFERED_TARGETS {
        let (status, answered) =
            update_policy(&router, &policy_arn, &json!({"availabilitySlo": {"target": target}}))
                .await;
        assert_refused(case, status, &answered);

        // The refused update wrote nothing: the policy still carries the level it was
        // created with, and the block the patch did not name is untouched too.
        let (read_status, read_back) = get_policy(&router, &policy_arn).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: reading the policy back expected HTTP 200, got {read_status} \
             {read_back:?}"
        );
        assert_eq!(
            read_back["policy"]["availabilitySlo"],
            json!({"target": stored_target}),
            "case {case:?}: the refused update must leave the stored level, got \
             {read_back:?}"
        );
        assert_eq!(
            read_back["policy"]["dataRecovery"],
            json!({"timeBetweenBackupsInMinutes": 60}),
            "case {case:?}: the refused update must leave every other block, got \
             {read_back:?}"
        );
    }

    // Each offered level IS applied, so the refusals above are about the value and not
    // about the operation.
    for target in OFFERED_TARGETS {
        let case = format!("an update to the offered level {target}");
        let (status, answered) =
            update_policy(&router, &policy_arn, &json!({"availabilitySlo": {"target": target}}))
                .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an offered level must be applied, got {status} {answered:?}"
        );
        assert_eq!(
            answered["policy"]["availabilitySlo"],
            json!({"target": target}),
            "case {case:?}: the update must answer the level it applied, got {answered:?}"
        );
        let (_, read_back) = get_policy(&router, &policy_arn).await;
        assert_eq!(
            read_back["policy"]["availabilitySlo"],
            json!({"target": target}),
            "case {case:?}: the read must answer the applied level, got {read_back:?}"
        );
    }
}

/// RULE: a target the model's own `@range` refuses is still reported as the schema finding
/// it is, so this service's narrower rule does not take over a refusal the request schema
/// already owns.
#[tokio::test]
async fn a_target_outside_the_modelled_range_keeps_its_schema_refusal() {
    let router = emulator();
    // (case, the target, the text the schema's own report carries)
    let rows: [(&str, f64, &str); 2] = [
        ("above the modelled maximum", 100.5, "maximum"),
        ("below the modelled minimum", -0.5, "minimum"),
    ];
    for (index, (case, target, expected_fragment)) in rows.into_iter().enumerate() {
        let (status, answered) = create_policy(
            &router,
            json!({
                "name": format!("Out-Of-Range-{index}"),
                "availabilitySlo": {"target": target},
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {answered:?}"
        );
        let code = error_code(&answered);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code:?} in {answered:?}"
        );
        let message = answered["message"].as_str().unwrap_or("<no message>");
        assert!(
            message.contains(expected_fragment),
            "case {case:?}: expected the schema's own report naming the \
             {expected_fragment:?} bound, got {message:?}"
        );
    }
}

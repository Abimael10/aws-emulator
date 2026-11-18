// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service recites a multi-member constraint report in, end to end over the
//! restJson1 wire.
//!
//! The rule under test, one level above the one request that filed the issue:
//!
//! > A report that carries one clause per offending member recites those clauses in the
//! > order the SERVICE traverses the containing structure's members, and each enumerated
//! > clause's permitted value set in the order the service recites that enum's values —
//! > neither of which is the order the model declares them in. The clauses themselves, the
//! > envelope around them and the withholding of the rejected value are unchanged.
//!
//! The rows differ along what the rule generalises over: HOW MANY of the structure's
//! members offend (one, two, all three) and WHICH of them, and — for the value set — the
//! member the clause is stated at. The orders are transcribed here, independently of the
//! service's own tables, so a drift in either fails this suite.

use serde_json::{json, Value};

use crate::error_contract::{call, router};

/// The order this service recites `AuthenticationType`'s values in, which is NOT the
/// order the model declares them (`API_KEY`, `AWS_IAM`, `AMAZON_COGNITO_USER_POOLS`,
/// `OPENID_CONNECT`, `AWS_LAMBDA`; `smithy_query(query="shape",
/// shape="AuthenticationType")`).
const RECITED_AUTHENTICATION_TYPES: &[&str] = &[
    "AWS_LAMBDA",
    "AMAZON_COGNITO_USER_POOLS",
    "OPENID_CONNECT",
    "AWS_IAM",
    "API_KEY",
];

/// The order this service recites an `EventConfig`'s authorization-mode members in, which
/// is NOT the order the model declares them (`authProviders`, `connectionAuthModes`,
/// `defaultPublishAuthModes`, `defaultSubscribeAuthModes`;
/// `smithy_query(query="shape", shape="EventConfig")`).
const RECITED_MEMBERS: &[&str] = &[
    "defaultPublishAuthModes",
    "connectionAuthModes",
    "defaultSubscribeAuthModes",
];

/// A value no `AuthenticationType` holds, so a mode carrying it is outside the value set.
const UNMODELLED_AUTH_TYPE: &str = "NOT_AN_AUTH_TYPE";

/// The clause this surface states for the member at `path`.
fn clause(path: &str) -> String {
    format!(
        "Value at '{path}' failed to satisfy constraint: Member must satisfy enum value set: \
         [{}]",
        RECITED_AUTHENTICATION_TYPES.join(", ")
    )
}

/// The path a clause names for the offending entry of `member`'s list, as the constraint
/// walk spells it: the member, the entry's position ([`OFFENDING_POSITION`], 1-based), the
/// collection step, then the member of the entry that offended.
fn path_of(member: &str) -> String {
    format!("eventConfig.{member}.{OFFENDING_POSITION}.member.authType")
}

/// Where in each list the offending mode sits: the second entry, so the clause's path has
/// a position to state and the list also holds a well-formed mode.
const OFFENDING_POSITION: usize = 2;

/// A `CreateApi` body whose auth-mode lists each hold one well-formed mode, plus the
/// unmodelled one for every member in `offending`.
fn body(name: &str, offending: &[&str]) -> Value {
    let mut event_config = json!({
        "authProviders": [{"authType": "API_KEY"}],
        "connectionAuthModes": [{"authType": "API_KEY"}],
        "defaultPublishAuthModes": [{"authType": "API_KEY"}],
        "defaultSubscribeAuthModes": [{"authType": "API_KEY"}],
    });
    for member in offending {
        event_config[*member] = json!([
            {"authType": "API_KEY"},
            {"authType": UNMODELLED_AUTH_TYPE},
        ]);
    }
    json!({"name": name, "eventConfig": event_config})
}

/// The report this surface states over the offending members, in the order the service
/// recites them.
fn report(offending: &[&str]) -> String {
    let mut recited: Vec<&str> = RECITED_MEMBERS
        .iter()
        .copied()
        .filter(|member| offending.contains(member))
        .collect();
    recited.dedup();
    let clauses: Vec<String> = recited
        .iter()
        .map(|member| clause(&path_of(member)))
        .collect();
    match clauses.len() {
        1 => format!("1 validation error detected: {}", clauses[0]),
        n => format!("{n} validation errors detected: {}", clauses.join("; ")),
    }
}

/// RULE: the clauses are recited in the service's order and each permitted set in the
/// service's order, however many members offend.
#[tokio::test]
async fn a_multi_member_report_is_recited_in_the_services_order() {
    let router = router();

    // The rows differ along which of the three members offend, and how many: every
    // single member, two of them in each pairing the model's own order would invert, and
    // all three.
    let rows: &[&[&str]] = &[
        &["defaultPublishAuthModes"],
        &["connectionAuthModes"],
        &["defaultSubscribeAuthModes"],
        &["connectionAuthModes", "defaultPublishAuthModes"],
        &["connectionAuthModes", "defaultSubscribeAuthModes"],
        &["defaultSubscribeAuthModes", "defaultPublishAuthModes"],
        &[
            "connectionAuthModes",
            "defaultPublishAuthModes",
            "defaultSubscribeAuthModes",
        ],
    ];

    for (index, offending) in rows.iter().enumerate() {
        let case = format!("the offending members {offending:?}");
        let answer = call(
            &router,
            "POST",
            "/v2/apis",
            Some(body(&format!("Events_order_{index}"), offending)),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected status 400, got {answer:?}"
        );
        let expected = report(offending);
        assert_eq!(
            answer.message, expected,
            "case {case:?}: expected the report {expected:?}, got {answer:?}"
        );
        assert!(
            !answer.message.contains(UNMODELLED_AUTH_TYPE),
            "case {case:?}: expected the rejected value withheld, got {answer:?}"
        );
        assert!(
            answer.body["api"].is_null(),
            "case {case:?}: a refused create answers no API, got {answer:?}"
        );
    }
}

/// RULE, stated the other way: the report does NOT recite the model's own orders — not
/// the declaration order of the value set, and not the model's member order.
#[tokio::test]
async fn the_report_does_not_recite_the_models_own_orders() {
    let router = router();
    let offending = [
        "connectionAuthModes",
        "defaultPublishAuthModes",
        "defaultSubscribeAuthModes",
    ];
    let answer = call(
        &router,
        "POST",
        "/v2/apis",
        Some(body("Events_order_models", &offending)),
    )
    .await;
    assert_eq!(
        answer.status, 400,
        "case \"all three members offending\": expected status 400, got {answer:?}"
    );

    // The model's declaration order of the value set.
    let declared = "[API_KEY, AWS_IAM, AMAZON_COGNITO_USER_POOLS, OPENID_CONNECT, AWS_LAMBDA]";
    assert!(
        !answer.message.contains(declared),
        "case \"all three members offending\": expected the recited value set rather than \
         the declared one, got {answer:?}"
    );

    // Every permitted value is still listed, so the ordering above withholds none of them.
    for value in RECITED_AUTHENTICATION_TYPES {
        assert!(
            answer.message.contains(value),
            "case \"all three members offending\": expected the permitted value {value:?} \
             listed, got {answer:?}"
        );
    }

    // The clause of the member the model declares FIRST is not the clause the report
    // states first.
    let first = answer
        .message
        .find(&path_of("connectionAuthModes"))
        .unwrap_or_default();
    let publish = answer
        .message
        .find(&path_of("defaultPublishAuthModes"))
        .unwrap_or_default();
    assert!(
        publish < first,
        "case \"all three members offending\": expected the publishing modes recited \
         before the connection modes, got {answer:?}"
    );
}

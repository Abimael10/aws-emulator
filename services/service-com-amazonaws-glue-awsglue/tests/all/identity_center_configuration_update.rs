// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `UpdateGlueIdentityCenterConfiguration`, read back through
//! `GetGlueIdentityCenterConfiguration`.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: the update addresses the account's SINGLETON configuration
//! ([`crate::identity_center_configuration`] pins the singleton itself), so each member the
//! request SENDS replaces the stored value of that member, a member it omits keeps the value
//! the last write gave it, and the application and instance the create registered are not
//! inputs of the update and survive it untouched. An account holding NO configuration has
//! nothing to modify, so the update is refused with `EntityNotFoundException` — the same
//! miss the read of the absent singleton answers — and writes nothing.
//!
//! The rows below differ along what the update carries (scopes only, background sessions
//! only, both, neither), how many scopes it sets (one, several, the modelled maximum),
//! whether the create had set that member at all, and how the account came to hold no
//! configuration (never created, or deleted).

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::messages::IDENTITY_CENTER_CONFIGURATION_NOT_FOUND;
use smithy_server_runtime::MultiServiceRouter;

use crate::identity_center_configuration::{emulator_holding, INSTANCE_ARN, INSTANCE_ID};
use crate::wire::{call, error_code, error_message, ok};

/// Read the configuration, answering the HTTP status and body.
async fn read(router: &MultiServiceRouter) -> (u16, Value) {
    call(router, "GetGlueIdentityCenterConfiguration", &json!({})).await
}

/// The configuration the account holds, which the read must serve.
async fn registered(router: &MultiServiceRouter, case: &str) -> Value {
    let (status, body) = read(router).await;
    assert_eq!(
        status, 200,
        "case {case}: the registered configuration must be readable, got {status} with {body}"
    );
    body
}

/// `max` scopes, as many as the modelled `Scopes` list takes
/// (`smithy_query(query="operation",operation="UpdateGlueIdentityCenterConfiguration")`
/// reports `IdentityCenterScopesList` with length 1..50), each a distinct value.
fn many_scopes(max: usize) -> Value {
    Value::Array(
        (0..max)
            .map(|index| Value::String(format!("glue:scope{index}")))
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// What an update writes, and what it leaves alone
// ---------------------------------------------------------------------------

/// Each member the update sends replaces the stored value of that member; a member it omits
/// keeps the value the last write gave it, and the `ApplicationArn` and `InstanceArn` the
/// create registered are untouched by every update.
#[tokio::test]
async fn an_update_replaces_the_members_it_sends_and_keeps_the_rest() {
    // (case, what the create set beside InstanceArn, the update, the Scopes and the
    //  UserBackgroundSessionsEnabled the read must report afterwards)
    let cases: [(&str, Value, Value, Value, Value); 7] = [
        (
            "scopes replaced, background sessions untouched",
            json!({ "Scopes": ["glue:read"], "UserBackgroundSessionsEnabled": false }),
            json!({ "Scopes": ["glue:write", "glue:read"] }),
            json!(["glue:write", "glue:read"]),
            json!(false),
        ),
        (
            "background sessions flipped, scopes untouched",
            json!({ "Scopes": ["glue:read"], "UserBackgroundSessionsEnabled": false }),
            json!({ "UserBackgroundSessionsEnabled": true }),
            json!(["glue:read"]),
            json!(true),
        ),
        (
            "both members set on a configuration created with neither",
            json!({}),
            json!({
                "Scopes": ["glue:read", "glue:write", "lakeformation:getDataAccess"],
                "UserBackgroundSessionsEnabled": true
            }),
            json!(["glue:read", "glue:write", "lakeformation:getDataAccess"]),
            json!(true),
        ),
        (
            "scopes set on a configuration created without them",
            json!({ "UserBackgroundSessionsEnabled": true }),
            json!({ "Scopes": ["glue:read"] }),
            json!(["glue:read"]),
            json!(true),
        ),
        (
            "an update that sends nothing changes nothing",
            json!({ "Scopes": ["glue:read"], "UserBackgroundSessionsEnabled": true }),
            json!({}),
            json!(["glue:read"]),
            json!(true),
        ),
        (
            "several scopes replaced by one",
            json!({ "Scopes": ["glue:read", "glue:write"] }),
            json!({ "Scopes": ["lakeformation:getDataAccess"] }),
            json!(["lakeformation:getDataAccess"]),
            Value::Null,
        ),
        (
            "the modelled maximum number of scopes",
            json!({ "Scopes": ["glue:read"] }),
            json!({ "Scopes": many_scopes(50), "UserBackgroundSessionsEnabled": false }),
            many_scopes(50),
            json!(false),
        ),
    ];

    for (case, created_with, update, expected_scopes, expected_sessions) in cases {
        let router = emulator_holding(&[INSTANCE_ID]);
        let mut create = json!({ "InstanceArn": INSTANCE_ARN });
        for (member, value) in created_with.as_object().expect("the row carries an object") {
            create[member] = value.clone();
        }
        let created = ok(&router, "CreateGlueIdentityCenterConfiguration", &create).await;
        let before = registered(&router, case).await;

        let answered = ok(
            &router,
            "UpdateGlueIdentityCenterConfiguration",
            &update,
        )
        .await;
        assert_eq!(
            answered,
            json!({}),
            "case {case}: the update models no output member, got {answered}"
        );

        let after = registered(&router, case).await;
        assert_eq!(
            after["Scopes"], expected_scopes,
            "case {case}: expected the scopes {expected_scopes} after the update {update}, got \
             {after}"
        );
        assert_eq!(
            after["UserBackgroundSessionsEnabled"], expected_sessions,
            "case {case}: expected UserBackgroundSessionsEnabled {expected_sessions} after the \
             update {update}, got {after}"
        );
        assert_eq!(
            after["ApplicationArn"], created["ApplicationArn"],
            "case {case}: the update must leave the application the create minted in place; \
             expected {}, got {after}",
            created["ApplicationArn"]
        );
        assert_eq!(
            after["InstanceArn"], before["InstanceArn"],
            "case {case}: the update must leave the instance the create named in place; \
             expected {}, got {after}",
            before["InstanceArn"]
        );

        // A read changes nothing, and neither does repeating the very same update: the
        // configuration the update wrote is what every later read reports.
        ok(
            &router,
            "UpdateGlueIdentityCenterConfiguration",
            &update,
        )
        .await;
        let again = registered(&router, case).await;
        assert_eq!(
            again, after,
            "case {case}: repeating the same update must leave the configuration as it was; \
             expected {after}, got {again}"
        );
    }
}

/// Updates applied one after another each leave their own members: the LAST write of a
/// member is what the read reports, and a member no update ever sent is still the create's.
#[tokio::test]
async fn the_last_update_of_each_member_is_what_the_read_reports() {
    let router = emulator_holding(&[INSTANCE_ID]);
    ok(
        &router,
        "CreateGlueIdentityCenterConfiguration",
        &json!({ "InstanceArn": INSTANCE_ARN, "Scopes": ["glue:read"] }),
    )
    .await;

    // (case, the update, the Scopes and the UserBackgroundSessionsEnabled expected after it)
    let steps: [(&str, Value, Value, Value); 4] = [
        (
            "first update sets the sessions flag",
            json!({ "UserBackgroundSessionsEnabled": true }),
            json!(["glue:read"]),
            json!(true),
        ),
        (
            "second update replaces the scopes",
            json!({ "Scopes": ["glue:write"] }),
            json!(["glue:write"]),
            json!(true),
        ),
        (
            "third update clears the sessions flag",
            json!({ "UserBackgroundSessionsEnabled": false }),
            json!(["glue:write"]),
            json!(false),
        ),
        (
            "fourth update sets both",
            json!({ "Scopes": ["glue:read", "glue:write"], "UserBackgroundSessionsEnabled": true }),
            json!(["glue:read", "glue:write"]),
            json!(true),
        ),
    ];

    for (case, update, expected_scopes, expected_sessions) in steps {
        ok(
            &router,
            "UpdateGlueIdentityCenterConfiguration",
            &update,
        )
        .await;
        let after = registered(&router, case).await;
        assert_eq!(
            after["Scopes"], expected_scopes,
            "case {case}: expected the scopes {expected_scopes}, got {after}"
        );
        assert_eq!(
            after["UserBackgroundSessionsEnabled"], expected_sessions,
            "case {case}: expected UserBackgroundSessionsEnabled {expected_sessions}, got {after}"
        );
    }
}

// ---------------------------------------------------------------------------
// There is nothing to update until a create registers it
// ---------------------------------------------------------------------------

/// An update of an account that holds no configuration is refused with
/// `EntityNotFoundException` naming the missing application, and writes nothing: the account
/// still reports no configuration, and a create that follows is accepted.
///
/// The rows differ along how the account came to hold none and along what the refused update
/// carries.
#[tokio::test]
async fn an_update_without_a_configuration_is_refused_and_writes_nothing() {
    // (case, whether a configuration is created and deleted first, the update)
    let cases: [(&str, bool, Value); 4] = [
        ("never created, empty update", false, json!({})),
        (
            "never created, scopes sent",
            false,
            json!({ "Scopes": ["glue:read"] }),
        ),
        (
            "deleted, scopes and sessions sent",
            true,
            json!({ "Scopes": ["glue:write"], "UserBackgroundSessionsEnabled": true }),
        ),
        (
            "deleted, sessions sent",
            true,
            json!({ "UserBackgroundSessionsEnabled": false }),
        ),
    ];

    for (case, deleted_first, update) in cases {
        let router = emulator_holding(&[INSTANCE_ID]);
        if deleted_first {
            ok(
                &router,
                "CreateGlueIdentityCenterConfiguration",
                &json!({ "InstanceArn": INSTANCE_ARN, "Scopes": ["glue:read"] }),
            )
            .await;
            ok(
                &router,
                "DeleteGlueIdentityCenterConfiguration",
                &json!({}),
            )
            .await;
        }

        let (status, body) = call(&router, "UpdateGlueIdentityCenterConfiguration", &update).await;
        assert_eq!(
            status, 400,
            "case {case}: an account with no configuration must refuse the update, got {status} \
             with {body}"
        );
        assert!(
            error_code(&body).ends_with("EntityNotFoundException"),
            "case {case}: expected EntityNotFoundException, got {} in {body}",
            error_code(&body)
        );
        assert_eq!(
            error_message(&body),
            IDENTITY_CENTER_CONFIGURATION_NOT_FOUND,
            "case {case}: the refusal must name the missing Identity Center application, got \
             {body}"
        );

        // The refused update wrote nothing: the account is still the not-configured one, and
        // that is the state a create is accepted from.
        let (status, after) = read(&router).await;
        assert_eq!(
            status, 400,
            "case {case}: a refused update must register nothing, got {status} with {after}"
        );
        let created = ok(
            &router,
            "CreateGlueIdentityCenterConfiguration",
            &json!({ "InstanceArn": INSTANCE_ARN }),
        )
        .await;
        assert!(
            created["ApplicationArn"].is_string(),
            "case {case}: the create after a refused update must mint an application, got \
             {created}"
        );
    }
}

/// An update the boundary refuses for breaking a modelled constraint on `Scopes` leaves the
/// stored configuration exactly as it was: the list takes 1..50 entries
/// (`smithy_query(query="operation",operation="UpdateGlueIdentityCenterConfiguration")`), so
/// the rows are the two values just outside those bounds.
#[tokio::test]
async fn an_update_breaking_the_scopes_constraint_changes_nothing() {
    // (case, the Scopes the update sends)
    let cases: [(&str, Value); 2] = [
        ("no scopes at all, one below the minimum", json!([])),
        (
            "fifty-one scopes, one above the maximum",
            many_scopes(51),
        ),
    ];

    for (case, scopes) in cases {
        let router = emulator_holding(&[INSTANCE_ID]);
        ok(
            &router,
            "CreateGlueIdentityCenterConfiguration",
            &json!({
                "InstanceArn": INSTANCE_ARN,
                "Scopes": ["glue:read"],
                "UserBackgroundSessionsEnabled": true
            }),
        )
        .await;
        let before = registered(&router, case).await;

        let (status, body) = call(
            &router,
            "UpdateGlueIdentityCenterConfiguration",
            &json!({ "Scopes": scopes }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case}: a Scopes list outside 1..50 must be refused, got {status} with {body}"
        );

        let after = registered(&router, case).await;
        assert_eq!(
            after, before,
            "case {case}: a refused update must change nothing; expected {before}, got {after}"
        );
    }
}

// SPDX-License-Identifier: CC0-1.0

//! What authorizes the user's OWN threat-protection feedback submission
//! (`UpdateAuthEventFeedback`), over the wire (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.UpdateAuthEventFeedback`,
//! unsigned — the operation is `@optionalAuth` with an empty auth list, so the holder
//! of a notification link calls it with no credential).
//!
//! The rules under test:
//!
//! * the request is authorized by the `FeedbackToken` Amazon Cognito minted for ONE
//!   event of ONE user, so the whole triple it names — user, event, token — is one
//!   question, and a request that binds to no such token is refused on the session it
//!   presented (HTTP 400, `NotAuthorizedException` in the body's `__type` and in the
//!   `x-amzn-errortype` header, carrying the fixed invalid-session sentence) rather
//!   than on whether the user or the event it named exists;
//! * that refusal does not depend on WHICH part of the triple failed, nor on HOW MANY
//!   parts failed, nor on the `AdvancedSecurityModeType` value the pool activated
//!   threat protection under: every such request is answered identically, so the
//!   refusal reports nothing about who is a member of the pool;
//! * while the pool-level add-on gate keeps its own class, because it is a property of
//!   the POOL and not of the user asked about: a pool running no threat protection
//!   answers both the submission and the history reader
//!   (`AdminListUserAuthEvents`) with `UserPoolAddOnNotEnabledException`, for a user it
//!   holds and for a name it does not alike.
//!
//! The rows span what the rules generalise over rather than one request: every
//! `AdvancedSecurityModeType` value that ACTIVATES threat protection (`AUDIT` and
//! `ENFORCED`, from `smithy_query(query="shape",shape="AdvancedSecurityModeType")`,
//! whose third value `OFF` is the one the gate above answers), every combination of the
//! triple with at least one part unbound (eight combinations less the one that binds),
//! both `FeedbackValueType` verdicts
//! (`smithy_query(query="shape",shape="FeedbackValueType")`), and — for the pool gate —
//! a pool that stores the off mode beside one that stores no `UserPoolAddOns` at all.
//!
//! Every pool, app client, user, event and feedback token here is real: they come from
//! `CreateUserPool`, `CreateUserPoolClient`, `AdminCreateUser`, `AdminSetUserPassword`
//! and `InitiateAuth`, and the stored feedback is read back only through
//! `AdminListUserAuthEvents`.
//!
//! Every assertion names its case and reports the value it saw.

use serde_json::json;

use crate::auth_event_feedback_refusal_masking::{
    dispatch, error_code, error_message, event_ids, feedback_token, given_pool, given_user,
    history, ok_anonymous, router_and_engine, sign_in, unresolved_usernames, Answered,
    ABSENT_EVENT_ID, FEEDBACK_TOKEN_INVALID_MESSAGE, FOREIGN_FEEDBACK_TOKEN,
    FOREIGN_POOL_USERNAME, OTHER_USERNAME, REFUSED, USERNAME,
    USER_POOL_ADD_ONS_NOT_ENABLED_MESSAGE,
};

/// The `AdvancedSecurityModeType` values that ACTIVATE threat protection, so a pool
/// stored under either of them logs events and serves this surface
/// (`smithy_query(query="shape",shape="AdvancedSecurityModeType")` lists
/// `OFF | AUDIT | ENFORCED`; `OFF` is the one that activates nothing).
const ACTIVATING_MODES: [&str; 2] = ["AUDIT", "ENFORCED"];

/// The value a pool stores to run no threat protection at all, the other half of that
/// enum.
const OFF_MODE: &str = "OFF";

/// Both `FeedbackValueType` verdicts, which a submission is refused the same way under
/// (`smithy_query(query="shape",shape="FeedbackValueType")`).
const VERDICTS: [&str; 2] = ["Valid", "Invalid"];

/// The username axis of every table here: the user the pool under test holds, beside
/// every form that resolves to no user of it
/// ([`unresolved_usernames`], which spans `UsernameType`'s published length bounds and
/// the ways a name resolves to nothing).
fn username_rows() -> Vec<(String, String)> {
    let mut rows = vec![("a user the pool holds".to_string(), USERNAME.to_string())];
    rows.extend(unresolved_usernames());
    rows
}

/// RULE: a `UpdateAuthEventFeedback` request that binds to no `FeedbackToken` this pool
/// minted is refused on the SESSION it presented — HTTP 400, `NotAuthorizedException`,
/// the fixed invalid-session sentence — identically for every way and every number of
/// ways the triple it names fails to bind, and under every add-on mode that activates
/// threat protection.
#[tokio::test]
async fn the_session_refusal_is_one_answer_under_every_activating_add_on_mode() {
    let (router, engine) = router_and_engine();

    // (case, answer) for every refused row of every mode, so one comparison at the end
    // shows the whole table is answered by a single byte-identical refusal.
    let mut answers: Vec<(String, Answered)> = Vec::new();

    for mode in ACTIVATING_MODES {
        let fixture = given_pool(&router, &format!("session-gate-{mode}"), Some(mode)).await;
        given_user(&router, &fixture.user_pool_id, USERNAME).await;
        given_user(&router, &fixture.user_pool_id, OTHER_USERNAME).await;
        sign_in(&router, &fixture.client_id, USERNAME).await;
        sign_in(&router, &fixture.client_id, OTHER_USERNAME).await;

        let mine = event_ids(&router, &fixture.user_pool_id, USERNAME).await;
        let subject = mine
            .first()
            .unwrap_or_else(|| panic!("mode {mode}: the sign-in must log one event, got {mine:?}"))
            .clone();
        let my_token = feedback_token(&engine, &fixture.user_pool_id, USERNAME, &subject);

        // A second pool holding the name one row names, so that row really is a name
        // ANOTHER pool holds a user for rather than one nothing created.
        let elsewhere =
            given_pool(&router, &format!("session-gate-elsewhere-{mode}"), Some(mode)).await;
        given_user(&router, &elsewhere.user_pool_id, FOREIGN_POOL_USERNAME).await;

        // Every combination of the triple with at least one part unbound: the username
        // names a user of the pool or nothing, the event id names an event of that
        // user's history or nothing, and the token is the one minted for the named event
        // or one minted for no event at all.
        for (user_case, user_name) in username_rows() {
            let named_user = user_name.as_str();
            for named_event in [subject.as_str(), ABSENT_EVENT_ID] {
                for (token_case, token) in [
                    ("its own token", my_token.as_str()),
                    ("a token minted for no event", FOREIGN_FEEDBACK_TOKEN),
                ] {
                    let binds = named_user == USERNAME
                        && named_event == subject.as_str()
                        && token == my_token.as_str();
                    if binds {
                        // The one combination that DOES bind is the accepted submission
                        // below, not a refusal.
                        continue;
                    }
                    for verdict in VERDICTS {
                        let case = format!(
                            "mode {mode}, {user_case} ({named_user:?}), event \
                             {named_event:?}, {token_case}, submitting {verdict}"
                        );
                        let answered = dispatch(
                            &router,
                            "UpdateAuthEventFeedback",
                            &json!({
                                "UserPoolId": &fixture.user_pool_id,
                                "Username": named_user,
                                "EventId": named_event,
                                "FeedbackToken": token,
                                "FeedbackValue": verdict,
                            }),
                            false,
                        )
                        .await;
                        assert_eq!(
                            (
                                answered.status,
                                error_code(&answered.payload),
                                error_message(&answered.payload),
                                answered.errortype.as_deref(),
                            ),
                            (
                                REFUSED,
                                "NotAuthorizedException",
                                FEEDBACK_TOKEN_INVALID_MESSAGE,
                                Some("NotAuthorizedException:"),
                            ),
                            "case {case:?}: expected the invalid-session refusal, got \
                             {answered:?}"
                        );
                        answers.push((case, answered));
                    }
                }
            }
        }

        // No refused row recorded anything on any event of this pool.
        for username in [USERNAME, OTHER_USERNAME] {
            let events = history(&router, &fixture.user_pool_id, username).await;
            assert!(
                events
                    .iter()
                    .all(|event| event.get("EventFeedback").is_none()),
                "mode {mode}, user {username}: a refused submission must record no feedback, \
                 got {events:?}"
            );
        }

        // The combination that DOES bind is served, under this mode too, and what it
        // asserts is readable through the history: the refusals above are the token
        // gate, not a blanket refusal of the surface.
        ok_anonymous(
            &router,
            "UpdateAuthEventFeedback",
            &json!({
                "UserPoolId": &fixture.user_pool_id,
                "Username": USERNAME,
                "EventId": &subject,
                "FeedbackToken": &my_token,
                "FeedbackValue": "Invalid",
            }),
        )
        .await;
        let events = history(&router, &fixture.user_pool_id, USERNAME).await;
        let recorded = events
            .iter()
            .find(|event| event["EventId"] == json!(subject))
            .unwrap_or_else(|| panic!("mode {mode}: the event fed back on, got {events:?}"));
        assert_eq!(
            (
                recorded["EventFeedback"]["FeedbackValue"].as_str(),
                recorded["EventFeedback"]["Provider"].as_str(),
            ),
            (Some("Invalid"), Some("User")),
            "mode {mode}: the token minted for the event must record the user's verdict, got \
             {recorded}"
        );
    }

    // The masking itself: nothing in the answer varies with which part of the triple
    // failed, with how many parts failed, or with the mode the pool activated threat
    // protection under.
    let (first_case, first) = answers.first().expect("the table has rows").clone();
    for (case, answered) in &answers {
        assert_eq!(
            *answered, first,
            "case {case:?}: expected the answer of case {first_case:?} ({first:?}), got \
             {answered:?}"
        );
    }
}

/// RULE: a pool that activated no threat protection reports that property of the POOL —
/// HTTP 400, `UserPoolAddOnNotEnabledException`, one fixed sentence — for the user's own
/// submission and for the history reader alike, before either resolves the user or the
/// session the request named.
///
/// The rows span the two ways a pool runs no threat protection (it stores the off mode,
/// or it stores no `UserPoolAddOns` at all) and whether the username names a user the
/// pool holds.
#[tokio::test]
async fn a_pool_without_threat_protection_answers_both_surfaces_on_its_add_ons() {
    let (router, _engine) = router_and_engine();
    let off = given_pool(&router, "session-gate-off", Some(OFF_MODE)).await;
    let plain = given_pool(&router, "session-gate-plain", None).await;
    given_user(&router, &off.user_pool_id, USERNAME).await;
    given_user(&router, &plain.user_pool_id, USERNAME).await;
    // A third pool holding the name one username row names, so that row really is a name
    // ANOTHER pool holds a user for.
    let elsewhere = given_pool(&router, "session-gate-off-elsewhere", Some("AUDIT")).await;
    given_user(&router, &elsewhere.user_pool_id, FOREIGN_POOL_USERNAME).await;

    for (pool_case, user_pool_id) in [
        ("a pool storing the off mode", off.user_pool_id.as_str()),
        ("a pool storing no add-ons at all", plain.user_pool_id.as_str()),
    ] {
        for (user_case, username) in username_rows() {
            // The user's own submission is unsigned; the administrative history reader
            // carries a credential scope, as an SDK sends it.
            for (operation, signed) in [
                ("UpdateAuthEventFeedback", false),
                ("AdminListUserAuthEvents", true),
            ] {
                let case = format!("{operation} on {pool_case} for {user_case}");
                let mut body = json!({ "UserPoolId": user_pool_id, "Username": &username });
                if operation == "UpdateAuthEventFeedback" {
                    body["EventId"] = json!(ABSENT_EVENT_ID);
                    body["FeedbackToken"] = json!(FOREIGN_FEEDBACK_TOKEN);
                    body["FeedbackValue"] = json!("Valid");
                }
                let answered = dispatch(&router, operation, &body, signed).await;
                assert_eq!(
                    (
                        answered.status,
                        error_code(&answered.payload),
                        error_message(&answered.payload),
                        answered.errortype.as_deref(),
                    ),
                    (
                        REFUSED,
                        "UserPoolAddOnNotEnabledException",
                        USER_POOL_ADD_ONS_NOT_ENABLED_MESSAGE,
                        Some("UserPoolAddOnNotEnabledException:"),
                    ),
                    "case {case:?}: expected the add-on refusal of the pool, got {answered:?}"
                );
            }
        }
    }
}

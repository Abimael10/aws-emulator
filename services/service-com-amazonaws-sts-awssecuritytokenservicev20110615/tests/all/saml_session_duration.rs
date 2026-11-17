// SPDX-License-Identifier: CC0-1.0

//! How long a session `AssumeRoleWithSAML` federates lasts, and when the duration asked for
//! is refused outright.
//!
//! RULE: a federated role session lasts the `DurationSeconds` the request asks for — the
//! operation's documented one hour when it asks for none — shortened to the instant the
//! authentication response's `SessionNotOnOrAfter` bounds the session at when that comes
//! first ("your role session lasts for the duration that you specify, or until the time
//! specified in the SAML authentication response's SessionNotOnOrAfter value, whichever is
//! shorter"); a duration past the role's own `MaxSessionDuration`, or outside the published
//! range of `roleDurationSecondsType`
//! (`smithy_query(query="operation",operation="AssumeRoleWithSAML")` reports 900..43200), is
//! refused `ValidationError` / HTTP 400 and mints nothing.
//!
//! Every row drives the real path end to end on ONE shared engine: the provider and the role
//! are registered through IAM's own operations, the response is signed HERE with the key that
//! provider's metadata publishes, the exchange is made UNSIGNED as a SAML client makes it,
//! and each session it vends is read back through `GetCallerIdentity` under the very
//! credential it handed out. The rows differ along what the rule generalises over: the
//! maximum the role holds (none, two hours, twelve hours), the duration asked for (none, each
//! published bound and one second past each, each side of the role's own maximum), and the
//! instant the response bounds the session at (none, before the duration asked for, after
//! it).

use smithy_server_runtime::MultiServiceRouter;

use super::saml_federation::{
    audience_condition, create_saml_provider, create_saml_role, emulator_with_iam, saml_assertion,
    IdentityProvider, Response,
};
use super::sts_sessions::{
    element, emulator_account, member, now_secs, query, timestamp_secs, LONG_TERM_KEY,
    STS_API_VERSION, STS_SIGNING_NAME,
};
use super::web_identity_federation::unsigned_call;

/// IAM's SigV4 signing name and awsQuery API version: the role's maximum session duration is
/// SET through IAM's own operation, so STS reads the value IAM stored.
const IAM_SIGNING_NAME: &str = "iam";
const IAM_API_VERSION: &str = "2010-05-08";

/// The session every response below names.
const SESSION_NAME: &str = "saml-duration";

/// How AWS words the refusal of a duration past the role's own maximum.
const OVER_ROLE_MAXIMUM_MESSAGE: &str =
    "The requested DurationSeconds exceeds the MaxSessionDuration set for this role.";

/// The published bounds of `roleDurationSecondsType`, this operation's `DurationSeconds`
/// (`smithy_query(query="operation",operation="AssumeRoleWithSAML")`).
const SHORTEST_DURATION_SECONDS: i32 = 900;
const LONGEST_DURATION_SECONDS: i32 = 43_200;

/// This operation's own default: "by default, the temporary security credentials created by
/// AssumeRoleWithSAML last for one hour".
const DEFAULT_DURATION_SECONDS: i64 = 3600;

/// How far from the instant a row expects an expiry may fall and still be that row's answer:
/// generous room for the host's clock and for whatever else it is running, and far smaller
/// than the gap between any two durations the rows ask for.
const CLOCK_SLACK_SECONDS: i64 = 300;

/// What a row expects of the exchange.
enum Expected {
    /// Served, vending a session of about this many seconds.
    Lasts(i64),
    /// Refused `ValidationError` / HTTP 400, minting nothing — and, when the row names a
    /// sentence, stating exactly it.
    Refused(Option<&'static str>),
}

/// Set, through IAM's own operation, the maximum session duration the role holds.
async fn set_max_session_duration(router: &MultiServiceRouter, role_name: &str, seconds: i32) {
    let (status, body) = query(
        router,
        LONG_TERM_KEY,
        IAM_SIGNING_NAME,
        IAM_API_VERSION,
        "UpdateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("MaxSessionDuration", seconds.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "role {role_name:?}: expected IAM to store MaxSessionDuration {seconds}, \
         got {status} {body}"
    );
}

/// One `AssumeRoleWithSAML`, made the way a SAML client makes it — UNSIGNED, holding no
/// Amazon Web Services credential — asking for `duration` when the row names one.
async fn exchange_for(
    router: &MultiServiceRouter,
    role_arn: &str,
    principal_arn: &str,
    assertion: &str,
    duration: Option<i32>,
) -> (u16, String) {
    let mut params = vec![
        ("RoleArn", role_arn.to_string()),
        ("PrincipalArn", principal_arn.to_string()),
        ("SAMLAssertion", assertion.to_string()),
    ];
    if let Some(seconds) = duration {
        params.push(("DurationSeconds", seconds.to_string()));
    }
    unsigned_call(router, "AssumeRoleWithSAML", &params).await
}

#[tokio::test]
async fn a_federated_session_lasts_the_shorter_of_what_was_asked_for_and_what_is_allowed() {
    let (router, _engine) = emulator_with_iam();
    let provider = IdentityProvider::mint("https://idp.example.test/saml");
    let provider_arn = create_saml_provider(&router, "duration-idp", &provider).await;

    // One role per maximum a row needs: none stored at all (so the documented one-hour
    // default applies), two hours, and the twelve hours `roleMaxSessionDurationType` tops out
    // at. The role is what the trust policy admitting this provider is attached to.
    let mut roles: Vec<(Option<i32>, &str, String)> = Vec::new();
    for (maximum, role_name) in [
        (None, "saml-default-maximum"),
        (Some(7200), "saml-two-hour-maximum"),
        (Some(LONGEST_DURATION_SECONDS), "saml-twelve-hour-maximum"),
    ] {
        let role_arn =
            create_saml_role(&router, role_name, &provider_arn, audience_condition()).await;
        if let Some(seconds) = maximum {
            set_max_session_duration(&router, role_name, seconds).await;
        }
        roles.push((maximum, role_name, role_arn));
    }
    let role_of = |maximum: Option<i32>| -> (&str, String) {
        roles
            .iter()
            .find(|(stored, _, _)| *stored == maximum)
            .map(|(_, name, arn)| (*name, arn.clone()))
            .expect("every row names a role the test created")
    };

    // One row: the case, the maximum the role holds, the duration the request asks for, the
    // instant (seconds from now) the response bounds the session at, and what the rule
    // answers.
    type Row = (
        &'static str,
        Option<i32>,
        Option<i32>,
        Option<i64>,
        Expected,
    );
    let rows: Vec<Row> = vec![
        (
            "no duration asked for and nothing bounding the session",
            None,
            None,
            None,
            Expected::Lasts(DEFAULT_DURATION_SECONDS),
        ),
        (
            "the shortest duration the operation publishes",
            Some(LONGEST_DURATION_SECONDS),
            Some(SHORTEST_DURATION_SECONDS),
            None,
            Expected::Lasts(i64::from(SHORTEST_DURATION_SECONDS)),
        ),
        (
            "one second under the shortest duration published",
            Some(LONGEST_DURATION_SECONDS),
            Some(SHORTEST_DURATION_SECONDS - 1),
            None,
            Expected::Refused(None),
        ),
        (
            "the longest duration published, on a role that admits it",
            Some(LONGEST_DURATION_SECONDS),
            Some(LONGEST_DURATION_SECONDS),
            None,
            Expected::Lasts(i64::from(LONGEST_DURATION_SECONDS)),
        ),
        (
            "one second past the longest duration published",
            Some(LONGEST_DURATION_SECONDS),
            Some(LONGEST_DURATION_SECONDS + 1),
            None,
            Expected::Refused(None),
        ),
        (
            "the role's own two-hour maximum, asked for exactly",
            Some(7200),
            Some(7200),
            None,
            Expected::Lasts(7200),
        ),
        (
            "one second past the role's own maximum",
            Some(7200),
            Some(7201),
            None,
            Expected::Refused(Some(OVER_ROLE_MAXIMUM_MESSAGE)),
        ),
        (
            "a role holding no maximum, asked for a second past the default hour",
            None,
            Some(3601),
            None,
            Expected::Refused(Some(OVER_ROLE_MAXIMUM_MESSAGE)),
        ),
        (
            "a response bounding the session before the duration asked for",
            Some(LONGEST_DURATION_SECONDS),
            Some(7200),
            Some(900),
            Expected::Lasts(900),
        ),
        (
            "a duration shorter than the bound the response states",
            Some(LONGEST_DURATION_SECONDS),
            Some(SHORTEST_DURATION_SECONDS),
            Some(7200),
            Expected::Lasts(i64::from(SHORTEST_DURATION_SECONDS)),
        ),
        (
            "a response bounding the session with no duration asked for",
            Some(LONGEST_DURATION_SECONDS),
            None,
            Some(1800),
            Expected::Lasts(1800),
        ),
    ];

    for (case, role_maximum, requested, session_bound, expected) in rows {
        let (role_name, role_arn) = role_of(role_maximum);
        let assertion = saml_assertion(
            &provider,
            &Response {
                name_id: SESSION_NAME.to_string(),
                session_not_on_or_after_offset: session_bound,
                ..Response::valid(&provider, &role_arn, &provider_arn)
            },
        );
        let (status, body) =
            exchange_for(&router, &role_arn, &provider_arn, &assertion, requested).await;

        match expected {
            Expected::Lasts(seconds) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the exchange to be served, got {status} {body}"
                );
                let expires_at = timestamp_secs(case, &member(case, &body, "Expiration"));
                let now = now_secs();
                let drift = expires_at - (now + seconds);
                assert!(
                    drift.abs() <= CLOCK_SLACK_SECONDS,
                    "case {case:?}: expected a session of about {seconds}s, got one expiring \
                     at {expires_at} with the clock at {now} ({drift}s out): {body}"
                );
                // The session is REAL: the credential it vended resolves to the assumed-role
                // identity of the very role the duration was decided against.
                let assumed_role_arn = format!(
                    "arn:aws:sts::{}:assumed-role/{role_name}/{SESSION_NAME}",
                    emulator_account()
                );
                let access_key_id = member(case, &body, "AccessKeyId");
                let (status, read) = query(
                    &router,
                    &access_key_id,
                    STS_SIGNING_NAME,
                    STS_API_VERSION,
                    "GetCallerIdentity",
                    &[],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the vended credential must resolve, got {status} {read}"
                );
                assert_eq!(
                    element(&read, "Arn").as_deref(),
                    Some(assumed_role_arn.as_str()),
                    "case {case:?}: expected the session to report {assumed_role_arn}, got {read}"
                );
            }
            Expected::Refused(sentence) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} {body}"
                );
                assert_eq!(
                    element(&body, "Code").as_deref(),
                    Some("ValidationError"),
                    "case {case:?}: expected <Code>ValidationError</Code>, got {body}"
                );
                assert!(
                    element(&body, "AccessKeyId").is_none(),
                    "case {case:?}: a refused exchange must mint nothing, got {body}"
                );
                if let Some(sentence) = sentence {
                    let reported = member(case, &body, "Message").replace("&amp;", "&");
                    assert_eq!(
                        reported, sentence,
                        "case {case:?}: expected the message {sentence:?}, got {reported:?}"
                    );
                }
            }
        }
    }
}

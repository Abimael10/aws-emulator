// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `ClientRequestToken` a `RotateSecret` call carries when the
//! secret ALREADY holds a version under that id, driven over the wire.
//!
//! The rule under test: a rotation's `ClientRequestToken` is an idempotency token — "This
//! value helps ensure idempotency. Secrets Manager uses this value to prevent the accidental
//! creation of duplicate versions if there are failures and retries during a rotation"
//! (`RotateSecretRequest$ClientRequestToken`, this service's Smithy model) — so a token that
//! already identifies a version of the secret names the version THAT rotation works on
//! rather than making the call invalid: the rotation is started and `AWSPENDING` is staked on
//! the version the id names. The only state that refuses a rotation is the documented one:
//! "If the AWSPENDING staging label is present but not attached to the same version as
//! AWSCURRENT, then any later invocation of RotateSecret assumes that a previous rotation
//! request is still in progress and returns an error" (`RotateSecret`, same model).
//!
//! That is what lets a caller reattempt the rotation an unfinished one left behind: a
//! rotation whose function raised leaves its candidate version staked `AWSPENDING`, the
//! caller cleans the label off with `UpdateSecretVersionStage`, and the rotation is driven
//! again under the token it was started with — the candidate the earlier attempt created is
//! picked up instead of a duplicate being minted for it.
//!
//! Every test is a TABLE over what the rule generalises over: WHICH stored version the token
//! names (the current one, the one `AWSCURRENT` was moved off, one no label reaches any
//! more, and the candidate an unfinished rotation left behind) and the token's length at both
//! published bounds of `ClientRequestTokenType` (`@length` 32..64) and inside them.
//!
//! No Lambda is mounted beside this service here, so nothing answers for the rotation
//! function and no step of the rotation runs: what each row asserts is the state the CALL
//! leaves, which is the state a rotation in flight is seen by. The rotation function's own
//! work is driven end to end in `secret_rotation_driver`.

use serde_json::{json, Map, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::state::ROTATION_IN_PROGRESS_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

use crate::secret_client_request_token::{
    call, emulator, fresh_token, ok, refusal, staging_map, text, token_of_length, TOKEN_MAX_LEN,
    TOKEN_MIN_LEN,
};

/// The ARN of a Lambda rotation function, as `RotationLambdaARN` carries it.
const LAMBDA_ARN: &str = "arn:aws:lambda:us-west-2:123456789012:function:RotateMySecret";

/// The staging labels this suite reads: the version a secret currently serves, the candidate
/// a rotation stakes, and the one `AWSCURRENT` was moved off.
const CURRENT_STAGE: &str = "AWSCURRENT";
const PENDING_STAGE: &str = "AWSPENDING";
const PREVIOUS_STAGE: &str = "AWSPREVIOUS";

/// Token lengths spanning `ClientRequestTokenType`: both published bounds and one inside
/// them.
const TOKEN_LENGTHS: &[usize] = &[TOKEN_MIN_LEN, 36, TOKEN_MAX_LEN];

/// Which stored version a row's rotation token names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Names {
    /// The version `AWSCURRENT` is attached to.
    TheCurrentVersion,
    /// The version `AWSCURRENT` was moved off, which holds `AWSPREVIOUS`.
    ThePreviousVersion,
    /// A version no staging label reaches any more, which is "considered to be 'deprecated'"
    /// (`UpdateSecretVersionStage`, this service's Smithy model) and still addressable by its
    /// id.
    ADeprecatedVersion,
    /// The candidate an unfinished rotation left behind, after the caller took `AWSPENDING`
    /// off it.
    AnUnstakedCandidate,
}

impl Names {
    /// Every row of the table, in the order the tests drive them.
    const ALL: [Names; 4] = [
        Names::TheCurrentVersion,
        Names::ThePreviousVersion,
        Names::ADeprecatedVersion,
        Names::AnUnstakedCandidate,
    ];

    /// A name for this row's secret, unique per row.
    fn segment(self) -> &'static str {
        match self {
            Names::TheCurrentVersion => "current",
            Names::ThePreviousVersion => "previous",
            Names::ADeprecatedVersion => "deprecated",
            Names::AnUnstakedCandidate => "candidate",
        }
    }
}

/// The history a row rotates on top of: the version its token names, the value that version
/// holds, and the version `AWSCURRENT` is on.
struct History {
    /// The version the rotation under test names with its `ClientRequestToken`.
    target: String,
    /// The value that version was stored with.
    target_value: String,
    /// The version holding `AWSCURRENT` when the rotation under test is made.
    current: String,
}

/// The rotation configuration every call in this suite carries.
fn rotation_request(secret_id: &str, token: &str) -> Value {
    json!({
        "SecretId": secret_id,
        "ClientRequestToken": token,
        "RotationLambdaARN": LAMBDA_ARN,
        "RotationRules": {"AutomaticallyAfterDays": 30},
    })
}

/// Build the secret `name` so that it holds a version of the shape `names` describes, each
/// version staked under a token of `length` characters.
async fn history(
    router: &MultiServiceRouter,
    name: &str,
    names: Names,
    length: usize,
    case: &str,
) -> History {
    let first = token_of_length(length);
    ok(
        router,
        "CreateSecret",
        &json!({
            "Name": name,
            "SecretString": "credential-0",
            "ClientRequestToken": first.clone(),
        }),
        case,
    )
    .await;
    match names {
        Names::TheCurrentVersion => History {
            target: first.clone(),
            target_value: "credential-0".to_string(),
            current: first,
        },
        Names::ThePreviousVersion | Names::ADeprecatedVersion => {
            let second = token_of_length(length);
            ok(
                router,
                "PutSecretValue",
                &json!({
                    "SecretId": name,
                    "SecretString": "credential-1",
                    "ClientRequestToken": second.clone(),
                }),
                case,
            )
            .await;
            if names == Names::ADeprecatedVersion {
                // "If this action results in the last label being removed from a version,
                // then the version is considered to be 'deprecated'"
                // (`UpdateSecretVersionStage`, this service's Smithy model).
                ok(
                    router,
                    "UpdateSecretVersionStage",
                    &json!({
                        "SecretId": name,
                        "VersionStage": PREVIOUS_STAGE,
                        "RemoveFromVersionId": first.clone(),
                    }),
                    case,
                )
                .await;
            }
            History {
                target: first,
                target_value: "credential-0".to_string(),
                current: second,
            }
        }
        Names::AnUnstakedCandidate => {
            // A rotation is started, its function stages the candidate it was told to create
            // under the rotation's own token — the `createSecret` step's write — and the
            // rotation then goes no further, because nothing here answers for that function.
            let candidate = token_of_length(length);
            ok(
                router,
                "RotateSecret",
                &rotation_request(name, &candidate),
                case,
            )
            .await;
            ok(
                router,
                "PutSecretValue",
                &json!({
                    "SecretId": name,
                    "SecretString": "candidate-value",
                    "ClientRequestToken": candidate.clone(),
                    "VersionStages": [PENDING_STAGE],
                }),
                case,
            )
            .await;
            // "You might need to remove the staging label AWSPENDING from the partially
            // created version" (`CancelRotateSecret`, this service's Smithy model): the
            // clean-up that leaves the candidate stored and unstaked.
            ok(
                router,
                "UpdateSecretVersionStage",
                &json!({
                    "SecretId": name,
                    "VersionStage": PENDING_STAGE,
                    "RemoveFromVersionId": candidate.clone(),
                }),
                case,
            )
            .await;
            History {
                target: candidate,
                target_value: "candidate-value".to_string(),
                current: first,
            }
        }
    }
}

/// The versions a staging map reports `stage` on.
fn staged(map: &Map<String, Value>, stage: &str) -> Vec<String> {
    map.iter()
        .filter(|(_, labels)| {
            labels
                .as_array()
                .is_some_and(|labels| labels.iter().any(|label| label == stage))
        })
        .map(|(version, _)| version.clone())
        .collect()
}

#[tokio::test]
async fn a_rotation_token_naming_a_stored_version_stakes_that_version() {
    // RULE: the token is the id of the version the rotation works on, so a token that
    // already identifies a version of the secret is served and `AWSPENDING` is staked on
    // THAT version — the rotation picks the stored candidate up instead of the call being
    // refused for the token. The rows run over which version the token names and over the
    // token's length.
    for names in Names::ALL {
        for length in TOKEN_LENGTHS {
            let case = format!(
                "a rotation whose token names {names:?} under a {length}-character token"
            );
            let router = emulator();
            let name = format!("reattempt/{}-{length}", names.segment());
            let history = history(&router, &name, names, *length, &case).await;

            let started = ok(
                &router,
                "RotateSecret",
                &rotation_request(&name, &history.target),
                &case,
            )
            .await;
            assert_eq!(
                text(&started, "VersionId", &case),
                history.target,
                "case {case:?}: expected the rotation to report the version its token names, \
                 got {started}"
            );

            // `AWSPENDING` is on the version the token names, and on no other: "each staging
            // label can be attached to only one version at a time".
            let map = staging_map(&router, &name, &case).await;
            assert_eq!(
                staged(&map, PENDING_STAGE),
                vec![history.target.clone()],
                "case {case:?}: expected {PENDING_STAGE} on {}, got {map:?}",
                history.target
            );
            // The call staked a LABEL: it replaced no value and moved the current one
            // nowhere.
            assert_eq!(
                staged(&map, CURRENT_STAGE),
                vec![history.current.clone()],
                "case {case:?}: expected {CURRENT_STAGE} left on {}, got {map:?}",
                history.current
            );
            let read = ok(
                &router,
                "GetSecretValue",
                &json!({"SecretId": name.clone(), "VersionId": history.target.clone()}),
                &case,
            )
            .await;
            assert_eq!(
                text(&read, "SecretString", &case),
                history.target_value,
                "case {case:?}: expected the staked version to keep its value, got {read}"
            );

            // The configuration the call carried is in force, whichever version its token
            // named.
            let described = ok(
                &router,
                "DescribeSecret",
                &json!({"SecretId": name.clone()}),
                &case,
            )
            .await;
            assert_eq!(
                described["RotationEnabled"].as_bool(),
                Some(true),
                "case {case:?}: expected rotation enabled, got {described}"
            );
            assert_eq!(
                text(&described, "RotationLambdaARN", &case),
                LAMBDA_ARN,
                "case {case:?}: expected the rotation function stored, got {described}"
            );
        }
    }
}

#[tokio::test]
async fn a_further_rotation_is_refused_only_while_the_candidate_is_not_the_current_version() {
    // The boundary of the rule: what refuses a rotation is the documented STATE, not the
    // token — "if the AWSPENDING staging label is present but not attached to the same
    // version as AWSCURRENT, then any later invocation of RotateSecret assumes that a
    // previous rotation request is still in progress and returns an error", and when it IS
    // attached to that same version the rotation may start. The rows run over which version
    // the first rotation's token named; the further rotation names a token of its own in
    // every row.
    for names in Names::ALL {
        let case = format!("a further rotation after one whose token named {names:?}");
        let router = emulator();
        let name = format!("further/{}", names.segment());
        let history = history(&router, &name, names, TOKEN_MIN_LEN + 4, &case).await;
        ok(
            &router,
            "RotateSecret",
            &rotation_request(&name, &history.target),
            &case,
        )
        .await;

        let fresh = fresh_token();
        let (status, parsed) = call(&router, "RotateSecret", &rotation_request(&name, &fresh)).await;
        // `AWSPENDING` on the `AWSCURRENT` version is the shape of a rotation that is NOT in
        // progress, so only a candidate sitting elsewhere refuses the next rotation.
        if history.target == history.current {
            assert_eq!(
                status, 200,
                "case {case:?}: expected a further rotation to be served, got {status} with \
                 {parsed}"
            );
            assert_eq!(
                text(&parsed, "VersionId", &case),
                fresh,
                "case {case:?}: expected the further rotation's own token back, got {parsed}"
            );
            let map = staging_map(&router, &name, &case).await;
            assert_eq!(
                staged(&map, PENDING_STAGE),
                vec![fresh.clone()],
                "case {case:?}: expected the candidate to move to {fresh}, got {map:?}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {parsed}"
            );
            let (code, message) = refusal(&parsed);
            assert_eq!(
                code, "InvalidRequestException",
                "case {case:?}: expected InvalidRequestException, got {code:?} with {message:?}"
            );
            assert_eq!(
                message, ROTATION_IN_PROGRESS_MESSAGE,
                "case {case:?}: expected {ROTATION_IN_PROGRESS_MESSAGE:?}, got {message:?}"
            );
            // The refused call staked nothing: the candidate is still the one the rotation
            // in flight named, and the token it carried names no version.
            let map = staging_map(&router, &name, &case).await;
            assert_eq!(
                staged(&map, PENDING_STAGE),
                vec![history.target.clone()],
                "case {case:?}: expected the candidate untouched, got {map:?}"
            );
            assert!(
                !map.contains_key(&fresh),
                "case {case:?}: expected the refused call to mint nothing, got {map:?}"
            );
        }
    }
}

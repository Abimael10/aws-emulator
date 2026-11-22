// SPDX-License-Identifier: CC0-1.0

//! A rotation that ended unfinished is RE-DRIVEN by the call that names its own candidate: the
//! `ClientRequestToken` is what tells a retry of that rotation from a request for a further one.
//!
//! The rule under test: **`RotateSecret` refuses a call that would start a rotation BESIDE the
//! outstanding one — "if the AWSPENDING staging label is present but not attached to the same
//! version as AWSCURRENT, then any later invocation of RotateSecret assumes that a previous
//! rotation request is still in progress and returns an error" (`RotateSecret`, Secrets
//! Manager's Smithy model) — but a rotation whose function RAN a step and did not complete it is
//! not in progress any more: it ended there, with `AWSCURRENT` on the version it was on and the
//! candidate still `AWSPENDING`, and a call whose `ClientRequestToken` names that candidate IS
//! that rotation, driven again: "you only need to specify this value if you implement your own
//! retry logic and you want to ensure that Secrets Manager doesn't attempt to create a secret
//! version twice. [...] Secrets Manager uses this value to prevent the accidental creation of
//! duplicate versions if there are failures and retries during a rotation"
//! (`RotateSecretRequest$ClientRequestToken`, `@idempotencyToken`). Such a call is served,
//! answers that candidate's own id, mints no second version for it, and drives the rotation
//! function again — so a client whose function failed part-way finishes the rotation by repairing
//! the function and re-driving the same token.**
//!
//! That is the recovery path a rotation that stopped mid-protocol leaves a client: without it a
//! secret whose rotation function raised is wedged for ever, since every call is answered with a
//! refusal promising a reattempt that no API call can ask for. The safety half of the same rule
//! is what the sibling suite
//! [`crate::secretsmanager_rotation_stops_at_the_step_its_function_raises_in`] pins — an
//! untested candidate is never promoted — and this suite starts where that one ends.
//!
//! The rows span what the rule generalises over: WHICH step the earlier rotation stopped at, so
//! the candidate the re-drive names is in turn one no version exists for yet (the rotation's
//! function could not even create it), one staged but untested, and one that passed its test and
//! was never promoted; and whether the re-driving call names the secret by name or by ARN. Every
//! row also pins the boundary: a call carrying a token of its OWN is still refused, with the
//! fixed sentence, and mints nothing. The other half of that boundary — a rotation still IN
//! FLIGHT, whose candidate not even its own token may re-drive — is pinned where a rotation that
//! reaches no function is driven, in the Secrets Manager crate's own
//! `secret_rotation::a_started_rotation_stages_awspending_and_leaves_awscurrent_alone`.
//!
//! Four crates have to agree — CloudFormation deploys the rotation functions and the role they
//! run as, IAM answers for that role and for the grant Secrets Manager invokes them under,
//! Lambda runs each invocation, Secrets Manager drives the protocol — so the suite lives here
//! and reuses the harness
//! [`crate::secretsmanager_rotation_function_is_tested_and_validated`] owns. Every wait looks for
//! the STATE it waits on, and its ceiling is there only so the suite cannot hang.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::secretsmanager_rotation_function_is_tested_and_validated::{
    build_router, deployed_function, described, recorded_steps, secretsmanager,
    secretsmanager_err as refused_in, secretsmanager_ok as served_in, stages_of, token,
    wait_for_steps, CURRENT_STAGE, POLL, POLLS, REGION, ROTATION_STEPS,
};

/// The staging label an outstanding candidate carries, and the one the version a promotion
/// replaced carries afterwards.
const PENDING_STAGE: &str = "AWSPENDING";
const PREVIOUS_STAGE: &str = "AWSPREVIOUS";

/// The value every row's secret is created with, and what its rotation function appends to it
/// when it stages the candidate: the re-driven rotation must leave the CANDIDATE's value
/// serving.
const INITIAL_VALUE: &str = "the-credential-before-any-rotation";
const ROTATED_SUFFIX: &str = "-rotated";

/// The sentence a call asking for a rotation beside the outstanding one is refused with. Fixed
/// text: it names no version, because what it reports is that a rotation of this secret has not
/// finished.
const ROTATION_IN_PROGRESS: &str =
    "A previous rotation isn't complete. That rotation will be reattempted.";

/// The rotation function every row is driven through, written the way the AWS rotation templates
/// are: it RECORDS the step it was invoked with before anything else — which is how a row waits
/// for a rotation that cannot finish — and then does that step's own work through Secrets
/// Manager's own operations, leaving a candidate an earlier attempt already staged alone. It
/// raises on the step `FAIL_AT` names, which is the only thing the deployments differ in.
const ROTATION_FUNCTION_BODY: &str = r#"
import boto3
from botocore.exceptions import ClientError

STEPS_TAG = "RotationSteps"
SUFFIX = "-rotated"


def record(client, arn, step):
    metadata = client.describe_secret(SecretId=arn)
    recorded = ""
    for tag in metadata.get("Tags", []):
        if tag["Key"] == STEPS_TAG:
            recorded = tag["Value"]
    recorded = recorded + "," + step if recorded else step
    client.tag_resource(SecretId=arn, Tags=[{"Key": STEPS_TAG, "Value": recorded}])
    return metadata


def staged(client, arn, token):
    try:
        return client.get_secret_value(SecretId=arn, VersionId=token)
    except ClientError:
        return None


def lambda_handler(event, context):
    arn = event["SecretId"]
    token = event["ClientRequestToken"]
    step = event["Step"]
    client = boto3.client("secretsmanager")
    metadata = record(client, arn, step)
    if step == FAIL_AT:
        raise RuntimeError("this rotation function cannot complete the %s step" % step)
    versions = metadata.get("VersionIdsToStages", {})
    if step == "createSecret":
        if staged(client, arn, token) is None:
            current = client.get_secret_value(SecretId=arn, VersionStage="AWSCURRENT")
            client.put_secret_value(
                SecretId=arn,
                ClientRequestToken=token,
                SecretString=current["SecretString"] + SUFFIX,
                VersionStages=["AWSPENDING"],
            )
    elif step == "setSecret":
        pass
    elif step == "testSecret":
        if staged(client, arn, token) is None:
            raise ValueError("the candidate %s holds no value to test" % token)
    elif step == "finishSecret":
        previous = None
        for version, stages in versions.items():
            if "AWSCURRENT" in stages:
                previous = version
        client.update_secret_version_stage(
            SecretId=arn,
            VersionStage="AWSCURRENT",
            MoveToVersionId=token,
            RemoveFromVersionId=previous,
        )
    else:
        raise ValueError("Invalid step parameter %s" % step)
    return {"step": step}
"#;

/// That function with the step it cannot complete fixed, which is the whole of a deployment's
/// identity here; `""` is the function that completes every step, since no step is named so.
fn rotation_function(fail_at: &str) -> String {
    format!("FAIL_AT = \"{fail_at}\"\n{ROTATION_FUNCTION_BODY}")
}

/// One row: the step the earlier rotation's function could not complete, and how the call that
/// re-drives that rotation names the secret.
struct Row {
    case: &'static str,
    /// The step the first rotation stops at, which decides what state its candidate is left in.
    fail_at: &'static str,
    /// The names this row's own stack, secret and tokens are built from.
    id: &'static str,
    /// Whether the re-driving call addresses the secret by its ARN rather than by its name.
    by_arn: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a candidate no version exists for, left by a function that raised in createSecret",
        fail_at: "createSecret",
        id: "uncreated",
        by_arn: false,
    },
    Row {
        case: "an untested candidate, left by a function that raised in testSecret",
        fail_at: "testSecret",
        id: "untested",
        by_arn: true,
    },
    Row {
        case: "a tested candidate never promoted, left by a function that raised in finishSecret",
        fail_at: "finishSecret",
        id: "unpromoted",
        by_arn: false,
    },
];

impl Row {
    /// The steps this row's first rotation gets THROUGH, the raising one included: the protocol
    /// stops at the first step that did not complete.
    fn stopped_after(&self) -> Vec<String> {
        let reached = ROTATION_STEPS
            .iter()
            .position(|step| *step == self.fail_at)
            .map_or(ROTATION_STEPS.len(), |at| at + 1);
        ROTATION_STEPS[..reached]
            .iter()
            .map(|step| (*step).to_string())
            .collect()
    }

    /// The steps recorded once the re-driven rotation has run as well: the ones its predecessor
    /// got through, then the whole protocol again.
    fn after_the_re_drive(&self) -> Vec<String> {
        let mut steps = self.stopped_after();
        steps.extend(ROTATION_STEPS.iter().map(|step| (*step).to_string()));
        steps
    }
}

/// Call one Secrets Manager operation in this suite's Region, requiring the 200 it answers.
async fn sm_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    served_in(router, case, REGION, operation, request).await
}

/// The same, requiring a refusal: its status, wire error code and message.
async fn sm_err(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> (u16, String, String) {
    refused_in(router, case, REGION, operation, request).await
}

/// Call one Secrets Manager operation in this suite's Region with whatever status it answered,
/// which is what a row that waits for a call to START being served reads.
async fn served(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    secretsmanager(router, REGION, operation, request).await
}

/// The versions a staging map reports `stage` on.
fn holders(stages: &BTreeMap<String, Vec<String>>, stage: &str) -> Vec<String> {
    stages
        .iter()
        .filter(|(_, labels)| labels.iter().any(|label| label == stage))
        .map(|(version, _)| version.clone())
        .collect()
}

/// Every version id `ListSecretVersionIds` reports for the secret, the ones no staging label
/// reaches any more included ("Specifies whether to include versions of secrets that don't have
/// any staging labels attached to them", `ListSecretVersionIdsRequest$IncludeDeprecated`).
async fn stored_versions(router: &MultiServiceRouter, case: &str, secret_id: &str) -> Vec<String> {
    let listed = sm_ok(
        router,
        case,
        "ListSecretVersionIds",
        &json!({ "SecretId": secret_id, "IncludeDeprecated": true }),
    )
    .await;
    let mut ids: Vec<String> = listed["Versions"]
        .as_array()
        .map(|versions| {
            versions
                .iter()
                .filter_map(|version| version["VersionId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

/// Poll `DescribeSecret` until the rotation of `secret_id` has FINISHED — stamped with the
/// instant it finished ("the last date and time that Secrets Manager rotated the secret",
/// `DescribeSecretResponse$LastRotatedDate`) — and answer the last answer read.
async fn wait_for_the_rotation_to_finish(
    router: &MultiServiceRouter,
    case: &str,
    secret_id: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..POLLS {
        last = described(router, case, secret_id).await;
        if last.get("LastRotatedDate").is_some() {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    last
}

/// RULE: the `ClientRequestToken` of a rotation that ENDED unfinished re-drives that rotation,
/// while a token of the caller's own asks for a further one and is refused — so a rotation its
/// function could not complete is finished by naming its candidate again, and no second
/// candidate is minted for it.
#[tokio::test]
async fn a_rotation_left_unfinished_is_re_driven_by_the_call_that_names_its_candidate() {
    let router = build_router();
    let completing =
        deployed_function(&router, "redrivecompleting", &rotation_function("")).await;

    for row in ROWS {
        let case = row.case;
        let name = format!("re-driven-credentials-{}", row.id);
        let created = sm_ok(
            &router,
            case,
            "CreateSecret",
            &json!({
                "Name": name.clone(),
                "ClientRequestToken": token(row.id),
                "SecretString": INITIAL_VALUE,
            }),
        )
        .await;
        let original = created["VersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {created}"))
            .to_string();
        let arn = created["ARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ARN in {created}"))
            .to_string();

        // A rotation that stops at the step its function could not complete, which is the state
        // the rule is about: the candidate outstanding, `AWSCURRENT` where it was.
        let stopping = deployed_function(
            &router,
            &format!("redrive{}", row.id),
            &rotation_function(row.fail_at),
        )
        .await;
        let candidate = token(&format!("cand{}", row.id));
        let started = sm_ok(
            &router,
            case,
            "RotateSecret",
            &json!({
                "SecretId": name.clone(),
                "ClientRequestToken": candidate.clone(),
                "RotationLambdaARN": stopping,
                "RotationRules": { "AutomaticallyAfterDays": 30 },
                "RotateImmediately": true,
            }),
        )
        .await;
        assert_eq!(
            started["VersionId"].as_str(),
            Some(candidate.as_str()),
            "case {case:?}: expected the rotation to stake the token the call named, got {started}"
        );

        let stopped_after = row.stopped_after();
        let wedged = wait_for_steps(&router, case, &name, stopped_after.len()).await;
        assert_eq!(
            recorded_steps(&wedged),
            stopped_after,
            "case {case:?}: expected the rotation to stop after {stopped_after:?}, got {wedged}"
        );
        let wedged_stages = stages_of(&wedged);
        assert_eq!(
            wedged_stages,
            BTreeMap::from([
                (original.clone(), vec![CURRENT_STAGE.to_string()]),
                (candidate.clone(), vec![PENDING_STAGE.to_string()]),
            ]),
            "case {case:?}: expected the candidate outstanding beside an untouched \
             {CURRENT_STAGE}, got {wedged}"
        );
        // The rotation did not COMPLETE, and nothing of it reached the value a client reads:
        // "the last date and time that Secrets Manager rotated the secret"
        // (`DescribeSecretResponse$LastRotatedDate`) is still unset, and the secret still
        // serves the version it served before — the candidate was never made current.
        assert!(
            wedged.get("LastRotatedDate").is_none(),
            "case {case:?}: expected no LastRotatedDate for a rotation that did not finish, got \
             {wedged}"
        );
        let before = sm_ok(
            &router,
            case,
            "GetSecretValue",
            &json!({ "SecretId": name.clone(), "VersionStage": CURRENT_STAGE }),
        )
        .await;
        assert_eq!(
            (
                before["VersionId"].as_str(),
                before["SecretString"].as_str(),
            ),
            (Some(original.as_str()), Some(INITIAL_VALUE)),
            "case {case:?}: expected the value the secret served before the rotation, got {before}"
        );

        // The boundary: a call carrying a token of its own asks for a rotation BESIDE that one,
        // and is refused with the fixed sentence without minting anything.
        let intruder = token(&format!("fresh{}", row.id));
        let (status, code, message) = sm_err(
            &router,
            case,
            "RotateSecret",
            &json!({
                "SecretId": name.clone(),
                "ClientRequestToken": intruder.clone(),
                "RotationLambdaARN": completing.clone(),
                "RotateImmediately": true,
            }),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidRequestException"),
            "case {case:?}: expected 400 InvalidRequestException for a further rotation, got \
             {status} {code:?} with {message:?}"
        );
        assert_eq!(
            message, ROTATION_IN_PROGRESS,
            "case {case:?}: expected {ROTATION_IN_PROGRESS:?}, got {message:?}"
        );
        assert_eq!(
            stages_of(&described(&router, case, &name).await),
            wedged_stages,
            "case {case:?}: expected the refused call to stake nothing, got a changed staging map"
        );

        // ... while the call naming the candidate of the rotation that ENDED is that rotation
        // driven again: served, answering that candidate's id, through the repaired function.
        //
        // The call is made until it is SERVED rather than once: the rotation ends when the
        // invocation its function raised in returns to the service, which is a moment after the
        // step record this row waited for, so a call that arrives in between is still refused
        // for a rotation in flight. The deadline is generous and only there so the row cannot
        // hang; a refused call stakes nothing, so repeating it changes nothing.
        let secret_id = if row.by_arn { arn.clone() } else { name.clone() };
        let request = json!({
            "SecretId": secret_id,
            "ClientRequestToken": candidate.clone(),
            "RotationLambdaARN": completing.clone(),
            "RotateImmediately": true,
        });
        let mut re_driven = Value::Null;
        for look in 0..POLLS {
            let (status, answered) = served(&router, "RotateSecret", &request).await;
            if status == 200 {
                re_driven = answered;
                break;
            }
            assert!(
                look + 1 < POLLS,
                "case {case:?}: the rotation's own token never re-drove it, last answer {status} \
                 with {answered}"
            );
            tokio::time::sleep(POLL).await;
        }
        assert_eq!(
            re_driven["VersionId"].as_str(),
            Some(candidate.as_str()),
            "case {case:?}: expected the re-drive to answer the candidate it names, got \
             {re_driven}"
        );

        // It runs the protocol again and this time reaches `finishSecret`, so the candidate is
        // promoted and the rotation is stamped as finished.
        let finished = wait_for_the_rotation_to_finish(&router, case, &name).await;
        assert!(
            finished.get("LastRotatedDate").is_some(),
            "case {case:?}: expected the re-driven rotation to finish, got {finished}"
        );
        let after_the_re_drive = row.after_the_re_drive();
        assert_eq!(
            recorded_steps(&finished),
            after_the_re_drive,
            "case {case:?}: expected the steps {after_the_re_drive:?}, got {finished}"
        );
        // The candidate is promoted and the version it replaced carries {PREVIOUS_STAGE}. The
        // candidate may keep {PENDING_STAGE} as well, because that is the shape of a rotation
        // that SUCCEEDED: "when rotation is successful, the AWSPENDING staging label might be
        // attached to the same version as the AWSCURRENT version, or it might not be attached
        // to any version" (`RotateSecret`, this service's Smithy model) — what it may not do is
        // sit on another version, which is the shape of a rotation still outstanding.
        let promoted = stages_of(&finished);
        assert_eq!(
            holders(&promoted, CURRENT_STAGE),
            vec![candidate.clone()],
            "case {case:?}: expected {CURRENT_STAGE} on the promoted candidate alone, got \
             {finished}"
        );
        assert_eq!(
            holders(&promoted, PREVIOUS_STAGE),
            vec![original.clone()],
            "case {case:?}: expected {PREVIOUS_STAGE} on the version the rotation replaced, got \
             {finished}"
        );
        assert!(
            holders(&promoted, PENDING_STAGE)
                .iter()
                .all(|version| *version == candidate),
            "case {case:?}: expected no candidate outstanding once the rotation finished, got \
             {finished}"
        );

        // What a client reads is that candidate's own value, and the secret holds no version
        // minted beside it: the token is what kept the rotation to one candidate.
        let read = sm_ok(
            &router,
            case,
            "GetSecretValue",
            &json!({ "SecretId": name.clone(), "VersionStage": CURRENT_STAGE }),
        )
        .await;
        let expected_value = format!("{INITIAL_VALUE}{ROTATED_SUFFIX}");
        assert_eq!(
            (
                read["VersionId"].as_str(),
                read["SecretString"].as_str(),
            ),
            (Some(candidate.as_str()), Some(expected_value.as_str())),
            "case {case:?}: expected the candidate's own value served, got {read}"
        );
        let mut expected_versions = vec![original.clone(), candidate.clone()];
        expected_versions.sort();
        assert_eq!(
            stored_versions(&router, case, &name).await,
            expected_versions,
            "case {case:?}: expected the two versions of the one rotation, and nothing minted \
             for {intruder:?}"
        );
    }
}

// SPDX-License-Identifier: CC0-1.0

//! `CancelRotateSecret` is the way OUT of a rotation that never finished: the next
//! `RotateSecret` resumes the candidate that rotation left instead of being refused for it.
//!
//! The rule under test: **while a rotation of a secret is still being reattempted — a candidate
//! holding `AWSPENDING` beside an untouched `AWSCURRENT`, with rotation still on — a further
//! `RotateSecret` is refused 400 `InvalidRequestException` with the fixed sentence "A previous
//! rotation isn't complete. That rotation will be reattempted."; but once
//! `CancelRotateSecret` has cancelled that rotation ("turns off automatic rotation, and if a
//! rotation is currently in progress, cancels the rotation", `CancelRotateSecret`, Secrets
//! Manager's Smithy model), while keeping the staging labels it left, the candidate of THAT
//! rotation is one nothing will reattempt, and the next `RotateSecret` TAKES IT UP: the call is
//! served, it reports the candidate's own version id rather than the `ClientRequestToken` it
//! carried, it mints no version for that token, and the rotation it drives is the leftover one —
//! so the value the secret ends up serving is the one that candidate already held.**
//!
//! That is the documented recovery path for a rotation whose function failed mid-protocol:
//! cancel, repair the function, rotate again. Without it a secret in that state is wedged, and
//! a client following the path gets the in-progress refusal for ever. Every OTHER outstanding
//! candidate keeps that refusal, because for it the model's sentence is still true — which is
//! why the rule is keyed on the cancel and not on rotation merely being off.
//!
//! The state the rule is about is reached two ways here. The first table stages the candidate
//! through the operation a rotation function's own `createSecret` step stages one with
//! (`PutSecretValue` with `VersionStages` naming `AWSPENDING`), so the rows need no function
//! that fails and nothing waits for a rotation to give up. The second test reaches it through a
//! rotation that really RUNS and raises in the middle of the protocol, which is the half the
//! rows cannot reach: the rotation cancelled there is one this service is actively driving, so
//! the cancel has to stop it — a cancelled rotation that went on to its last step would finish
//! the candidate itself, and the call that resumes it would have nothing left to take up.
//!
//! The rows differ along what the rule generalises over: whether the resuming call carries a
//! `ClientRequestToken` of its own at all, and whether it names the secret by name or by ARN.
//! Five crates have to agree for the resumed rotation to run — CloudFormation deploys the
//! rotation function, IAM answers for its execution role and for the grant Secrets Manager
//! invokes it under, Lambda runs it, Secrets Manager drives it — so the suite lives here, and
//! it drives that estate through the harness its sibling
//! `secretsmanager_rotation_function_is_tested_and_validated` owns. Every wait looks for the
//! STATE it waits on, and its ceiling is there only so the suite cannot hang.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::secretsmanager_rotation_function_is_tested_and_validated::{
    build_router, deployed_function, described, recorded_steps, secretsmanager_err as refused_in,
    secretsmanager_ok as served_in, stages_of, token, wait_for_steps, CURRENT_STAGE, POLL, POLLS,
    REGION, ROTATION_STEPS, TEST_STEP,
};

/// The staging labels a candidate and the version it replaces carry.
const PENDING_STAGE: &str = "AWSPENDING";
const PREVIOUS_STAGE: &str = "AWSPREVIOUS";

/// The value each row's secret is created with, and the one its stuck candidate holds: the
/// rotation that resumes must leave the CANDIDATE's value serving, which is what says the
/// resumed rotation was the leftover one.
const INITIAL_VALUE: &str = "credential-in-use";
const CANDIDATE_VALUE: &str = "credential-the-stuck-rotation-staged";

/// The sentence a further `RotateSecret` is refused with while the rotation is still being
/// reattempted. Fixed text: it names no version.
const ROTATION_IN_PROGRESS: &str = "A previous rotation isn't complete. That rotation will be \
                                    reattempted.";

/// The rotation function every row rotates through: the four documented steps, written the way
/// the AWS rotation templates are — it records the step it was invoked with, leaves a candidate
/// another attempt already staged alone, proves the staged value is readable on `testSecret` and
/// moves `AWSCURRENT` onto it on `finishSecret`, all through Secrets Manager's own operations.
///
/// Its `setSecret` step — the one that "changes the credential in the service that owns it" —
/// RAISES while the secret does not carry [`SET_SECRET_READY_TAG`], which is how a row below
/// makes a rotation fail in the middle of the protocol and then repairs the function's own work
/// without redeploying it.
const ROTATION_FUNCTION: &str = r#"
import boto3
from botocore.exceptions import ClientError

STEPS_TAG = "RotationSteps"
READY_TAG = "SetSecretReady"
SUFFIX = "-rotated"


def staged_value(client, arn, token):
    try:
        return client.get_secret_value(SecretId=arn, VersionId=token)
    except ClientError:
        return None


def lambda_handler(event, context):
    arn = event["SecretId"]
    token = event["ClientRequestToken"]
    step = event["Step"]
    client = boto3.client("secretsmanager")
    metadata = client.describe_secret(SecretId=arn)
    tags = {}
    for tag in metadata.get("Tags", []):
        tags[tag["Key"]] = tag["Value"]
    recorded = tags.get(STEPS_TAG, "")
    recorded = recorded + "," + step if recorded else step
    client.tag_resource(SecretId=arn, Tags=[{"Key": STEPS_TAG, "Value": recorded}])
    versions = metadata.get("VersionIdsToStages", {})
    staged = versions.get(token, [])
    if "AWSCURRENT" in staged:
        return {"step": step, "already_current": True}
    if step == "createSecret":
        if staged_value(client, arn, token) is None:
            current = client.get_secret_value(SecretId=arn, VersionStage="AWSCURRENT")
            client.put_secret_value(
                SecretId=arn,
                ClientRequestToken=token,
                SecretString=current["SecretString"] + SUFFIX,
                VersionStages=["AWSPENDING"],
            )
    elif step == "setSecret":
        if READY_TAG not in tags:
            raise RuntimeError("this rotation function cannot set the credential yet")
    elif step == "testSecret":
        client.get_secret_value(SecretId=arn, VersionId=token)
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

/// The tag whose presence lets that function's `setSecret` step complete: the stand-in for the
/// service whose credential that step changes being reachable. A row writes it to repair a
/// rotation function that could not complete the protocol.
const SET_SECRET_READY_TAG: &str = "SetSecretReady";

/// What that function appends to the value it stages on `createSecret`.
const ROTATED_SUFFIX: &str = "-rotated";

/// Call one Secrets Manager operation in this suite's Region, requiring the 200 it answers.
async fn secretsmanager_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> Value {
    served_in(router, case, REGION, operation, request).await
}

/// The same, requiring a refusal, as its status, wire error code and message.
async fn secretsmanager_err(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
) -> (u16, String, String) {
    refused_in(router, case, REGION, operation, request).await
}

/// The versions `DescribeSecret` reports the staging label `stage` on.
fn staged_versions(described: &Value, stage: &str) -> Vec<String> {
    stages_of(described)
        .into_iter()
        .filter(|(_, labels)| labels.iter().any(|label| label == stage))
        .map(|(version, _)| version)
        .collect()
}

/// Poll `DescribeSecret` until the rotation has finished — `LastRotatedDate` stamped — and
/// answer the last answer read.
async fn wait_until_rotated(router: &MultiServiceRouter, case: &str, secret_id: &str) -> Value {
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

/// Let the rotation function's `setSecret` step complete for this secret, by writing the tag
/// that step reads ([`SET_SECRET_READY_TAG`]).
async fn allow_set_secret(router: &MultiServiceRouter, case: &str, secret_id: &str) {
    secretsmanager_ok(
        router,
        case,
        "TagResource",
        &json!({
            "SecretId": secret_id,
            "Tags": [{ "Key": SET_SECRET_READY_TAG, "Value": "yes" }],
        }),
    )
    .await;
}

/// One row: whether the resuming call carries a `ClientRequestToken` of its own, and how it
/// names the secret.
struct Row {
    case: &'static str,
    names_token: bool,
    by_arn: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the resuming call carries a fresh token and names the secret by name",
        names_token: true,
        by_arn: false,
    },
    Row {
        case: "the resuming call carries no token and names the secret by ARN",
        names_token: false,
        by_arn: true,
    },
];

/// RULE: the in-progress refusal stands while the rotation is still being reattempted, and
/// stands down for the candidate `CancelRotateSecret` cancelled the rotation of — after which
/// the next `RotateSecret` resumes that candidate, reporting its id and minting nothing for the
/// token the call carried.
#[tokio::test]
async fn a_rotation_after_a_cancel_resumes_the_candidate_the_cancelled_one_left() {
    let router = build_router();
    let function = deployed_function(&router, "resumedrotationstack", ROTATION_FUNCTION).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("resumed-credentials-{index}");
        let created = secretsmanager_ok(
            &router,
            case,
            "CreateSecret",
            &json!({
                "Name": name,
                "ClientRequestToken": token(&format!("current{index}")),
                "SecretString": INITIAL_VALUE,
            }),
        )
        .await;
        // The rotation function's `setSecret` step can complete for this secret from the
        // start: what these rows are about is the candidate, not a function that fails.
        allow_set_secret(&router, case, &name).await;
        let arn = created["ARN"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ARN in {created}"))
            .to_string();
        let current = created["VersionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {created}"))
            .to_string();
        let secret_id = if row.by_arn { arn.clone() } else { name.clone() };

        // Rotation ON, with no rotation started: the configuration this secret's rotations are
        // driven through. The call tests that configuration by invoking the function once, so
        // the row waits for that record before staging anything of its own — every step
        // recorded after it belongs to the rotation the rows are about.
        secretsmanager_ok(
            &router,
            case,
            "RotateSecret",
            &json!({
                "SecretId": secret_id,
                "RotationLambdaARN": function,
                "RotationRules": { "AutomaticallyAfterDays": 30 },
                "RotateImmediately": false,
            }),
        )
        .await;
        let tested = wait_for_steps(&router, case, &name, 1).await;
        assert_eq!(
            recorded_steps(&tested),
            vec![TEST_STEP.to_string()],
            "case {case:?}: expected the deferred call to test the configuration with \
             {TEST_STEP:?}, got {tested}"
        );

        // The state the rule is about: a candidate staged `AWSPENDING` beside the untouched
        // current version, staged the way a rotation function's own `createSecret` step stages
        // one, while rotation is still on.
        let stuck = token(&format!("stuck{index}"));
        secretsmanager_ok(
            &router,
            case,
            "PutSecretValue",
            &json!({
                "SecretId": secret_id,
                "ClientRequestToken": stuck,
                "SecretString": CANDIDATE_VALUE,
                "VersionStages": [PENDING_STAGE],
            }),
        )
        .await;
        let wedged = described(&router, case, &name).await;
        assert_eq!(
            stages_of(&wedged),
            BTreeMap::from([
                (current.clone(), vec![CURRENT_STAGE.to_string()]),
                (stuck.clone(), vec![PENDING_STAGE.to_string()]),
            ]),
            "case {case:?}: expected the candidate staged beside the current version, got \
             {wedged}"
        );
        assert_eq!(
            wedged["RotationEnabled"].as_bool(),
            Some(true),
            "case {case:?}: expected rotation still on, got {wedged}"
        );

        // While that rotation is still the service's to reattempt, a further call is refused —
        // and mints nothing for the token it carried.
        let refused_token = token(&format!("refused{index}"));
        let (status, code, message) = secretsmanager_err(
            &router,
            case,
            "RotateSecret",
            &json!({ "SecretId": secret_id, "ClientRequestToken": refused_token }),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidRequestException"),
            "case {case:?}: expected 400 InvalidRequestException for the further rotation, got \
             {status} {code:?} with {message:?}"
        );
        assert_eq!(
            message, ROTATION_IN_PROGRESS,
            "case {case:?}: expected {ROTATION_IN_PROGRESS:?}, got {message:?}"
        );
        let (status, code, message) = secretsmanager_err(
            &router,
            case,
            "GetSecretValue",
            &json!({ "SecretId": secret_id, "VersionId": refused_token }),
        )
        .await;
        let expected = format!(
            "Secrets Manager can't find the specified secret value for VersionId: \
             {refused_token}"
        );
        assert_eq!(
            (status, code.as_str()),
            (400, "ResourceNotFoundException"),
            "case {case:?}: expected the refused call to mint no version, got {status} \
             {code:?} with {message:?}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // The cancel reports the candidate it cancelled the rotation of, turns rotation off and
        // keeps that candidate staged.
        let cancelled = secretsmanager_ok(
            &router,
            case,
            "CancelRotateSecret",
            &json!({ "SecretId": secret_id }),
        )
        .await;
        assert_eq!(
            cancelled["VersionId"].as_str(),
            Some(stuck.as_str()),
            "case {case:?}: expected the cancel to report the candidate {stuck:?}, got \
             {cancelled}"
        );
        let after_cancel = described(&router, case, &name).await;
        assert_eq!(
            after_cancel["RotationEnabled"].as_bool(),
            Some(false),
            "case {case:?}: expected rotation off after the cancel, got {after_cancel}"
        );
        assert_eq!(
            staged_versions(&after_cancel, PENDING_STAGE),
            vec![stuck.clone()],
            "case {case:?}: expected the cancel to keep the candidate staged, got {after_cancel}"
        );

        // The next rotation RESUMES that candidate: it is served, it reports the candidate's own
        // id, and it mints no version for the token it carried.
        let fresh = token(&format!("fresh{index}"));
        let mut request = json!({ "SecretId": secret_id });
        if row.names_token {
            request["ClientRequestToken"] = json!(fresh);
        }
        let resumed = secretsmanager_ok(&router, case, "RotateSecret", &request).await;
        assert_eq!(
            resumed["VersionId"].as_str(),
            Some(stuck.as_str()),
            "case {case:?}: expected the resumed rotation to report the candidate {stuck:?} \
             rather than the token the call carried, got {resumed}"
        );
        if row.names_token {
            let (status, code, message) = secretsmanager_err(
                &router,
                case,
                "GetSecretValue",
                &json!({ "SecretId": secret_id, "VersionId": fresh }),
            )
            .await;
            let expected = format!(
                "Secrets Manager can't find the specified secret value for VersionId: {fresh}"
            );
            assert_eq!(
                (status, code.as_str()),
                (400, "ResourceNotFoundException"),
                "case {case:?}: expected no version minted for the resuming call's token, got \
                 {status} {code:?} with {message:?}"
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }

        // ... and the rotation it drove is the LEFTOVER one: the four steps run for the
        // candidate, `AWSCURRENT` ends on it, the version it replaced carries `AWSPREVIOUS`, and
        // the value served is the one that candidate already held.
        let rotated = wait_until_rotated(&router, case, &name).await;
        let steps = recorded_steps(&rotated);
        let tail: Vec<String> = steps
            .iter()
            .rev()
            .take(ROTATION_STEPS.len())
            .rev()
            .cloned()
            .collect();
        assert_eq!(
            tail, ROTATION_STEPS,
            "case {case:?}: expected the resumed rotation to run the four documented steps in \
             order, got {steps:?}"
        );
        assert_eq!(
            staged_versions(&rotated, CURRENT_STAGE),
            vec![stuck.clone()],
            "case {case:?}: expected AWSCURRENT on the resumed candidate {stuck:?}, got {rotated}"
        );
        assert_eq!(
            staged_versions(&rotated, PREVIOUS_STAGE),
            vec![current.clone()],
            "case {case:?}: expected AWSPREVIOUS on the version the rotation replaced, got \
             {rotated}"
        );
        let read = secretsmanager_ok(
            &router,
            case,
            "GetSecretValue",
            &json!({ "SecretId": secret_id, "VersionStage": CURRENT_STAGE }),
        )
        .await;
        assert_eq!(
            read["SecretString"].as_str(),
            Some(CANDIDATE_VALUE),
            "case {case:?}: expected the value the resumed candidate already held, got {read}"
        );
        assert_eq!(
            read["VersionId"].as_str(),
            Some(stuck.as_str()),
            "case {case:?}: expected the served version to be the resumed candidate, got {read}"
        );
    }
}

/// RULE, with the candidate left by a rotation that really ran: a rotation whose function
/// raises in the middle of the protocol leaves its candidate staged, the cancel stops THAT
/// rotation (so it does not go on to finish once its function works again), and the next
/// `RotateSecret` resumes the same candidate and drives it to the end.
///
/// This is the recovery path as a client walks it — rotate, cancel, repair the function, rotate
/// again — and it is the half the rows above cannot reach: the rotation that is cancelled here
/// is one this service is actively driving.
#[tokio::test]
async fn a_rotation_cancelled_mid_protocol_is_resumed_once_its_function_is_repaired() {
    let case = "a rotation whose function raises in setSecret, cancelled and then resumed";
    let router = build_router();
    let function =
        deployed_function(&router, "cancelledmidrotationstack", ROTATION_FUNCTION).await;
    let name = "mid-protocol-credentials";
    let created = secretsmanager_ok(
        &router,
        case,
        "CreateSecret",
        &json!({
            "Name": name,
            "ClientRequestToken": token("midcurrent"),
            "SecretString": INITIAL_VALUE,
        }),
    )
    .await;
    let current = created["VersionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no VersionId in {created}"))
        .to_string();

    // A rotation that runs: its function stages the candidate on `createSecret` and then raises
    // on `setSecret`, because the secret does not carry the tag that step reads — so the
    // rotation stops in the middle of the protocol, with the candidate staged.
    let stuck = token("midstuck");
    let started = secretsmanager_ok(
        &router,
        case,
        "RotateSecret",
        &json!({
            "SecretId": name,
            "ClientRequestToken": stuck,
            "RotationLambdaARN": function,
            "RotationRules": { "AutomaticallyAfterDays": 30 },
        }),
    )
    .await;
    assert_eq!(
        started["VersionId"].as_str(),
        Some(stuck.as_str()),
        "case {case:?}: expected the rotation to stake the token the call named, got {started}"
    );

    // The rotation reached its second step and could not get past it, which is what leaves the
    // candidate outstanding: the row waits for the record of that step rather than for a span.
    let wedged = wait_for_steps(&router, case, name, 2).await;
    let steps = recorded_steps(&wedged);
    assert_eq!(
        steps.first().map(String::as_str),
        Some(ROTATION_STEPS[0]),
        "case {case:?}: expected the rotation to be driven from its first step, got {steps:?}"
    );
    assert!(
        steps.iter().any(|step| step == ROTATION_STEPS[1]),
        "case {case:?}: expected the function to have been invoked for {:?}, got {steps:?}",
        ROTATION_STEPS[1]
    );
    assert!(
        !steps.iter().any(|step| step == ROTATION_STEPS[3]),
        "case {case:?}: expected the rotation not to have reached {:?}, got {steps:?}",
        ROTATION_STEPS[3]
    );
    assert_eq!(
        staged_versions(&wedged, PENDING_STAGE),
        vec![stuck.clone()],
        "case {case:?}: expected the candidate {stuck:?} staged, got {wedged}"
    );
    assert_eq!(
        staged_versions(&wedged, CURRENT_STAGE),
        vec![current.clone()],
        "case {case:?}: expected the current version untouched, got {wedged}"
    );

    // While that rotation is still the service's to reattempt, a further one is refused.
    let (status, code, message) = secretsmanager_err(
        &router,
        case,
        "RotateSecret",
        &json!({ "SecretId": name, "ClientRequestToken": token("midrefused") }),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidRequestException"),
        "case {case:?}: expected 400 InvalidRequestException while the rotation is in progress, \
         got {status} {code:?} with {message:?}"
    );
    assert_eq!(
        message, ROTATION_IN_PROGRESS,
        "case {case:?}: expected {ROTATION_IN_PROGRESS:?}, got {message:?}"
    );

    // Cancel, then repair what the function's `setSecret` step could not do. The cancelled
    // rotation is not driven a step further, so it is the call below that finishes the
    // candidate — which is what makes that call's answer the candidate's own id rather than a
    // fresh one.
    let cancelled = secretsmanager_ok(
        &router,
        case,
        "CancelRotateSecret",
        &json!({ "SecretId": name }),
    )
    .await;
    assert_eq!(
        cancelled["VersionId"].as_str(),
        Some(stuck.as_str()),
        "case {case:?}: expected the cancel to report the candidate {stuck:?}, got {cancelled}"
    );
    allow_set_secret(&router, case, name).await;

    let resumed = secretsmanager_ok(
        &router,
        case,
        "RotateSecret",
        &json!({ "SecretId": name, "ClientRequestToken": token("midfresh") }),
    )
    .await;
    assert_eq!(
        resumed["VersionId"].as_str(),
        Some(stuck.as_str()),
        "case {case:?}: expected the resumed rotation to report the candidate the cancelled one \
         left, got {resumed}"
    );

    // The resumed rotation ran to its end, on that candidate: the value the first attempt's
    // `createSecret` wrote is the one the secret now serves.
    let rotated = wait_until_rotated(&router, case, name).await;
    let steps = recorded_steps(&rotated);
    assert!(
        steps.iter().any(|step| step == ROTATION_STEPS[3]),
        "case {case:?}: expected the resumed rotation to reach {:?}, got {steps:?}",
        ROTATION_STEPS[3]
    );
    assert_eq!(
        staged_versions(&rotated, CURRENT_STAGE),
        vec![stuck.clone()],
        "case {case:?}: expected AWSCURRENT on the resumed candidate {stuck:?}, got {rotated}"
    );
    assert_eq!(
        staged_versions(&rotated, PREVIOUS_STAGE),
        vec![current.clone()],
        "case {case:?}: expected AWSPREVIOUS on the version the rotation replaced, got {rotated}"
    );
    let read = secretsmanager_ok(
        &router,
        case,
        "GetSecretValue",
        &json!({ "SecretId": name, "VersionStage": CURRENT_STAGE }),
    )
    .await;
    let expected = format!("{INITIAL_VALUE}{ROTATED_SUFFIX}");
    assert_eq!(
        read["SecretString"].as_str(),
        Some(expected.as_str()),
        "case {case:?}: expected the value the first attempt staged, got {read}"
    );
    assert_eq!(
        read["VersionId"].as_str(),
        Some(stuck.as_str()),
        "case {case:?}: expected the served version to be the resumed candidate, got {read}"
    );
}

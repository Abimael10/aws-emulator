// SPDX-License-Identifier: CC0-1.0

//! A rotation refused for want of the rotation function's invoke grant is refused on the
//! policy that function holds AT THE MOMENT OF THE CALL — so writing the grant and repeating
//! the SAME call rotates. Secrets Manager and Lambda over ONE engine, the shape
//! `core/router/src/lib.rs` mounts them in.
//!
//! THE RULE: "You also need `lambda:InvokeFunction` permissions on the rotation function"
//! (`RotateSecret`, this service's Smithy model), and Secrets Manager invokes it as its own
//! service principal — so every `RotateSecret` that would drive a rotation function reads that
//! FUNCTION's resource-based policy as it stands when the call is made, and
//!
//! * a call no such policy admits is refused SYNCHRONOUSLY with the 400
//!   `AccessDeniedException` and leaves the secret exactly as it was: rotation not enabled, no
//!   version staged `AWSPENDING`, no version beyond the ones already stored, `AWSCURRENT` on
//!   the version and the value it already held — and the rotation function not invoked at all;
//! * the same call made once the policy admits it starts the rotation, which runs through to
//!   its end: `AWSCURRENT` moves onto the version the call staked, the version it came from
//!   carries `AWSPREVIOUS` ("Secrets Manager moves the staging label `AWSPREVIOUS` to the
//!   version that `AWSCURRENT` was removed from", `UpdateSecretVersionStage`), the served value
//!   is the one the function wrote, and `LastRotatedDate` is stamped.
//!
//! The refusal is therefore a verdict on the policy, not a mark on the secret: nothing about
//! being refused once stops the next call from rotating.
//!
//! The rows differ along what the rule generalises over — WHY the first call found no grant (a
//! function holding no policy at all, one whose grant names another service principal, one
//! whose grant is scoped to a different secret) and WHICH shape of grant is written before the
//! second (unscoped, scoped to the secret being rotated, scoped to that secret's own account),
//! which is also the three shapes `AddPermission` writes. Each row is a secret of its own, and
//! all of them rotate through ONE function created once, because what a row varies is that
//! function's POLICY, which is no part of the execution environment's identity.
//!
//! Each row carries its own POSITIVE CONTROL: the second half rotates through the SAME secret
//! and the SAME function the first half was refused on, so the first half's "nothing moved and
//! the function never ran" is the refusal rather than a harness that could not have rotated
//! anything. Every wait looks for the STATE the rotation reaches rather than for a span to
//! pass, and the ceiling on the looking is only there so the suite cannot hang.
//!
//! What is asserted elsewhere and not repeated here:
//! `crate::secret_rotation_driver` pins which shapes of document admit or withhold the
//! invocation, and what a rotation that runs writes; this suite pins only that the decision is
//! taken afresh per call and that a refused call is inert.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::secret_client_request_token::fresh_token;
use crate::secret_rotation_driver::{
    apply_grant, create_function, described, rotated, sm_err, sm_ok, staged_versions, tag_value,
    value_at, Grant, CURRENT_STAGE, PENDING_STAGE, SERVICE_PRINCIPAL,
};

/// The staging label the version `AWSCURRENT` was taken from carries once a rotation has moved
/// it: "Secrets Manager moves the staging label AWSPREVIOUS to the version that AWSCURRENT was
/// removed from" (`UpdateSecretVersionStage`, this service's Smithy model).
const PREVIOUS_STAGE: &str = "AWSPREVIOUS";

/// What this service answers a rotation whose function does not admit its invocation: AWS's
/// own sentence for that condition (Secrets Manager User Guide, *Troubleshoot rotation*),
/// whose only variable part is the service principal it names.
const INVOKE_REFUSAL: &str = "Secrets Manager cannot invoke the specified Lambda function. \
                              Ensure that the function policy grants access to the principal \
                              secretsmanager.amazonaws.com.";

/// The value every row's secret holds before its rotation, so what the rotation changed is
/// asserted on the material rather than on a version id alone.
const INITIAL_VALUE: &str = "the-credential-before-the-rotation";

/// What the rotation function below appends to the value it rotates.
const ROTATED_SUFFIX: &str = "-rotated";

/// The tag key the rotation function records every invocation under, which is what says the
/// function RAN: a refused `RotateSecret` never reaches it, so the tag is absent.
const INVOKED_STEP_TAG: &str = "invoked-step";

/// The rotation function every row rotates through: the four steps AWS invokes a rotation
/// function with, each served through this service's own operations, and — before anything
/// else — a tag naming the step it was invoked for, so that the function having run at all is
/// something a client can read.
const ROTATION_FUNCTION: &str = r#"
import boto3

SUFFIX = "-rotated"


def lambda_handler(event, context):
    arn = event["SecretId"]
    token = event["ClientRequestToken"]
    step = event["Step"]
    client = boto3.client("secretsmanager")
    client.tag_resource(
        SecretId=arn, Tags=[{"Key": "invoked-step", "Value": step}])
    metadata = client.describe_secret(SecretId=arn)
    versions = metadata["VersionIdsToStages"]
    if "AWSCURRENT" in versions.get(token, []):
        return {"step": step}
    if "AWSPENDING" not in versions.get(token, []):
        raise ValueError(
            "Secret version %s not set as AWSPENDING for rotation of secret %s." % (token, arn))
    if step == "createSecret":
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

/// One row: the grant in force when the first call is made, and the one written before the
/// second call repeats it.
struct Row {
    case: &'static str,
    /// What the function's policy holds for the call that must be refused.
    refusing: Grant,
    /// What is written onto it before the same call is made again, which must admit the
    /// rotation.
    permitting: Grant,
}

/// The rows, spanning why a call finds no grant and which shape of grant admits the next one.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a function holding no policy at all, then an unscoped grant",
            refusing: Grant::None,
            permitting: Grant::Unscoped,
        },
        Row {
            case: "a grant to another service principal, then one scoped to this secret",
            refusing: Grant::AnotherPrincipal,
            permitting: Grant::ThisSecret,
        },
        Row {
            case: "a grant scoped to another secret, then one scoped to this account",
            refusing: Grant::AnotherSecret,
            permitting: Grant::ThisAccount,
        },
    ]
}

/// The secret `name`, created holding [`INITIAL_VALUE`]; answers its ARN and the id of the
/// version `AWSCURRENT` starts on.
async fn create_secret(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let token = fresh_token();
    let created = sm_ok(
        router,
        case,
        "CreateSecret",
        &json!({
            "Name": name,
            "ClientRequestToken": &token,
            "SecretString": INITIAL_VALUE,
        }),
    )
    .await;
    let arn = created["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected an ARN from CreateSecret, got {created}"))
        .to_string();
    (arn, token)
}

/// Assert that `RotateSecret` with `request` is refused for want of the function's grant, and
/// that nothing about the secret moved as a consequence.
async fn refused_and_inert(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    request: &Value,
    initial_version: &str,
) {
    let (status, code, message) = sm_err(router, case, "RotateSecret", request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {code} and {message:?}"
    );
    assert_eq!(
        code, "AccessDeniedException",
        "case {case:?}: expected AccessDeniedException, got {code} with {message:?}"
    );
    assert_eq!(
        message, INVOKE_REFUSAL,
        "case {case:?}: expected the refusal {INVOKE_REFUSAL:?}, got {message:?}"
    );

    let held = described(router, case, name).await;
    assert_ne!(
        held["RotationEnabled"].as_bool(),
        Some(true),
        "case {case:?}: a refused rotation enabled rotation, got {held}"
    );
    assert!(
        held.get("LastRotatedDate").is_none(),
        "case {case:?}: a refused rotation stamped LastRotatedDate, got {held}"
    );
    assert_eq!(
        staged_versions(&held, PENDING_STAGE),
        Vec::<String>::new(),
        "case {case:?}: a refused rotation staked {PENDING_STAGE}, got {held}"
    );
    assert_eq!(
        staged_versions(&held, CURRENT_STAGE),
        vec![initial_version.to_string()],
        "case {case:?}: a refused rotation moved {CURRENT_STAGE}, got {held}"
    );
    let versions = held["VersionIdsToStages"]
        .as_object()
        .map_or(0, serde_json::Map::len);
    assert_eq!(
        versions, 1,
        "case {case:?}: a refused rotation created a version, got {held}"
    );
    assert_eq!(
        value_at(router, case, name, CURRENT_STAGE).await,
        Some(INITIAL_VALUE.to_string()),
        "case {case:?}: a refused rotation changed the current value"
    );
    // The function was never invoked: it tags the secret with its step before doing anything,
    // so a secret carrying no such tag is one no invocation reached.
    assert_eq!(
        tag_value(&held, INVOKED_STEP_TAG),
        None,
        "case {case:?}: a refused rotation invoked the rotation function, got {held}"
    );
}

/// Assert that `RotateSecret` with `request` now starts a rotation that runs to its end, and
/// that the staging labels and the served value are what a finished rotation leaves.
async fn rotates(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    request: &Value,
    initial_version: &str,
    token: &str,
) {
    let started = sm_ok(router, case, "RotateSecret", request).await;
    assert_eq!(
        started["VersionId"].as_str(),
        Some(token),
        "case {case:?}: expected the staked version to be the ClientRequestToken {token:?}, got \
         {started}"
    );

    let finished = rotated(router, case, name).await;
    assert!(
        finished.get("LastRotatedDate").is_some(),
        "case {case:?}: expected the rotation to finish and stamp LastRotatedDate, got {finished}"
    );
    assert_eq!(
        finished["RotationEnabled"].as_bool(),
        Some(true),
        "case {case:?}: expected rotation to be enabled, got {finished}"
    );
    assert_eq!(
        staged_versions(&finished, CURRENT_STAGE),
        vec![token.to_string()],
        "case {case:?}: expected {CURRENT_STAGE} on the rotated version {token:?}, got {finished}"
    );
    assert_eq!(
        staged_versions(&finished, PREVIOUS_STAGE),
        vec![initial_version.to_string()],
        "case {case:?}: expected {PREVIOUS_STAGE} on the version {CURRENT_STAGE} came from \
         ({initial_version:?}), got {finished}"
    );
    assert!(
        staged_versions(&finished, PENDING_STAGE)
            .iter()
            .all(|version| version == token),
        "case {case:?}: expected no other version left holding {PENDING_STAGE}, got {finished}"
    );
    // The value served is the one the rotation FUNCTION wrote, and the one it replaced is still
    // readable under the previous label.
    assert_eq!(
        value_at(router, case, name, CURRENT_STAGE).await,
        Some(format!("{INITIAL_VALUE}{ROTATED_SUFFIX}")),
        "case {case:?}: expected the rotated value, got {finished}"
    );
    assert_eq!(
        value_at(router, case, name, PREVIOUS_STAGE).await,
        Some(INITIAL_VALUE.to_string()),
        "case {case:?}: expected the pre-rotation value under {PREVIOUS_STAGE}, got {finished}"
    );
    assert!(
        tag_value(&finished, INVOKED_STEP_TAG).is_some(),
        "case {case:?}: expected the rotation function to have recorded an invocation, got \
         {finished}"
    );
}

/// RULE: the invoke grant is read per call, so a `RotateSecret` refused for want of it leaves
/// the secret untouched and the SAME call rotates once the grant is there.
#[tokio::test]
async fn the_grant_is_read_per_call_so_a_refused_rotation_is_inert_and_repeatable() {
    let (router, function, function_arn) = router_with_function().await;
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("regranted-secret-{index}");
        let (secret_arn, initial_version) = create_secret(&router, case, &name).await;

        // The same call throughout: only the FUNCTION's policy differs between the two halves,
        // so what the second half shows is the decision being taken afresh.
        let token = fresh_token();
        let request = json!({
            "SecretId": &name,
            "RotationLambdaARN": &function_arn,
            "RotationRules": {"AutomaticallyAfterDays": 1},
            "RotateImmediately": true,
            "ClientRequestToken": &token,
        });

        apply_grant(&router, case, function, row.refusing, &secret_arn).await;
        refused_and_inert(&router, case, &name, &request, &initial_version).await;

        apply_grant(&router, case, function, row.permitting, &secret_arn).await;
        rotates(&router, case, &name, &request, &initial_version, &token).await;
    }
}

/// The router every row runs on, and the one rotation function they share: created once,
/// because what a row varies is that function's resource-based policy.
async fn router_with_function() -> (MultiServiceRouter, &'static str, String) {
    let router = crate::secret_rotation_driver::router();
    let function = "regrant-rotator";
    let function_arn = create_function(
        &router,
        "the rotation function the regrant rows share",
        function,
        ROTATION_FUNCTION,
    )
    .await;
    assert!(
        function_arn.contains(function),
        "case \"the shared rotation function\": expected the ARN to name {function:?}, got \
         {function_arn:?}"
    );
    // The principal the grants below are written for is the one this service invokes as.
    assert_eq!(
        SERVICE_PRINCIPAL, "secretsmanager.amazonaws.com",
        "case \"the invoking principal\": expected Secrets Manager's own service principal, got \
         {SERVICE_PRINCIPAL:?}"
    );
    (router, function, function_arn)
}

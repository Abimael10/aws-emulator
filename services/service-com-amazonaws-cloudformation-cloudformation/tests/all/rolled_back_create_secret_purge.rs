// SPDX-License-Identifier: CC0-1.0

//! What the ROLLBACK of a failed stack CREATE leaves of the secrets that create had already
//! made, end to end over both services' own wires.
//!
//! RULE: wherever CloudFormation gives up a secret IT created — the rollback of a create that
//! failed, as much as the stack's own deletion — the secret is removed from AWS Secrets
//! Manager with NO recovery window under the resource's own `DeletionPolicy`, so once the
//! operation reports its terminal status that service holds no such secret: every read of it
//! is refused as a secret that does not exist, and the name is free for the next create. Only
//! `Retain` keeps the resource; `Delete` (the documented default a declaration stating no
//! policy gets), `Snapshot` on a type whose service takes no snapshot of one, and
//! `RetainExceptOnCreate` — "retain the resource only if the resource creation fails", whose
//! one disposing event is the rollback of the operation that created it — all remove it.
//!
//! A secret left merely SCHEDULED for deletion fails that rule in three ways a client sees:
//! it still answers `DescribeSecret` with its whole record, it still holds its name against a
//! redeployment of the same template, and the credential material the rollback should have
//! destroyed stays readable for the whole recovery window. `DeleteSecret` models exactly that
//! switch — `ForceDeleteWithoutRecovery` beside `RecoveryWindowInDays` — and what a stack's
//! disposal of a secret asks for is the first.
//!
//! The rows of the first case differ along what the rule generalises over: every documented
//! `DeletionPolicy` value plus the declaration that states none, which is what decides
//! whether the rollback disposes of the secret or leaves it. The second case is the recorded
//! shape end to end: one template deployed, deleted, and deployed again, so the create fails
//! on the `Retain` secret the first deployment left behind and the rollback must leave
//! nothing of the `Delete` one.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    call, create_stack, deploy, describe_stack, member, resource_event_reason,
    resource_event_statuses, router, settled_stack, xml_text,
};
use crate::stack_owned_secret_teardown::{error_code, secretsmanager};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    deletion_policy, resource_status, status,
};

/// The error code Secrets Manager refuses a read of a secret it does not hold with
/// (`ResourceNotFoundException`, that service's own modelled refusal of `DescribeSecret` and
/// `GetSecretValue`, HTTP 400).
const SECRET_NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentence that refusal carries: fixed text, naming no part of the request — the
/// requested `SecretId` is not echoed into it.
const SECRET_NOT_FOUND_MESSAGE: &str = "Secrets Manager can't find the specified secret.";

/// The stack the first case deploys.
const STACK: &str = "rollback-secret-purge";

/// The name the failing declaration asks for, held by a secret of the account's own — so no
/// record of CloudFormation's knows it, the create reaches Secrets Manager, and that service
/// is what refuses it.
const COLLISION_NAME: &str = "rollback-secret-purge-taken";

/// The value that pre-existing secret holds, which the failed create must leave untouched.
const COLLISION_VALUE: &str = "{\"username\":\"probeadmin\",\"password\":\"already-there\"}";

/// One declared secret of the failing stack: the `DeletionPolicy` it states and what the
/// rollback must leave of it.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The logical id the template declares it under. Ordered so the declaration that FAILS
    /// sorts last whichever order the template reader yields, beside the `DependsOn` that
    /// states the order outright.
    logical_id: &'static str,
    /// The name the declaration states, which is the handle the secret is read back by.
    secret_name: &'static str,
    /// The `DeletionPolicy` the declaration states, or `""` for the declaration that states
    /// none and so gets the documented default.
    policy: &'static str,
    /// Whether the rollback must LEAVE the secret in Secrets Manager.
    kept: bool,
}

/// Every row's declaration: the four documented `DeletionPolicy` values
/// ([`deletion_policy`], whose doc sentences are the source of what each must do) and the
/// declaration that states none.
const ROWS: &[Row] = &[
    Row {
        // The recorded shape: the policy stated outright.
        case: "DeletionPolicy=Delete is removed by the rollback",
        logical_id: "ADelete",
        secret_name: "rollback-secret-purge-delete",
        policy: deletion_policy::DELETE,
        kept: false,
    },
    Row {
        // No attribute at all: "Delete" is the documented default.
        case: "a declaration stating no policy takes the documented default",
        logical_id: "BDefault",
        secret_name: "rollback-secret-purge-default",
        policy: "",
        kept: false,
    },
    Row {
        // The value whose answer depends on WHICH disposal this is: a create's rollback is
        // the one event it does not keep the resource at.
        case: "DeletionPolicy=RetainExceptOnCreate is removed by a CREATE's rollback",
        logical_id: "CExceptOnCreate",
        secret_name: "rollback-secret-purge-except-on-create",
        policy: deletion_policy::RETAIN_EXCEPT_ON_CREATE,
        kept: false,
    },
    Row {
        // Secrets Manager publishes no call that snapshots a secret, so the policy has no
        // snapshot to take and the secret is disposed of as the default disposes of one.
        case: "DeletionPolicy=Snapshot on a type with no snapshot call still removes it",
        logical_id: "DSnapshot",
        secret_name: "rollback-secret-purge-snapshot",
        policy: deletion_policy::SNAPSHOT,
        kept: false,
    },
    Row {
        // The one value that KEEPS the resource: nothing at all is asked of the owning
        // service for it.
        case: "DeletionPolicy=Retain survives the rollback untouched",
        logical_id: "ERetain",
        secret_name: "rollback-secret-purge-retain",
        policy: deletion_policy::RETAIN,
        kept: true,
    },
];

/// The value a row's declaration states, distinct per row so a read that served the wrong
/// secret is visible.
fn declared_value(row: &Row) -> String {
    format!("{{\"username\":\"probeadmin\",\"password\":\"{}\"}}", row.secret_name)
}

/// One `AWS::SecretsManager::Secret` declaration: the row's name, value and policy.
fn declaration(row: &Row) -> Value {
    let mut declared = json!({
        "Type": "AWS::SecretsManager::Secret",
        "Properties": {
            "Name": row.secret_name,
            "Description": "a secret the failed create had already made",
            "SecretString": declared_value(row),
            "Tags": [{"Key": "probe", "Value": "rollback-purge"}]
        }
    });
    if !row.policy.is_empty() {
        declared
            .as_object_mut()
            .expect("a declaration is a JSON object")
            .insert("DeletionPolicy".to_string(), json!(row.policy));
    }
    declared
}

/// The template of the first case: every row, and last the declaration whose name Secrets
/// Manager already holds — which is what fails the create. It `DependsOn` every row, so the
/// rows are created before the failure and the rollback has something to dispose of.
fn template() -> String {
    let mut resources = serde_json::Map::new();
    for row in ROWS {
        resources.insert(row.logical_id.to_string(), declaration(row));
    }
    resources.insert(
        "ZCollision".to_string(),
        json!({
            "Type": "AWS::SecretsManager::Secret",
            "DeletionPolicy": deletion_policy::DELETE,
            "DependsOn": ROWS.iter().map(|row| row.logical_id).collect::<Vec<_>>(),
            "Properties": {
                "Name": COLLISION_NAME,
                "SecretString": "a value no create of this name ever stores"
            }
        }),
    );
    json!({"Resources": resources}).to_string()
}

/// Read the stack's event stream until it carries `wanted` for `logical_id`, and answer every
/// status it carries for that resource.
///
/// Waits for the CONDITION — an event is recorded at an instant the operation reaches, so what
/// a caller waits for is the event appearing — under the shared, generous event deadline.
async fn event_statuses_including(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    wanted: &str,
) -> Vec<String> {
    let deadline = std::time::Instant::now() + crate::harness::EVENT_DEADLINE;
    loop {
        let statuses = resource_event_statuses(router, case, stack, logical_id).await;
        if statuses.iter().any(|status| status == wanted) {
            return statuses;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the stack's events for {logical_id} to carry {wanted}, \
             got {statuses:?}"
        );
        tokio::time::sleep(crate::harness::EVENT_POLL_INTERVAL).await;
    }
}

/// `CreateSecret` through Secrets Manager's own wire, as a client that is NOT an AWS SDK
/// makes it: "if you generate a raw HTTP request to the Secrets Manager service endpoint,
/// then you must generate a ClientRequestToken and include it in the request"
/// (`CreateSecretRequest$ClientRequestToken`), which the SDKs fill in with a random UUID. The
/// token is derived from `nth` so every call of one case carries its own.
async fn create_secret(
    router: &MultiServiceRouter,
    name: &str,
    value: &str,
    nth: usize,
) -> (u16, Value) {
    secretsmanager(
        router,
        "CreateSecret",
        &json!({
            "Name": name,
            "SecretString": value,
            "ClientRequestToken": format!("00000000-0000-4000-8000-{nth:012}")
        }),
    )
    .await
}

/// The `Message` an awsJson error body carries, whichever casing it spells the member with.
fn message(parsed: &Value) -> Option<&str> {
    parsed
        .get("Message")
        .or_else(|| parsed.get("message"))
        .and_then(Value::as_str)
}

/// Assert that Secrets Manager holds NOTHING under `name`: every read of it is refused with
/// the modelled code, status and sentence.
async fn assert_no_such_secret(router: &MultiServiceRouter, case: &str, name: &str) {
    for operation in ["DescribeSecret", "GetSecretValue"] {
        let (code, refused) = secretsmanager(router, operation, &json!({"SecretId": name})).await;
        assert_eq!(
            code, 400,
            "case {case:?}: expected {operation}({name}) to be refused once the disposal is \
             reported, got {code} {refused}"
        );
        assert_eq!(
            error_code(case, &refused),
            SECRET_NOT_FOUND,
            "case {case:?}: expected {operation}({name}) to be refused with \
             {SECRET_NOT_FOUND}, got {refused}"
        );
        assert_eq!(
            message(&refused),
            Some(SECRET_NOT_FOUND_MESSAGE),
            "case {case:?}: expected the refusal of {operation}({name}) to carry \
             {SECRET_NOT_FOUND_MESSAGE:?}, got {refused}"
        );
    }
}

/// Assert that Secrets Manager serves `name` and that its value is `expected` — the read that
/// says a secret the disposal was not asked about is untouched.
async fn assert_secret_is_served(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &str,
) {
    let (code, described) =
        secretsmanager(router, "DescribeSecret", &json!({"SecretId": name})).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected DescribeSecret({name}) to report the secret, got {code} \
         {described}"
    );
    assert!(
        described.get("DeletedDate").is_none(),
        "case {case:?}: expected DescribeSecret({name}) to report a secret no deletion is \
         scheduled for, got {described}"
    );
    let (code, served) = secretsmanager(router, "GetSecretValue", &json!({"SecretId": name})).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected GetSecretValue({name}) to serve the value, got {code} {served}"
    );
    assert_eq!(
        served.get("SecretString").and_then(Value::as_str),
        Some(expected),
        "case {case:?}: expected the untouched value {expected:?}, got {served}"
    );
}

#[tokio::test]
async fn a_rolled_back_create_purges_the_secrets_its_policies_dispose_of() {
    let router = router();
    // Whoever holds the colliding name first: a secret of the account's own, which the
    // declaration below cannot have.
    let (code, taken) = create_secret(&router, COLLISION_NAME, COLLISION_VALUE, 0).await;
    assert_eq!(
        code, 200,
        "the colliding name must be taken first, got {code} {taken}"
    );

    // The create reaches the owning service only with the pre-deployment existence check
    // OFF: with it on the name conflict fails the create before any resource is asked for,
    // and then the rollback has nothing of its own to dispose of (the second case covers
    // that path end to end).
    let (code, body) = create_stack(
        &router,
        STACK,
        &template(),
        &[member("DisableValidation", "true")],
    )
    .await;
    assert_eq!(
        code, 200,
        "CreateStack must answer 200 — the create starts, and the stack reports the outcome \
         — got {code} {body}"
    );
    // Waits for the CONDITION: the first status the stack reports that is no longer an
    // operation in flight. Everything after this read is what a client sees the moment the
    // rollback ended.
    let described = settled_stack(&router, "the rolled-back create", STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "expected the refused create to be rolled back, got {described}"
    );

    // The declaration whose name Secrets Manager already held is the one the create failed
    // on, and the per-resource event carries that service's OWN refusal.
    let failed = event_statuses_including(
        &router,
        "the declaration whose name was taken",
        STACK,
        "ZCollision",
        resource_status::CREATE_FAILED,
    )
    .await;
    let reason = resource_event_reason(&router, STACK, "ZCollision", resource_status::CREATE_FAILED)
        .await;
    assert!(
        reason.contains(&format!(
            "The operation failed because the secret {COLLISION_NAME} already exists."
        )),
        "expected the owning service's own refusal on the failed resource's event, got \
         {reason:?} among {failed:?}"
    );

    for (nth, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // The create really MADE this row's secret before the failure — the stack's own event
        // stream says so — which is what makes the rollback's disposal of it a disposal of
        // something.
        event_statuses_including(
            &router,
            case,
            STACK,
            row.logical_id,
            resource_status::CREATE_COMPLETE,
        )
        .await;
        if row.kept {
            // Nothing at all was asked of the owning service for it: the one thing the stack
            // records is that its disposal was SKIPPED.
            let statuses = event_statuses_including(
                &router,
                case,
                STACK,
                row.logical_id,
                resource_status::DELETE_SKIPPED,
            )
            .await;
            assert!(
                !statuses
                    .iter()
                    .any(|status| status == resource_status::DELETE_IN_PROGRESS),
                "case {case:?}: a retained resource's deletion is never opened, got \
                 {statuses:?}"
            );
            assert_secret_is_served(&router, case, row.secret_name, &declared_value(row)).await;
            continue;
        }
        event_statuses_including(
            &router,
            case,
            STACK,
            row.logical_id,
            resource_status::DELETE_COMPLETE,
        )
        .await;
        // ...and the record is BACKED: that service holds nothing under the name, on the
        // FIRST read after the rollback ended.
        assert_no_such_secret(&router, case, row.secret_name).await;
        // ...so the NAME is free again. A secret merely scheduled for deletion still holds
        // its name, and a create of that name is refused while it does, so this is what
        // tells a purge from a schedule.
        let (code, recreated) =
            create_secret(&router, row.secret_name, "the name is free again", nth + 1).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected the name the rollback freed to take a new secret, got \
             {code} {recreated}"
        );
    }

    // The secret that was already there is no part of what the stack created, so the failed
    // create and its rollback leave it exactly as it was.
    assert_secret_is_served(
        &router,
        "the secret that was already there",
        COLLISION_NAME,
        COLLISION_VALUE,
    )
    .await;
}

/// The stack of the recorded shape, deployed twice over one `Retain` secret.
const REDEPLOY_STACK: &str = "rollback-secret-redeploy";

/// The secret the template retains, which the first deployment leaves behind and the second
/// deployment's create cannot have.
const RETAINED_SECRET: &str = "rollback-secret-redeploy-retained";

/// The secret the template disposes of, whose name the second deployment therefore finds free
/// — and which its rollback must leave nothing of.
const DROPPED_SECRET: &str = "rollback-secret-redeploy-dropped";

/// The value both secrets of that template hold.
const REDEPLOY_VALUE: &str = "{\"username\":\"probeadmin\",\"password\":\"a-declared-example\"}";

/// The template of the second case: one secret kept whatever becomes of the stack, one
/// disposed of with it.
fn redeploy_template() -> String {
    json!({
        "Resources": {
            "DroppedSecret": {
                "Type": "AWS::SecretsManager::Secret",
                "DeletionPolicy": deletion_policy::DELETE,
                "UpdateReplacePolicy": deletion_policy::DELETE,
                "Properties": {
                    "Name": DROPPED_SECRET,
                    "Description": "the secret whose DeletionPolicy is Delete",
                    "SecretString": REDEPLOY_VALUE
                }
            },
            "RetainedSecret": {
                "Type": "AWS::SecretsManager::Secret",
                "DeletionPolicy": deletion_policy::RETAIN,
                "UpdateReplacePolicy": deletion_policy::RETAIN,
                "DependsOn": ["DroppedSecret"],
                "Properties": {
                    "Name": RETAINED_SECRET,
                    "Description": "the secret whose DeletionPolicy is Retain",
                    "SecretString": REDEPLOY_VALUE
                }
            }
        }
    })
    .to_string()
}

#[tokio::test]
async fn a_redeploy_over_a_retained_secret_leaves_nothing_of_the_dropped_one() {
    let router = router();
    let body = redeploy_template();

    // The first deployment: both secrets really exist in the owning service, read back
    // through its own wire by the names the declarations state.
    deploy(&router, REDEPLOY_STACK, &body).await;
    for name in [DROPPED_SECRET, RETAINED_SECRET] {
        assert_secret_is_served(&router, "the first deployment", name, REDEPLOY_VALUE).await;
    }

    // The teardown: the `Retain` secret survives, the `Delete` one is gone and its name is
    // free — which is what lets the second deployment create it again.
    let (code, deleted) = call(
        &router,
        "DeleteStack",
        &[member("StackName", REDEPLOY_STACK)],
    )
    .await;
    assert_eq!(
        code, 200,
        "DeleteStack must answer 200, got {code} {deleted}"
    );
    let (code, described) = describe_stack(&router, REDEPLOY_STACK).await;
    assert_eq!(
        code, 200,
        "DescribeStacks must answer 200 while the teardown runs, got {code} {described}"
    );
    let stack_id = xml_text(&described, "StackId").unwrap_or_default();
    let described = settled_stack(&router, "the teardown", &stack_id).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::DELETE_COMPLETE),
        "expected the teardown to complete, got {described}"
    );
    assert_no_such_secret(&router, "the teardown", DROPPED_SECRET).await;
    assert_secret_is_served(&router, "the teardown", RETAINED_SECRET, REDEPLOY_VALUE).await;

    // The identical template again, under the same stack name: the `Retain` secret the first
    // deployment left behind is one this create cannot have, so the create fails and the
    // stack rolls back.
    let (code, recreated) = create_stack(&router, REDEPLOY_STACK, &body, &[]).await;
    assert_eq!(
        code, 200,
        "the second CreateStack must answer 200, got {code} {recreated}"
    );
    let described = settled_stack(&router, "the redeploy", REDEPLOY_STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "expected the redeploy over the retained secret to roll back, got {described}"
    );

    // What the rollback left: nothing under the `Delete` secret's name — whether the create
    // had made it and the rollback disposed of it, or the create never got that far — and
    // the `Retain` secret untouched, value and all.
    assert_no_such_secret(&router, "the rolled-back redeploy", DROPPED_SECRET).await;
    assert_secret_is_served(
        &router,
        "the rolled-back redeploy",
        RETAINED_SECRET,
        REDEPLOY_VALUE,
    )
    .await;
}

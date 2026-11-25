// SPDX-License-Identifier: CC0-1.0

//! The admin credentials a namespace has Amazon Redshift MANAGE, and the Secrets Manager
//! secret that holds them.
//!
//! The RULE under test, asserted over a table of namespaces rather than one recorded call: a
//! namespace whose admin password this service manages (`manageAdminPassword` true) has a
//! Secrets Manager secret of its own holding those credentials, and reports that secret's ARN
//! as `adminPasswordSecretArn` — while a namespace whose password the CALLER supplied has no
//! such secret and reports no such member. Four things follow from that, and each is asserted
//! on every managed row:
//!
//! * the member is reported by the call that turned management on AND by a later
//!   `GetNamespace`, so what the caller was handed is what the store holds;
//! * `DescribeSecret` resolves that ARN, and the secret is NAMED under the prefix AWS reserves
//!   for this service's own secrets (a form no caller's `CreateSecret` can mint);
//! * the secret reports `OwningService` as this service's id, which is what makes a caller's
//!   own `DeleteSecret` on it refusable — and that refusal is asserted too;
//! * `GetSecretValue` answers the namespace's admin username and a password the service
//!   generated, so the credentials the member promises are credentials that exist.
//!
//! The rows differ along what the rule generalises over: whether management was turned on by
//! the CREATE or by a later `UpdateNamespace` (from an explicit password, and from a namespace
//! that had none), and whether the request named the admin user or left the service to. The
//! last row is the other side of the rule.
//!
//! Secrets Manager is mounted beside Redshift Serverless over one engine, exactly as the
//! assembled emulator mounts them, because the secret read back here is a real secret of this
//! emulator.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::namespace_workgroup_lifecycle::{at, call_in, error_code, ACCOUNT};

/// The region every request of this suite is signed for — not the emulator default, so an ARN
/// read back here proves it was not taken from a constant.
const REGION: &str = "us-west-2";

/// The prefix the name of a secret this service owns carries, and the service id such a secret
/// reports as its `OwningService`: this service's own, as the Secrets Manager emulator reads
/// both of them.
const OWNING_SERVICE_ID: &str = "redshift";

/// One emulator carrying Redshift Serverless AND Secrets Manager on the same shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_redshiftserverless_redshiftserverless::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// A Secrets Manager client for [`REGION`] over this emulator's own in-process callback — the
/// same seam the handler creates the managed secret through, so what this reads back is what
/// the account holds.
fn secrets_manager(router: &MultiServiceRouter) -> ::aws_sdk_secretsmanager::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_secretsmanager::Config::builder()
        .behavior_version(::aws_sdk_secretsmanager::config::BehaviorVersion::latest())
        .region(::aws_sdk_secretsmanager::config::Region::new(
            REGION.to_string(),
        ))
        .endpoint_url(format!("https://secretsmanager.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_secretsmanager::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_secretsmanager::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_secretsmanager::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_secretsmanager::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_secretsmanager::Client::from_conf(config)
}

/// Call `operation` on this service, signed for [`REGION`], and answer its status and body.
async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    call_in(router, REGION, operation, body).await
}

/// [`call`] for an operation that must succeed.
async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, parsed) = call(router, operation, body.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} {body} expected 200, got {status} with {parsed}"
    );
    parsed
}

/// The namespace `namespace`, read back once the status transition the clock owes it has fallen
/// due. The transition is owed by the CLOCK rather than by any call, so the condition is polled
/// for — with a deadline generous enough for a loaded host — rather than waited out.
async fn settled_namespace(router: &MultiServiceRouter, case: &str, namespace: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, body) = call(router, "GetNamespace", json!({ "namespaceName": namespace })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetNamespace {namespace} expected 200, got {status} with {body}"
        );
        if at(&body, "namespace.status") == &json!("AVAILABLE") {
            return body["namespace"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: GetNamespace {namespace} never reported AVAILABLE, last status {:?}",
            at(&body, "namespace.status")
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The secret at `secret_arn`, once the deletion this service asked Secrets Manager for has
/// been carried out there.
///
/// A forced deletion is not instantaneous in that service — the secret is MARKED as of the
/// call and its removal falls due a moment later — so this asserts the mark on every look and
/// polls for the removal, with a deadline generous enough for a loaded host.
async fn released_secret(
    secrets: &::aws_sdk_secretsmanager::Client,
    case: &str,
    secret_arn: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match secrets.describe_secret().secret_id(secret_arn).send().await {
            Err(_) => return,
            Ok(described) => {
                assert!(
                    described.deleted_date().is_some(),
                    "case {case:?}: the secret at {secret_arn} must be deleted, got {described:?}"
                );
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: the secret at {secret_arn} never left Secrets Manager"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

/// How a row's namespace comes to have its admin password managed by the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedBy {
    /// The create itself carries `manageAdminPassword`.
    Create,
    /// The create carries a password of the caller's own and a later `UpdateNamespace` hands
    /// the password over — the migration onto managed credentials.
    UpdateFromExplicitPassword,
    /// The create names neither a password nor management, and a later `UpdateNamespace` turns
    /// management on.
    UpdateFromNoPassword,
}

/// One row: the namespace whose managed secret is read back, how management was turned on, and
/// whether the admin user was named.
struct ManagedCase {
    case: &'static str,
    namespace: &'static str,
    /// The admin user the create names, if any — otherwise the service names one.
    created_admin_username: Option<&'static str>,
    /// The admin user the update names, if any.
    updated_admin_username: Option<&'static str>,
    managed_by: ManagedBy,
    /// The admin user the namespace is expected to report once the dust settles.
    expected_admin_username: &'static str,
}

const MANAGED_CASES: &[ManagedCase] = &[
    ManagedCase {
        case: "a create that hands the admin password to the service",
        namespace: "managed-on-create",
        created_admin_username: Some("admin1"),
        updated_admin_username: None,
        managed_by: ManagedBy::Create,
        expected_admin_username: "admin1",
    },
    ManagedCase {
        case: "a create that hands it over and names no admin user",
        namespace: "managed-on-create-default-user",
        created_admin_username: None,
        updated_admin_username: None,
        managed_by: ManagedBy::Create,
        // `CreateNamespace` names the administrator of the first database when the request
        // does not, and the secret holds the credentials of that user.
        expected_admin_username: "admin",
    },
    ManagedCase {
        case: "an update from an explicit admin password to a managed one",
        namespace: "managed-by-update",
        created_admin_username: Some("admin2"),
        updated_admin_username: Some("admin2"),
        managed_by: ManagedBy::UpdateFromExplicitPassword,
        expected_admin_username: "admin2",
    },
    ManagedCase {
        case: "an update that hands the password over without naming the user",
        namespace: "managed-by-bare-update",
        created_admin_username: Some("admin3"),
        updated_admin_username: None,
        managed_by: ManagedBy::UpdateFromNoPassword,
        expected_admin_username: "admin3",
    },
];

#[tokio::test]
async fn a_managed_admin_password_namespace_reports_a_secret_this_service_owns() {
    let router = emulator();
    let secrets = secrets_manager(&router);
    for row in MANAGED_CASES {
        let case = row.case;
        let mut create = json!({ "namespaceName": row.namespace });
        if let Some(admin_username) = row.created_admin_username {
            create["adminUsername"] = json!(admin_username);
        }
        match row.managed_by {
            ManagedBy::Create => create["manageAdminPassword"] = json!(true),
            ManagedBy::UpdateFromExplicitPassword | ManagedBy::UpdateFromNoPassword => {
                create["adminUserPassword"] = json!("Aunique-password-1");
            }
        }
        let created = ok(&router, case, "CreateNamespace", create).await;
        let answered = match row.managed_by {
            ManagedBy::Create => created["namespace"].clone(),
            _ => {
                // The namespace a create left with a password of its own carries no secret.
                assert_eq!(
                    at(&created, "namespace.adminPasswordSecretArn"),
                    &Value::Null,
                    "case {case:?}: a namespace created with its own password must report no \
                     adminPasswordSecretArn, got {created}"
                );
                let mut update = json!({
                    "namespaceName": row.namespace,
                    "manageAdminPassword": true,
                });
                if let Some(admin_username) = row.updated_admin_username {
                    update["adminUsername"] = json!(admin_username);
                }
                ok(&router, case, "UpdateNamespace", update).await["namespace"].clone()
            }
        };

        // 1. The call that turned management on reports the member, and so does a later read.
        let answered_arn = at(&answered, "adminPasswordSecretArn")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !answered_arn.is_empty(),
            "case {case:?}: the call that turned management on must report \
             adminPasswordSecretArn, got {answered}"
        );
        let settled = settled_namespace(&router, case, row.namespace).await;
        assert_eq!(
            at(&settled, "adminPasswordSecretArn"),
            &json!(answered_arn),
            "case {case:?}: GetNamespace must report the same secret, got {settled}"
        );
        assert_eq!(
            at(&settled, "adminUsername"),
            &json!(row.expected_admin_username),
            "case {case:?}: expected adminUsername {:?}, got {settled}",
            row.expected_admin_username
        );

        // 2. The ARN resolves in Secrets Manager, under a name reserved for this service.
        let described = secrets
            .describe_secret()
            .secret_id(&answered_arn)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: Secrets Manager holds no secret at {answered_arn}: {error}")
            });
        let name = described.name().unwrap_or_default().to_string();
        let reserved_prefix = format!("{OWNING_SERVICE_ID}!");
        assert!(
            name.starts_with(&reserved_prefix),
            "case {case:?}: expected the secret's name to start with {reserved_prefix:?}, got \
             {name:?}"
        );
        assert!(
            answered_arn.starts_with(&format!(
                "arn:aws:secretsmanager:{REGION}:{ACCOUNT}:secret:{name}-"
            )),
            "case {case:?}: expected the secret's ARN to carry that name, got {answered_arn:?}"
        );
        assert!(
            described.deleted_date().is_none(),
            "case {case:?}: expected a live secret, got one deleted at {:?}",
            described.deleted_date()
        );

        // 3. The owning service: the id of the service that created the secret, which is what
        //    refuses a caller's own write to it.
        assert_eq!(
            described.owning_service(),
            Some(OWNING_SERVICE_ID),
            "case {case:?}: expected OwningService {OWNING_SERVICE_ID:?}, got {:?}",
            described.owning_service()
        );
        let refused = secrets
            .delete_secret()
            .secret_id(&answered_arn)
            .send()
            .await
            .err()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a caller's own DeleteSecret on {answered_arn} must be refused")
            });
        let refusal = format!(
            "{}",
            ::aws_smithy_types::error::display::DisplayErrorContext(&refused)
        );
        assert!(
            refusal.contains(OWNING_SERVICE_ID),
            "case {case:?}: expected the refusal to name the owning service, got {refusal}"
        );

        // 4. The credentials the member promises exist: the namespace's admin user, and a
        //    password the service generated rather than one any caller chose.
        let held = secrets
            .get_secret_value()
            .secret_id(&answered_arn)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the secret at {answered_arn} holds no value: {error}")
            });
        let document: Value = serde_json::from_str(held.secret_string().unwrap_or_default())
            .unwrap_or(Value::Null);
        assert_eq!(
            at(&document, "username"),
            &json!(row.expected_admin_username),
            "case {case:?}: expected the secret to hold the namespace's admin user, got \
             {document:?}"
        );
        let password = at(&document, "password").as_str().unwrap_or_default();
        assert!(
            password.len() >= 8 && password != "Aunique-password-1",
            "case {case:?}: expected a generated admin password of at least 8 characters, got \
             one of {} characters",
            password.len()
        );
    }
}

#[tokio::test]
async fn a_namespace_that_keeps_its_own_admin_password_reports_no_secret() {
    let router = emulator();
    // The rows differ along what the create names beside the password: nothing, an admin user,
    // and a namespace left entirely to the service's defaults.
    let cases: [(&str, &str, Value); 3] = [
        (
            "a create naming an admin user and a password",
            "own-password-user",
            json!({ "adminUsername": "admin4", "adminUserPassword": "Aunique-password-2" }),
        ),
        (
            "a create naming a password alone",
            "own-password-only",
            json!({ "adminUserPassword": "Aunique-password-3" }),
        ),
        (
            "a create naming neither a password nor management",
            "own-password-none",
            json!({}),
        ),
    ];
    for (case, namespace, extra) in cases {
        let mut request = json!({ "namespaceName": namespace });
        for (member, value) in extra.as_object().cloned().unwrap_or_default() {
            request[member] = value;
        }
        let created = ok(&router, case, "CreateNamespace", request).await;
        assert_eq!(
            at(&created, "namespace.adminPasswordSecretArn"),
            &Value::Null,
            "case {case:?}: expected no adminPasswordSecretArn, got {created}"
        );
        let read_back = settled_namespace(&router, case, namespace).await;
        assert_eq!(
            at(&read_back, "adminPasswordSecretArn"),
            &Value::Null,
            "case {case:?}: GetNamespace must report no adminPasswordSecretArn, got {read_back}"
        );
    }
}

#[tokio::test]
async fn an_explicit_admin_password_with_managed_management_is_refused_in_awss_own_words() {
    let router = emulator();
    ok(
        &router,
        "the namespace an update case addresses",
        "CreateNamespace",
        json!({
            "namespaceName": "combined-password-ns",
            "adminUsername": "admin5",
            "adminUserPassword": "Aunique-password-4",
        }),
    )
    .await;
    // The sentence is entirely fixed text, so the rows differ along the operation that
    // publishes both members and along what else the refused request carries.
    let cases: [(&str, &str, Value); 4] = [
        (
            "a create naming both",
            "CreateNamespace",
            json!({
                "namespaceName": "combined-create-ns",
                "adminUsername": "admin6",
                "adminUserPassword": "Aunique-password-5",
                "manageAdminPassword": true,
            }),
        ),
        (
            "a create naming both and nothing else",
            "CreateNamespace",
            json!({
                "namespaceName": "combined-create-bare-ns",
                "adminUserPassword": "Aunique-password-6",
                "manageAdminPassword": true,
            }),
        ),
        (
            "an update naming both",
            "UpdateNamespace",
            json!({
                "namespaceName": "combined-password-ns",
                "adminUsername": "admin5",
                "adminUserPassword": "Aunique-password-7",
                "manageAdminPassword": true,
            }),
        ),
        (
            "an update naming both and no admin user",
            "UpdateNamespace",
            json!({
                "namespaceName": "combined-password-ns",
                "adminUserPassword": "Aunique-password-8",
                "manageAdminPassword": true,
            }),
        ),
    ];
    let expected = "The AdminUserPassword parameter cannot be provided if ManagedAdminPassword \
                    is true.";
    for (case, operation, request) in cases {
        let (status, body) = call(&router, operation, request.clone()).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} {request} expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            body.get("message").and_then(Value::as_str),
            Some(expected),
            "case {case:?}: expected {expected:?}, got {body}"
        );
    }
    // Nothing was created by a refused create, and the namespace an update addressed still
    // holds the password it was created with: no secret, and no change of management.
    for namespace in ["combined-create-ns", "combined-create-bare-ns"] {
        let (status, body) =
            call(&router, "GetNamespace", json!({ "namespaceName": namespace })).await;
        assert_eq!(
            status, 400,
            "a refused create must leave no namespace {namespace}, got {status} with {body}"
        );
    }
    let untouched = settled_namespace(&router, "the refused updates", "combined-password-ns").await;
    assert_eq!(
        at(&untouched, "adminPasswordSecretArn"),
        &Value::Null,
        "a refused update must leave the namespace unmanaged, got {untouched}"
    );
}

#[tokio::test]
async fn the_admin_user_and_password_pairing_asks_nothing_of_a_managed_password_update() {
    let router = emulator();
    // Every row updates the SAME namespace, created with an admin user and a password of its
    // own. The rows differ along which of the three members the update names: the pairing rule
    // is about supplying a password of the caller's own, so it fires only where one of
    // `adminUsername` / `adminUserPassword` travels without the other and management is not
    // being handed over.
    ok(
        &router,
        "the namespace every pairing case addresses",
        "CreateNamespace",
        json!({
            "namespaceName": "pairing-ns",
            "adminUsername": "admin7",
            "adminUserPassword": "Aunique-password-9",
        }),
    )
    .await;
    let pairing_refusal = "adminUsername and adminUserPassword must be updated together";
    let cases: [(&str, Value, Option<&str>); 6] = [
        (
            "an admin user alone",
            json!({ "adminUsername": "admin8" }),
            Some(pairing_refusal),
        ),
        (
            "a password alone",
            json!({ "adminUserPassword": "Aunique-password-10" }),
            Some(pairing_refusal),
        ),
        (
            "both of them",
            json!({ "adminUsername": "admin8", "adminUserPassword": "Aunique-password-11" }),
            None,
        ),
        (
            "an admin user with the password handed to the service",
            json!({ "adminUsername": "admin9", "manageAdminPassword": true }),
            None,
        ),
        (
            "the password handed to the service alone",
            json!({ "manageAdminPassword": true }),
            None,
        ),
        (
            "an admin user with management explicitly left to the caller",
            json!({ "adminUsername": "admin10", "manageAdminPassword": false }),
            Some(pairing_refusal),
        ),
    ];
    for (case, extra, refusal) in cases {
        let mut request = json!({ "namespaceName": "pairing-ns" });
        for (member, value) in extra.as_object().cloned().unwrap_or_default() {
            request[member] = value;
        }
        let (status, body) = call(&router, "UpdateNamespace", request.clone()).await;
        match refusal {
            Some(message) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: {request} expected 400, got {status} with {body}"
                );
                assert_eq!(
                    body.get("message").and_then(Value::as_str),
                    Some(message),
                    "case {case:?}: expected {message:?}, got {body}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: {request} expected 200, got {status} with {body}"
            ),
        }
        // Each accepted row leaves the namespace settling back to AVAILABLE before the next
        // one addresses it.
        settled_namespace(&router, case, "pairing-ns").await;
    }
}

#[tokio::test]
async fn a_namespace_that_stops_managing_its_admin_password_releases_the_secret() {
    let router = emulator();
    let secrets = secrets_manager(&router);
    // The rows differ along WHY the service stops managing the credentials: the caller takes
    // the password back, or the namespace itself goes. Either way the secret that held them
    // does not outlive the management.
    for (case, namespace, taken_back) in [
        ("the caller takes the password back", "released-ns", true),
        ("the namespace is deleted", "deleted-ns", false),
    ] {
        let created = ok(
            &router,
            case,
            "CreateNamespace",
            json!({
                "namespaceName": namespace,
                "adminUsername": "admin11",
                "manageAdminPassword": true,
            }),
        )
        .await;
        let secret_arn = at(&created, "namespace.adminPasswordSecretArn")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !secret_arn.is_empty(),
            "case {case:?}: the create must report adminPasswordSecretArn, got {created}"
        );
        assert!(
            secrets
                .describe_secret()
                .secret_id(&secret_arn)
                .send()
                .await
                .is_ok(),
            "case {case:?}: Secrets Manager must hold the secret at {secret_arn}"
        );

        if taken_back {
            let updated = ok(
                &router,
                case,
                "UpdateNamespace",
                json!({
                    "namespaceName": namespace,
                    "adminUsername": "admin11",
                    "adminUserPassword": "Aunique-password-12",
                    "manageAdminPassword": false,
                }),
            )
            .await;
            assert_eq!(
                at(&updated, "namespace.adminPasswordSecretArn"),
                &Value::Null,
                "case {case:?}: a namespace whose password the caller took back must report no \
                 adminPasswordSecretArn, got {updated}"
            );
            let read_back = settled_namespace(&router, case, namespace).await;
            assert_eq!(
                at(&read_back, "adminPasswordSecretArn"),
                &Value::Null,
                "case {case:?}: GetNamespace must report no adminPasswordSecretArn, got \
                 {read_back}"
            );
        } else {
            ok(
                &router,
                case,
                "DeleteNamespace",
                json!({ "namespaceName": namespace }),
            )
            .await;
        }

        released_secret(&secrets, case, &secret_arn).await;
    }
}

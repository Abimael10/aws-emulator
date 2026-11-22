// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ModifyTenantDatabase`.
//!
//! Every call goes over the wire as a real awsQuery form POST carrying the SigV4 credential
//! scope the router keys on, and the assertions read the XML the emulator answered with, so
//! a handler that computed nothing could not pass them.
//!
//! The RULE under test: a modification of a tenant database is APPLIED by the call that
//! made it, and that call answers the tenant database as it WAS with what it changed named
//! in `TenantDatabasePendingModifiedValues` — "changes that will be applied. Specific
//! changes are identified by subelements" — so the next caller finds the tenant database
//! under the name the modification gave it, with the immutable identity
//! (`TenantDatabaseResourceId`, `TenantDatabaseARN`, `TenantDatabaseCreateTime`) it was
//! created with and no pending values of its own. The secret RDS manages for the master user
//! password is Secrets Manager's own resource, minted, rotated or deleted while the call
//! runs. Every refusal is one a published constraint or the DB instance's own configuration
//! states.
//!
//! The tables below span what the rule generalises over: the shortest name and the longest
//! one `NewTenantDBName` publishes (and one character past it), a rename, a password change,
//! both at once and neither, a rename that differs from the current name only in case (the
//! member "isn't case-sensitive"), tenant databases whose password the caller supplied and
//! ones RDS manages, and each documented refusal.
//!
//! Secrets Manager and KMS are MOUNTED beside the RDS service
//! ([`crate::managed_master_user_password::emulator`]), because the managed-secret path
//! calls them.

use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, expect_error, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;
use crate::managed_master_user_password::{emulator, held_credentials, master_user_secret};

/// The instance class every DB instance below is created with; which class an instance runs
/// on is nothing this operation reads.
const INSTANCE_CLASS: &str = "db.m5.large";

/// One of the Oracle container-database engines, which is what makes an instance one that
/// holds tenant databases at all (`st::is_multi_tenant_engine`).
const CDB_ENGINE: &str = "oracle-ee-cdb";

/// The master user every tenant database below is created with.
const TENANT_MASTER_USERNAME: &str = "tenantadmin";

/// A password the Oracle engine's own `MasterUserPassword` rule accepts, for a tenant
/// database whose password the caller supplies.
const SUPPLIED_PASSWORD: &str = "a-strong-password";

/// What `PendingModifiedValues$MasterUserPassword` reports while a password change is
/// pending: the member is `@sensitive`, so the masked form is reported and never the value.
const MASKED_PASSWORD: &str = "****";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The query parameters of one call, OWNED, so a row can hold them and a caller can add to
/// them.
macro_rules! owned_params {
    ($($name:expr => $value:expr),* $(,)?) => {
        params![$($name => $value),*].to_vec()
    };
}

/// One row of the modification table: the tenant database a row modifies, and what it asks
/// for.
struct ModificationCase {
    case: &'static str,
    /// The `TenantDBName` the row creates.
    created: &'static str,
    /// `NewTenantDBName`, absent for a row that renames nothing.
    renamed_to: Option<&'static str>,
    /// `MasterUserPassword`, absent for a row that changes no password.
    password: Option<&'static str>,
    /// The name the tenant database answers to once the call has returned: the new one for a
    /// rename, the created one otherwise.
    answers_to: &'static str,
}

/// `CreateDBInstance` on a container-database engine, followed by a poll loop until the
/// instance reports `available` — the state a tenant database is created in.
async fn create_available_instance(router: &MultiServiceRouter, identifier: &str, engine: &str) {
    expect_ok(
        &format!("create instance {identifier}"),
        call(
            router,
            "CreateDBInstance",
            &params![
                "DBInstanceIdentifier" => identifier,
                "Engine" => engine,
                "DBInstanceClass" => INSTANCE_CLASS,
                "MasterUsername" => "oracleadmin",
                "MasterUserPassword" => SUPPLIED_PASSWORD,
                "MultiTenant" => "true",
            ],
        )
        .await,
    );
    let read = params!["DBInstanceIdentifier" => identifier];
    poll_until(
        identifier,
        "the DB instance reaching available",
        || call(router, "DescribeDBInstances", &read),
        |status, body| {
            status == 200 && optional_text(body, "DBInstanceStatus").as_deref() == Some("available")
        },
    )
    .await;
}

/// `CreateTenantDatabase` followed by a poll loop until the tenant database reports
/// `available`. `extra` carries the members a row adds (the managed-password ones).
///
/// Answers the `<TenantDatabase>` envelope the report settled on.
async fn create_available_tenant(
    router: &MultiServiceRouter,
    case: &str,
    instance: &str,
    tenant_db_name: &str,
    extra: &[(&'static str, String)],
) -> String {
    let mut parameters = owned_params![
        "DBInstanceIdentifier" => instance,
        "TenantDBName" => tenant_db_name,
        "MasterUsername" => TENANT_MASTER_USERNAME,
    ];
    parameters.extend(extra.iter().cloned());
    expect_ok(
        &format!("{case}: create tenant database {tenant_db_name}"),
        call(router, "CreateTenantDatabase", &parameters).await,
    );
    let settled = poll_until(
        case,
        "the tenant database reaching available",
        || read_tenant(router, instance, tenant_db_name),
        |status, body| status == 200 && optional_text(body, "Status").as_deref() == Some("available"),
    )
    .await
    .0;
    the_tenant(case, &settled)
}

/// `DescribeTenantDatabases` narrowed to one tenant database of one DB instance.
async fn read_tenant(
    router: &MultiServiceRouter,
    instance: &str,
    tenant_db_name: &str,
) -> (u16, String) {
    call(
        router,
        "DescribeTenantDatabases",
        &params![
            "DBInstanceIdentifier" => instance,
            "TenantDBName" => tenant_db_name,
        ],
    )
    .await
}

/// The one `<TenantDatabase>` subtree a response carries.
fn the_tenant(case: &str, body: &str) -> String {
    let found = subtrees(body, "TenantDatabase");
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one TenantDatabase, got {} in {body}",
        found.len()
    );
    found.into_iter().next().expect("checked just above")
}

/// The `PendingModifiedValues` of a tenant database envelope, as
/// `(TenantDBName, MasterUserPassword)` — both absent when nothing is pending, whether the
/// element is left out or carries no member.
fn pending_values(case: &str, envelope: &str) -> (Option<String>, Option<String>) {
    let found = subtrees(envelope, "PendingModifiedValues");
    assert!(
        found.len() <= 1,
        "case {case:?}: expected at most one PendingModifiedValues, got {} in {envelope}",
        found.len()
    );
    match found.into_iter().next() {
        Some(subtree) => (
            optional_text(&subtree, "TenantDBName"),
            optional_text(&subtree, "MasterUserPassword"),
        ),
        None => (None, None),
    }
}

#[tokio::test]
async fn a_modification_is_applied_and_answered_with_what_it_changed() {
    // The rows differ along what the rule generalises over: the shortest name and the
    // longest one `NewTenantDBName` publishes ("Can't be longer than 8 characters"), a
    // rename alone, a password change alone, both at once, and a rename to the name the
    // tenant database already carries in another case — which the member "isn't
    // case-sensitive" makes no change at all.
    let cases = [
        ModificationCase {
            case: "renamed to the shortest name",
            created: "tenant1",
            renamed_to: Some("t"),
            password: None,
            answers_to: "t",
        },
        ModificationCase {
            case: "renamed to the longest published name",
            created: "tenant2",
            renamed_to: Some("eightchr"),
            password: None,
            answers_to: "eightchr",
        },
        ModificationCase {
            case: "the master user password alone",
            created: "tenant3",
            renamed_to: None,
            password: Some("pass1234"),
            answers_to: "tenant3",
        },
        ModificationCase {
            case: "renamed and the password changed at once",
            created: "tenant4",
            renamed_to: Some("renamed"),
            // The longest password the Oracle engine publishes: 30 characters.
            password: Some("abcdefghijklmnopqrstuvwxyz1234"),
            answers_to: "renamed",
        },
        ModificationCase {
            case: "renamed to the name it already carries, in another case",
            created: "tenant5",
            renamed_to: Some("TENANT5"),
            password: None,
            answers_to: "tenant5",
        },
    ];
    let router = emulator();
    let instance = "emu-cdb-modified";
    create_available_instance(&router, instance, CDB_ENGINE).await;

    for ModificationCase {
        case,
        created,
        renamed_to,
        password,
        answers_to,
    } in cases
    {
        let before = create_available_tenant(
            &router,
            case,
            instance,
            created,
            &[("MasterUserPassword", SUPPLIED_PASSWORD.to_string())],
        )
        .await;
        let resource_id = text(&before, "TenantDatabaseResourceId");
        let arn = text(&before, "TenantDatabaseARN");
        let created_at = text(&before, "TenantDatabaseCreateTime");

        let mut parameters = owned_params![
            "DBInstanceIdentifier" => instance,
            "TenantDBName" => created,
        ];
        if let Some(new_name) = renamed_to {
            parameters.push(("NewTenantDBName", new_name.to_string()));
        }
        if let Some(new_password) = password {
            parameters.push(("MasterUserPassword", new_password.to_string()));
        }
        let answered = expect_ok(case, call(&router, "ModifyTenantDatabase", &parameters).await);
        if let Some(new_password) = password {
            assert!(
                !answered.contains(new_password),
                "case {case:?}: the password must never be reported: {answered}"
            );
        }
        let modified = the_tenant(case, &answered);
        // The answer is the tenant database as it WAS: the modelled pending values are what
        // name the change, so the name in the envelope is still the one it was called by.
        assert_eq!(
            text(&modified, "TenantDBName"),
            created,
            "case {case:?}: expected the name the tenant database was called by, got {modified}"
        );
        // A rename asking for the name it already carries changes nothing, so it reports
        // nothing; everything else the call changed is named in the pending values.
        let renames = renamed_to.filter(|new_name| !new_name.eq_ignore_ascii_case(created));
        assert_eq!(
            pending_values(case, &modified),
            (
                renames.map(str::to_string),
                password.map(|_| MASKED_PASSWORD.to_string())
            ),
            "case {case:?}: expected the change as the pending values, got {modified}"
        );
        // Identity and creation time are the tenant database's own and survive a
        // modification: "the Region-unique, IMMUTABLE identifier for the tenant database".
        for (member, expected) in [
            ("TenantDatabaseResourceId", &resource_id),
            ("TenantDatabaseARN", &arn),
            ("TenantDatabaseCreateTime", &created_at),
            ("MasterUsername", &TENANT_MASTER_USERNAME.to_string()),
            ("DBInstanceIdentifier", &instance.to_string()),
        ] {
            assert_eq!(
                &text(&modified, member),
                expected,
                "case {case:?}: {member} must not change, got {modified}"
            );
        }

        // The change is the tenant database's own from here on: the next caller finds it
        // under the name this call gave it, with nothing left pending, and it is the SAME
        // tenant database.
        let applied = the_tenant(
            case,
            &expect_ok(case, read_tenant(&router, instance, answers_to).await),
        );
        assert_eq!(
            pending_values(case, &applied),
            (None, None),
            "case {case:?}: a report of the tenant database itself carries no pending values: \
             {applied}"
        );
        assert_eq!(
            text(&applied, "TenantDatabaseResourceId"),
            resource_id,
            "case {case:?}: the applied change must leave the tenant database's id, got {applied}"
        );
        assert_eq!(
            text(&applied, "TenantDBName"),
            answers_to,
            "case {case:?}: expected the name the modification gave it, got {applied}"
        );
        if let Some(new_name) = renames {
            // The name it was renamed FROM resolves nothing any more: one tenant database
            // moved, rather than a second one appearing.
            let (status, body) = read_tenant(&router, instance, created).await;
            assert_eq!(
                (status, subtrees(&body, "TenantDatabase").len()),
                (200, 0),
                "case {case:?}: expected no tenant database left under {created:?} after the \
                 rename to {new_name:?}, got {body}"
            );
        }
    }
}

#[tokio::test]
async fn the_managed_master_user_password_follows_the_tenant_database() {
    let case = "a tenant database whose password the caller supplied";
    let router = emulator();
    let instance = "emu-cdb-secret";
    create_available_instance(&router, instance, CDB_ENGINE).await;
    let created = create_available_tenant(
        &router,
        case,
        instance,
        "tenantsc",
        &[("MasterUserPassword", SUPPLIED_PASSWORD.to_string())],
    )
    .await;
    assert_eq!(
        master_user_secret(case, &created),
        None,
        "case {case:?}: a tenant database whose password the caller supplied manages no \
         secret: {created}"
    );

    // Turning the management ON mints the secret and the tenant database reports it: a real
    // secret of this emulator's Secrets Manager, holding this tenant database's master user.
    let turned_on = the_tenant(
        case,
        &expect_ok(
            case,
            call(
                &router,
                "ModifyTenantDatabase",
                &params![
                    "DBInstanceIdentifier" => instance,
                    "TenantDBName" => "tenantsc",
                    "ManageMasterUserPassword" => "true",
                ],
            )
            .await,
        ),
    );
    let secret = master_user_secret(case, &turned_on).unwrap_or_else(|| {
        panic!("case {case:?}: expected the tenant database to report MasterUserSecret: {turned_on}")
    });
    let secret_arn = text(&secret, "SecretArn");
    let held = held_credentials(case, &router, &secret_arn).await;
    assert_eq!(
        held.username.as_deref(),
        Some(TENANT_MASTER_USERNAME),
        "case {case:?}: expected the secret to hold the tenant database's master user, got {:?}",
        held.username
    );
    assert_eq!(
        st::check_master_user_password(CDB_ENGINE, &held.password),
        Ok(()),
        "case {case:?}: the generated password must satisfy MasterUserPassword for an Oracle \
         engine, and it has {} characters",
        held.password.chars().count()
    );
    // A modification of the managed secret is not a staged change: the secret exists from
    // the moment the call returns, so nothing about the tenant database is pending.
    assert_eq!(
        pending_values(case, &turned_on),
        (None, None),
        "case {case:?}: turning the management on stages no change of the tenant database's \
         own: {turned_on}"
    );

    // Rotating stakes another password in that same secret.
    let rotated = the_tenant(
        case,
        &expect_ok(
            case,
            call(
                &router,
                "ModifyTenantDatabase",
                &params![
                    "DBInstanceIdentifier" => instance,
                    "TenantDBName" => "tenantsc",
                    "RotateMasterUserPassword" => "true",
                ],
            )
            .await,
        ),
    );
    let rotated_secret = master_user_secret(case, &rotated).unwrap_or_else(|| {
        panic!("case {case:?}: a rotated tenant database still reports its secret: {rotated}")
    });
    assert_eq!(
        text(&rotated_secret, "SecretArn"),
        secret_arn,
        "case {case:?}: a rotation stakes a new password in the SAME secret: {rotated}"
    );
    let after_rotation = held_credentials(case, &router, &secret_arn).await;
    assert_ne!(
        after_rotation.password, held.password,
        "case {case:?}: expected the rotation to stake another password in the secret"
    );
    assert_eq!(
        st::check_master_user_password(CDB_ENGINE, &after_rotation.password),
        Ok(()),
        "case {case:?}: the rotated password must satisfy MasterUserPassword too, and it has \
         {} characters",
        after_rotation.password.chars().count()
    );
    // The secret reaches `active` on its own, and the tenant database reports that status
    // from the secret rather than from the call that asked for the rotation.
    poll_until(
        case,
        "the rotated secret reaching active",
        || read_tenant(&router, instance, "tenantsc"),
        |status, body| {
            status == 200
                && subtrees(body, "MasterUserSecret")
                    .first()
                    .is_some_and(|secret| text(secret, "SecretStatus") == "active")
        },
    )
    .await;

    // Turning the management OFF deletes the secret, so the credentials it held can no
    // longer be read, and the tenant database reports no `MasterUserSecret` at all.
    let turned_off = the_tenant(
        case,
        &expect_ok(
            case,
            call(
                &router,
                "ModifyTenantDatabase",
                &params![
                    "DBInstanceIdentifier" => instance,
                    "TenantDBName" => "tenantsc",
                    "ManageMasterUserPassword" => "false",
                    "MasterUserPassword" => SUPPLIED_PASSWORD,
                ],
            )
            .await,
        ),
    );
    assert_eq!(
        master_user_secret(case, &turned_off),
        None,
        "case {case:?}: a tenant database that no longer manages its password reports no \
         secret: {turned_off}"
    );
    let read_back = the_tenant(
        case,
        &expect_ok(case, read_tenant(&router, instance, "tenantsc").await),
    );
    assert_eq!(
        master_user_secret(case, &read_back),
        None,
        "case {case:?}: and a later report says the same: {read_back}"
    );
}

#[tokio::test]
async fn a_renamed_tenant_database_is_deleted_under_the_name_it_was_given() {
    // The modification is applied by the call that made it, so the tenant database the next
    // request addresses is the renamed one: a delete naming the name it was created with
    // resolves nothing, and the one naming the new name takes the tenant database away.
    let case = "a renamed tenant database, deleted";
    let router = emulator();
    let instance = "emu-cdb-renamed-delete";
    create_available_instance(&router, instance, CDB_ENGINE).await;
    create_available_tenant(
        &router,
        case,
        instance,
        "tenantp",
        &[("MasterUserPassword", SUPPLIED_PASSWORD.to_string())],
    )
    .await;
    expect_ok(
        case,
        call(
            &router,
            "ModifyTenantDatabase",
            &params![
                "DBInstanceIdentifier" => instance,
                "TenantDBName" => "tenantp",
                "NewTenantDBName" => "renamedp",
            ],
        )
        .await,
    );
    expect_error(
        case,
        call(
            &router,
            "DeleteTenantDatabase",
            &params![
                "DBInstanceIdentifier" => instance,
                "TenantDBName" => "tenantp",
                "SkipFinalSnapshot" => "true",
            ],
        )
        .await,
        404,
        "TenantDatabaseNotFound",
    );
    expect_ok(
        case,
        call(
            &router,
            "DeleteTenantDatabase",
            &params![
                "DBInstanceIdentifier" => instance,
                "TenantDBName" => "renamedp",
                "SkipFinalSnapshot" => "true",
            ],
        )
        .await,
    );
    let read = params!["DBInstanceIdentifier" => instance];
    let (left, _) = poll_until(
        case,
        "the deleted tenant database leaving the DB instance",
        || call(&router, "DescribeTenantDatabases", &read),
        |status, body| status == 200 && subtrees(body, "TenantDatabase").is_empty(),
    )
    .await;
    assert!(
        subtrees(&left, "TenantDatabase").is_empty(),
        "case {case:?}: neither name may be left behind: {left}"
    );
}

/// One row of the refusal table: what the request names, and the refusal the constraint it
/// breaks publishes.
struct RefusalCase {
    case: &'static str,
    parameters: Vec<(&'static str, String)>,
    status: u16,
    code: &'static str,
    message: String,
}

#[tokio::test]
async fn every_refusal_is_one_a_published_constraint_states() {
    let router = emulator();
    let cdb = "emu-cdb-refusals";
    let single_tenant = "emu-single-tenant";
    create_available_instance(&router, cdb, CDB_ENGINE).await;
    create_available_instance(&router, single_tenant, "postgres").await;
    create_available_tenant(
        &router,
        "refusals",
        cdb,
        "tenantr",
        &[("MasterUserPassword", SUPPLIED_PASSWORD.to_string())],
    )
    .await;
    create_available_tenant(
        &router,
        "refusals",
        cdb,
        "tenantm",
        &[("ManageMasterUserPassword", "true".to_string())],
    )
    .await;

    // Each row: what the request names, and the refusal the constraint it breaks publishes.
    let rows: Vec<RefusalCase> = vec![
        RefusalCase {
            case: "a DB instance that does not exist",
            parameters: owned_params![
                "DBInstanceIdentifier" => "emu-missing-cdb",
                "TenantDBName" => "tenantr",
            ],
            status: 404,
            code: "DBInstanceNotFound",
            message: "DBInstance not found: emu-missing-cdb".to_string(),
        },
        RefusalCase {
            case: "a DB instance that holds no tenant databases at all",
            parameters: owned_params![
                "DBInstanceIdentifier" => single_tenant,
                "TenantDBName" => "tenantr",
            ],
            status: 400,
            code: "InvalidParameterCombination",
            message: format!(
                "The ModifyTenantDatabase operation isn't supported for DB instance \
                 {single_tenant}."
            ),
        },
        RefusalCase {
            case: "a tenant database that does not exist in that instance",
            parameters: owned_params![
                "DBInstanceIdentifier" => cdb,
                "TenantDBName" => "tengone",
            ],
            status: 404,
            code: "TenantDatabaseNotFound",
            message: format!("TenantDatabase tengone not found in DB instance {cdb}."),
        },
        RefusalCase {
            case: "a new name one character past the published bound",
            parameters: owned_params![
                "DBInstanceIdentifier" => cdb,
                "TenantDBName" => "tenantr",
                "NewTenantDBName" => "ninechars",
            ],
            status: 400,
            code: "InvalidParameterValue",
            message: st::check_new_tenant_db_name("ninechars")
                .expect_err("a nine-character name is past the published bound"),
        },
        RefusalCase {
            case: "a new name the member documents as reserved",
            parameters: owned_params![
                "DBInstanceIdentifier" => cdb,
                "TenantDBName" => "tenantr",
                "NewTenantDBName" => "null",
            ],
            status: 400,
            code: "InvalidParameterValue",
            message: st::check_new_tenant_db_name("null")
                .expect_err("null is the reserved word named"),
        },
        RefusalCase {
            case: "a new name another tenant database of the instance already holds",
            parameters: owned_params![
                "DBInstanceIdentifier" => cdb,
                "TenantDBName" => "tenantr",
                "NewTenantDBName" => "tenantm",
            ],
            status: 400,
            code: "TenantDatabaseAlreadyExists",
            message: format!("Tenant database tenantm already exists in DB instance {cdb}"),
        },
        RefusalCase {
            case: "a password past the bound the Oracle engine publishes",
            parameters: owned_params![
                "DBInstanceIdentifier" => cdb,
                "TenantDBName" => "tenantr",
                // One character past the 30 `MasterUserPassword` publishes for Oracle.
                "MasterUserPassword" => "abcdefghijklmnopqrstuvwxyz12345",
            ],
            status: 400,
            code: "InvalidParameterValue",
            message: st::check_master_user_password(CDB_ENGINE, "abcdefghijklmnopqrstuvwxyz12345")
                .expect_err("31 characters is past the Oracle bound"),
        },
        RefusalCase {
            case: "the password and its management at once",
            parameters: owned_params![
                "DBInstanceIdentifier" => cdb,
                "TenantDBName" => "tenantr",
                "ManageMasterUserPassword" => "true",
                "MasterUserPassword" => SUPPLIED_PASSWORD,
            ],
            status: 400,
            code: "InvalidParameterValue",
            message: st::manage_master_user_password_conflict_message("MasterUserPassword"),
        },
        RefusalCase {
            case: "a rotation of a password RDS does not manage",
            parameters: owned_params![
                "DBInstanceIdentifier" => cdb,
                "TenantDBName" => "tenantr",
                "RotateMasterUserPassword" => "true",
            ],
            status: 400,
            code: "InvalidParameterCombination",
            message: st::rotate_without_managed_secret_message("tenant database"),
        },
        RefusalCase {
            case: "a secret key named without the management that would use it",
            parameters: owned_params![
                "DBInstanceIdentifier" => cdb,
                "TenantDBName" => "tenantr",
                "MasterUserSecretKmsKeyId" => "alias/aws/secretsmanager",
            ],
            status: 400,
            code: "InvalidParameterCombination",
            message: st::master_user_secret_kms_key_without_management_message(
                "ManageMasterUserPassword",
            ),
        },
        RefusalCase {
            case: "management turned off with no password to use instead",
            parameters: owned_params![
                "DBInstanceIdentifier" => cdb,
                "TenantDBName" => "tenantm",
                "ManageMasterUserPassword" => "false",
            ],
            status: 400,
            code: "InvalidParameterCombination",
            message: st::master_user_password_required_to_stop_management_message(
                "MasterUserPassword",
            ),
        },
    ];

    for RefusalCase {
        case,
        parameters,
        status,
        code,
        message,
    } in rows
    {
        let refused = expect_error(
            case,
            call(&router, "ModifyTenantDatabase", &parameters).await,
            status,
            code,
        );
        assert_eq!(
            text(&refused, "Message"),
            message,
            "case {case:?}: expected the published sentence, got {refused}"
        );
        assert_eq!(
            subtrees(&refused, "TenantDatabase").len(),
            0,
            "case {case:?}: a refused modification answers no tenant database: {refused}"
        );
    }

    // And nothing was modified by any of them: the tenant database the rows addressed still
    // carries its own name with nothing pending.
    let untouched = the_tenant(
        "refusals",
        &expect_ok("refusals", read_tenant(&router, cdb, "tenantr").await),
    );
    assert_eq!(
        (
            text(&untouched, "TenantDBName"),
            pending_values("refusals", &untouched)
        ),
        ("tenantr".to_string(), (None, None)),
        "a refused modification changes nothing: {untouched}"
    );
}

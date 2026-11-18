// SPDX-License-Identifier: CC0-1.0

//! The secret a DB INSTANCE's managed master user password lives in, from the two sides a
//! client reaches it from: Secrets Manager's own listing of the account's secrets, and the
//! KMS key the secret is encrypted under.
//!
//! THE RULE under test, asserted over a table of inputs rather than the one recorded call:
//! when a create turns `ManageMasterUserPassword` on, the secret the resource reports as
//! `MasterUserSecret.SecretArn` is a REAL secret of Secrets Manager — so it is discoverable
//! through Secrets Manager's own listing of the account's secrets, and its value is
//! encrypted under the KMS key the request named, whichever way it named it, or under the
//! service's own default key when the request named none.
//!
//! What follows from that one rule, and is asserted on every row:
//!
//! * the create answers with a `MasterUserSecret` whose `SecretArn` is not empty — the ARN
//!   is the only way a client can find the generated credentials;
//! * `ListSecrets` reports exactly one secret at that ARN, owned by RDS and named after the
//!   instance's own immutable resource id, so a client scanning the account's secrets finds
//!   it;
//! * the key: the `KmsKeyId` the instance reports and the `KmsKeyId` Secrets Manager holds
//!   for that secret resolve, through KMS, to the SAME key — the one the request named, else
//!   the Amazon Web Services managed key for Secrets Manager (which a secret records by
//!   naming no key of its own);
//! * the credentials in it are the master user's, readable through that key
//!   ([`a_managed_secret_is_unreadable_while_the_key_it_names_is_disabled`] is the other
//!   side of that: a secret under a key that cannot be used cannot be read, which is what
//!   makes the key a real one rather than a reported identifier).
//!
//! The rows differ along what the rule generalises over: HOW the request identifies the key
//! for the secret (the key's id, the key's ARN, an alias of the key, and no key at all) and
//! the engine the instance runs.
//!
//! Secrets Manager and KMS are mounted beside the RDS service over one engine
//! ([`crate::managed_master_user_password::emulator`]), because the secret read back here is
//! a real secret of this emulator and the key is a real key of its KMS.

use aws_sdk_secretsmanager::operation::get_secret_value::GetSecretValueError;
use aws_sdk_secretsmanager::types::SecretListEntry;
use service_com_amazonaws_rds_amazonrdsv19::state as st;
use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, expect_ok, text};
use crate::managed_master_user_password::{
    emulator, held_credentials, kms, master_user_secret, secrets_manager, the_resource,
};

/// The Amazon Web Services managed key a secret that names no key of its own is encrypted
/// under: "If you don't specify MasterUserSecretKmsKeyId, then the aws/secretsmanager KMS
/// key is used to encrypt the secret" (`MasterUserSecretKmsKeyId`, this service's Smithy
/// model), named by the alias AWS publishes for it.
const DEFAULT_SECRET_KEY_ALIAS: &str = "alias/aws/secretsmanager";

/// How a row's request identifies the key for the secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyForm {
    /// `MasterUserSecretKmsKeyId` carries the key's id.
    Id,
    /// It carries the key's ARN.
    Arn,
    /// It carries an alias of the key.
    Alias,
    /// The request names no key at all.
    Unnamed,
}

/// One row: the instance to create, and how its request names the key for the secret.
struct Row {
    case: &'static str,
    engine: &'static str,
    instance_class: &'static str,
    master_username: &'static str,
    key_form: KeyForm,
}

const ROWS: &[Row] = &[
    Row {
        case: "an RDS for PostgreSQL DB instance naming the key by its id",
        engine: "postgres",
        instance_class: "db.t4g.micro",
        master_username: "pgadmin",
        key_form: KeyForm::Id,
    },
    Row {
        case: "an RDS for MySQL DB instance naming the key by its ARN",
        engine: "mysql",
        instance_class: "db.t3.micro",
        master_username: "admin",
        key_form: KeyForm::Arn,
    },
    Row {
        case: "an RDS for MariaDB DB instance naming an alias of the key",
        engine: "mariadb",
        instance_class: "db.t3.small",
        master_username: "mariaadmin",
        key_form: KeyForm::Alias,
    },
    Row {
        case: "an RDS for PostgreSQL DB instance naming no key at all",
        engine: "postgres",
        instance_class: "db.t4g.micro",
        master_username: "dbadmin",
        key_form: KeyForm::Unnamed,
    },
];

/// The ARN of the key `named` resolves to, through KMS's own `DescribeKey` — which is what
/// makes two different identifiers of one key (an id, an ARN, an alias) comparable.
async fn resolved_key_arn(case: &str, router: &MultiServiceRouter, named: &str) -> String {
    let described = kms(router)
        .describe_key()
        .key_id(named)
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: KMS resolves no key at {named:?}: {error}")
        });
    described
        .key_metadata()
        .and_then(|metadata| metadata.arn())
        .unwrap_or_else(|| panic!("case {case:?}: the key at {named:?} reports no ARN"))
        .to_string()
}

/// The ONE entry `ListSecrets` reports for `arn`, walking every page of the listing.
async fn listed_secret(case: &str, router: &MultiServiceRouter, arn: &str) -> SecretListEntry {
    let mut found: Vec<SecretListEntry> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = secrets_manager(router)
            .list_secrets()
            .set_next_token(token)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: Secrets Manager does not list its secrets: {error}")
            });
        found.extend(
            page.secret_list()
                .iter()
                .filter(|entry| entry.arn() == Some(arn))
                .cloned(),
        );
        token = page.next_token().map(str::to_string);
        if token.is_none() {
            break;
        }
    }
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected ListSecrets to report exactly one secret at {arn}, got {}",
        found.len()
    );
    found.pop().expect("checked just above")
}

/// The `MasterUserSecretKmsKeyId` a row's request carries, in the form that row names the
/// key in: one key, three identifiers of it, and the row that names none at all.
fn named_key(row: &Row, key_id: &str, key_arn: &str, key_alias: &str) -> Option<String> {
    match row.key_form {
        KeyForm::Id => Some(key_id.to_string()),
        KeyForm::Arn => Some(key_arn.to_string()),
        KeyForm::Alias => Some(key_alias.to_string()),
        KeyForm::Unnamed => None,
    }
}

/// `CreateDBInstance` for a row, with the management of the master user password turned on.
async fn create_managed_instance(
    router: &MultiServiceRouter,
    row: &Row,
    identifier: &str,
    named_key: Option<&str>,
) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("DBInstanceIdentifier", identifier.to_string()),
        ("Engine", row.engine.to_string()),
        ("DBInstanceClass", row.instance_class.to_string()),
        ("AllocatedStorage", "20".to_string()),
        ("MasterUsername", row.master_username.to_string()),
        ("ManageMasterUserPassword", "true".to_string()),
    ];
    if let Some(key) = named_key {
        params.push(("MasterUserSecretKmsKeyId", key.to_string()));
    }
    let created = expect_ok(row.case, call(router, "CreateDBInstance", &params).await);
    the_resource(row.case, &created, "DBInstance")
}

#[tokio::test]
async fn a_managed_master_user_secret_is_listed_and_keyed_as_the_request_asked() {
    let router = emulator();
    // A key of the account's own, minted through KMS itself, plus an alias naming it — the
    // three identifiers of one key the rows send.
    let minted = kms(&router)
        .create_key()
        .description("the key a managed master user secret is encrypted under")
        .send()
        .await
        .expect("KMS mints a customer managed key");
    let metadata = minted
        .key_metadata()
        .expect("a minted key reports its metadata");
    let key_id = metadata.key_id().to_string();
    let key_alias = "alias/emu-managed-master-user-secret";
    kms(&router)
        .create_alias()
        .alias_name(key_alias)
        .target_key_id(&key_id)
        .send()
        .await
        .expect("KMS names an alias for the key");
    let key_arn = resolved_key_arn("the customer managed key", &router, &key_id).await;
    let customer_key = key_arn.clone();
    let default_key =
        resolved_key_arn("the default secret key", &router, DEFAULT_SECRET_KEY_ALIAS).await;
    assert_ne!(
        customer_key, default_key,
        "the table needs the customer managed key and the default key to be different keys"
    );

    for (index, row) in ROWS.iter().enumerate() {
        let identifier = format!("emu-secret-key-{index}");
        let asked_key = named_key(row, &key_id, &key_arn, key_alias);
        let envelope =
            create_managed_instance(&router, row, &identifier, asked_key.as_deref()).await;

        // 1. The create answers with the secret it minted — the ARN a client has no other
        //    way of learning.
        let secret = master_user_secret(row.case, &envelope).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the instance to report MasterUserSecret, got {envelope}",
                row.case
            )
        });
        let secret_arn = text(&secret, "SecretArn");
        assert!(
            !secret_arn.is_empty(),
            "case {:?}: expected a SecretArn on the reported secret, got {secret}",
            row.case
        );

        // 2. The key the instance reports is the one the request named, whichever
        //    identifier it used, else the service's default key.
        let expected_key = match row.key_form {
            KeyForm::Unnamed => &default_key,
            _ => &customer_key,
        };
        // The member is the key's own identity, so it is compared as the key KMS resolves it
        // to rather than as one permitted spelling of it.
        let reported_key = text(&secret, "KmsKeyId");
        let reported_key_arn = resolved_key_arn(row.case, &router, &reported_key).await;
        assert_eq!(
            &reported_key_arn, expected_key,
            "case {:?}: expected the secret to report the key it is encrypted under, got \
             {reported_key:?}",
            row.case
        );

        // 3. Secrets Manager really holds that secret, and LISTS it for the account: a
        //    client scanning the account's secrets finds the managed one, owned by RDS and
        //    named in the reserved form AWS gives a secret of its own.
        let entry = listed_secret(row.case, &router, &secret_arn).await;
        assert_eq!(
            entry.owning_service(),
            Some("rds"),
            "case {:?}: expected the listed secret to be owned by RDS, got {entry:?}",
            row.case
        );
        let expected_prefix = format!("{}db-", st::MANAGED_SECRET_NAME_PREFIX);
        assert!(
            entry
                .name()
                .is_some_and(|name| name.starts_with(&expected_prefix)),
            "case {:?}: expected the listed secret to be named {expected_prefix:?}<uuid>, got \
             {:?}",
            row.case,
            entry.name()
        );
        assert!(
            secret_arn.contains(entry.name().unwrap_or_default()),
            "case {:?}: expected the secret's ARN to carry its name, got {secret_arn:?}",
            row.case
        );

        // 4. And the key Secrets Manager holds for it is that same key: a secret that names
        //    no key of its own is on the Amazon Web Services managed key for the service.
        let held_key = resolved_key_arn(
            row.case,
            &router,
            entry.kms_key_id().unwrap_or(DEFAULT_SECRET_KEY_ALIAS),
        )
        .await;
        assert_eq!(
            &held_key, expected_key,
            "case {:?}: expected Secrets Manager to hold the secret under the same key, got \
             {held_key:?} from {:?}",
            row.case,
            entry.kms_key_id()
        );

        // 5. The credentials in it are the master user's, readable through that key.
        let held = held_credentials(row.case, &router, &secret_arn).await;
        assert_eq!(
            held.username.as_deref(),
            Some(row.master_username),
            "case {:?}: expected the secret to hold the master user the request named, got \
             {:?}",
            row.case,
            held.username
        );
        assert_eq!(
            st::check_master_user_password(row.engine, &held.password),
            Ok(()),
            "case {:?}: the generated password must satisfy MasterUserPassword for engine {}",
            row.case,
            row.engine
        );
    }
}

/// The other side of the key being a REAL key: the value of a secret encrypted under a
/// customer managed key can only be read while that key can be used, so disabling the key
/// takes the credentials out of reach and re-enabling it brings the same credentials back —
/// while a secret on the service's own default key, which names no key a caller can act on,
/// is untouched by either.
#[tokio::test]
async fn a_managed_secret_is_unreadable_while_the_key_it_names_is_disabled() {
    let router = emulator();
    let minted = kms(&router)
        .create_key()
        .description("the key one of the two managed secrets is encrypted under")
        .send()
        .await
        .expect("KMS mints a customer managed key");
    let key_id = minted
        .key_metadata()
        .expect("a minted key reports its metadata")
        .key_id()
        .to_string();

    let key_arn = minted
        .key_metadata()
        .expect("a minted key reports its metadata")
        .arn()
        .expect("a minted key reports its ARN")
        .to_string();
    let key_alias = "alias/emu-managed-secret-under-a-disabled-key";
    kms(&router)
        .create_alias()
        .alias_name(key_alias)
        .target_key_id(&key_id)
        .send()
        .await
        .expect("KMS names an alias for the key");

    let under_key = Row {
        case: "an instance whose secret names a key of its own",
        engine: "postgres",
        instance_class: "db.t4g.micro",
        master_username: "pgadmin",
        key_form: KeyForm::Id,
    };
    let on_default = Row {
        case: "an instance whose secret names no key",
        engine: "mysql",
        instance_class: "db.t3.micro",
        master_username: "admin",
        key_form: KeyForm::Unnamed,
    };
    // Each row's request carries the key in the form its own `key_form` names, as the table
    // above does — here, the key's id for one row and no key at all for the other.
    let keyed_envelope = create_managed_instance(
        &router,
        &under_key,
        "emu-key-disabled",
        named_key(&under_key, &key_id, &key_arn, key_alias).as_deref(),
    )
    .await;
    let default_envelope = create_managed_instance(
        &router,
        &on_default,
        "emu-key-default",
        named_key(&on_default, &key_id, &key_arn, key_alias).as_deref(),
    )
    .await;
    let keyed_arn = text(
        &master_user_secret(under_key.case, &keyed_envelope).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected a MasterUserSecret: {keyed_envelope}",
                under_key.case
            )
        }),
        "SecretArn",
    );
    let default_arn = text(
        &master_user_secret(on_default.case, &default_envelope).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected a MasterUserSecret: {default_envelope}",
                on_default.case
            )
        }),
        "SecretArn",
    );

    // Both read while the key is enabled.
    let keyed_before = held_credentials(under_key.case, &router, &keyed_arn).await;
    let default_before = held_credentials(on_default.case, &router, &default_arn).await;

    kms(&router)
        .disable_key()
        .key_id(&key_id)
        .send()
        .await
        .expect("KMS disables the key");

    let refused = secrets_manager(&router)
        .get_secret_value()
        .secret_id(&keyed_arn)
        .send()
        .await
        .err()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the read of a secret under a disabled key to be refused",
                under_key.case
            )
        });
    let service_error = refused.into_service_error();
    assert!(
        matches!(service_error, GetSecretValueError::DecryptionFailure(_)),
        "case {:?}: expected DecryptionFailure while the key is disabled, got \
         {service_error:?}",
        under_key.case
    );

    // The secret on the service's own default key names no key a caller can disable, so it
    // still reads, and holds exactly what it held.
    let default_during = held_credentials(on_default.case, &router, &default_arn).await;
    assert_eq!(
        default_during.password, default_before.password,
        "case {:?}: a secret on the default key is untouched by another key being disabled",
        on_default.case
    );

    kms(&router)
        .enable_key()
        .key_id(&key_id)
        .send()
        .await
        .expect("KMS enables the key again");
    let keyed_after = held_credentials(under_key.case, &router, &keyed_arn).await;
    assert_eq!(
        keyed_after.password, keyed_before.password,
        "case {:?}: the same credentials come back once the key can be used again",
        under_key.case
    );
    assert_eq!(
        keyed_after.username, keyed_before.username,
        "case {:?}: the same credentials come back once the key can be used again",
        under_key.case
    );
}

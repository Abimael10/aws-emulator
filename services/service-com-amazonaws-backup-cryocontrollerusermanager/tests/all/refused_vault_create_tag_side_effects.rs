// SPDX-License-Identifier: CC0-1.0

//! A backup-vault create that is REFUSED applies none of its own parameters,
//! and the tag read is what proves it.
//!
//! The rules under test:
//!
//! * a create refused because the account already holds that vault name in the
//!   Region leaves the stored vault exactly as its own create wrote it — tag
//!   set, creation instant and ARN — whatever the refused request asked for, and
//!   the refusal carries the condition's own code and sentence;
//! * `ListTags` on a vault ARN answers 200 with that stored tag set, which for a
//!   vault nothing has tagged since is the set its create stored;
//! * a vault name belongs to one Region, so a refusal in one Region reaches
//!   neither the tag set of the same name's vault in another Region nor its own.
//!
//! The rows span what the rule generalises over: the shape of the create-time
//! tag set (none, one pair, several, punctuated values), the FORM of the refused
//! repeat (different tags, no tags, no idempotency token, another token, a named
//! KMS key, and the other vault type's create of the same name), and the Region
//! each vault belongs to. The recorded scenario — one vault tagged at create,
//! one rejected duplicate, one tag read — is one row of the first table.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::logically_air_gapped_vault::create_air_gapped;
use crate::vault_and_backup_job::{
    call, call_in_region, create_vault, create_vault_in, error_code, reported_tags, router,
    string_at, vault_arn, ACCOUNT, REGION,
};

/// The sentence this service states for "the account already holds a vault of
/// that name in this Region" — fixed text, no state about the stored vault.
const ALREADY_EXISTS: &str = "Backup vault with the same name already exists";

/// The idempotency token the original create of every row carries, so a repeat
/// that replays it is a retry and every other repeat is a collision.
const ORIGINAL_TOKEN: &str = "token-1";

/// `GET /tags/{ResourceArn}`, as the model binds the tag read, through a client
/// bound to `region`.
async fn list_tags_in(router: &MultiServiceRouter, region: &str, resource_arn: &str) -> (u16, Value) {
    call_in_region(router, region, "GET", &format!("/tags/{resource_arn}"), None).await
}

/// The create-time tag sets the rows span.
fn tag_sets() -> Vec<(&'static str, Value)> {
    vec![
        ("a vault the create left untagged", json!({})),
        (
            "the one pair of the recorded scenario",
            json!({"sim2real": "refarch-bkp"}),
        ),
        (
            "several pairs",
            json!({"team": "backups", "owner": "ops", "stage": "prod"}),
        ),
        (
            "a key and a value carrying punctuation",
            json!({"cost:center": "eng/platform 42.0"}),
        ),
    ]
}

/// A repeat create of a name the Region already holds: the ordinary create, or
/// the air-gapped one, which addresses the same vault namespace.
enum Repeat {
    Ordinary(String),
    AirGapped(String),
}

impl Repeat {
    async fn attempt(&self, router: &MultiServiceRouter, name: &str) -> (u16, Value) {
        match self {
            Self::Ordinary(body) => create_vault(router, name, body).await,
            Self::AirGapped(body) => create_air_gapped(router, name, body).await,
        }
    }
}

/// The FORMS of refused repeat the rows span. Each asks for tags of its own, a
/// key of its own, or nothing at all; none of them may reach the stored vault.
fn refused_repeats() -> Vec<(&'static str, Repeat)> {
    let intruding = json!({"sim2real": "overwritten", "intruder": "yes"});
    vec![
        (
            "a duplicate asking for tags of its own",
            Repeat::Ordinary(
                json!({"BackupVaultTags": intruding, "CreatorRequestId": ORIGINAL_TOKEN})
                    .to_string(),
            ),
        ),
        (
            "a duplicate asking for no tags at all",
            Repeat::Ordinary("{}".to_string()),
        ),
        (
            "a duplicate carrying no idempotency token",
            Repeat::Ordinary(json!({"BackupVaultTags": intruding}).to_string()),
        ),
        (
            "a duplicate carrying another idempotency token",
            Repeat::Ordinary(
                json!({"BackupVaultTags": intruding, "CreatorRequestId": "token-2"}).to_string(),
            ),
        ),
        (
            "a duplicate naming a KMS key",
            Repeat::Ordinary(
                json!({
                    "BackupVaultTags": intruding,
                    "CreatorRequestId": ORIGINAL_TOKEN,
                    "EncryptionKeyArn":
                        format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/11111111-2222-3333-4444-555555555555"),
                })
                .to_string(),
            ),
        ),
        (
            "an air-gapped create of the same name",
            Repeat::AirGapped(
                json!({
                    "MinRetentionDays": 7,
                    "MaxRetentionDays": 365,
                    "BackupVaultTags": intruding,
                })
                .to_string(),
            ),
        ),
    ]
}

/// The body of the original create of a row: its tags plus the idempotency
/// token every row's original carries.
fn original_body(tags: &Value) -> String {
    json!({"BackupVaultTags": tags, "CreatorRequestId": ORIGINAL_TOKEN}).to_string()
}

/// A refused duplicate create applies NONE of its own tags: the tag read still
/// reports the set the original create stored, and the vault's ARN and creation
/// instant are unchanged too.
#[tokio::test]
async fn a_refused_duplicate_create_applies_none_of_its_own_tags() {
    let name = "refarch-bkp-vault";
    for (tag_case, tags) in tag_sets() {
        for (repeat_case, repeat) in refused_repeats() {
            let case = format!("{tag_case}, then {repeat_case}");
            let router = router();

            let (status, created) = create_vault(&router, name, &original_body(&tags)).await;
            assert_eq!(
                (status, error_code(&created).as_str()),
                (200, "<none>"),
                "case {case:?}: expected CreateBackupVault 200, got {status} {created:?}"
            );
            let arn = string_at(&created, "BackupVaultArn");
            assert_eq!(
                arn,
                vault_arn(REGION, name),
                "case {case:?}: expected the vault ARN of this account and Region, got \
                 {created:?}"
            );
            let creation_date = created.get("CreationDate").cloned();
            assert!(
                creation_date.is_some(),
                "case {case:?}: the create records a creation instant, got {created:?}"
            );

            // The tag read answers from the stored vault BEFORE the refusal.
            let (status, read) = list_tags_in(&router, REGION, &arn).await;
            assert_eq!(
                (status, error_code(&read).as_str(), reported_tags(&read)),
                (200, "<none>", tags.clone()),
                "case {case:?}: expected ListTags 200 with the create's tags, got {status} \
                 {read:?}"
            );

            let (status, refused) = repeat.attempt(&router, name).await;
            assert_eq!(
                (
                    status,
                    error_code(&refused).as_str(),
                    string_at(&refused, "Message").as_str()
                ),
                (400, "AlreadyExistsException", ALREADY_EXISTS),
                "case {case:?}: expected the duplicate to be refused, got {status} {refused:?}"
            );

            // And the refusal reached nothing: the tag set, the ARN and the
            // creation instant are still the original create's.
            let (status, read) = list_tags_in(&router, REGION, &arn).await;
            assert_eq!(
                (status, error_code(&read).as_str(), reported_tags(&read)),
                (200, "<none>", tags.clone()),
                "case {case:?}: the refused create must not re-tag the surviving vault, got \
                 {status} {read:?}"
            );
            let (status, described) =
                call(&router, "GET", &format!("/backup-vaults/{name}"), None).await;
            assert_eq!(
                (
                    status,
                    string_at(&described, "BackupVaultArn"),
                    described.get("CreationDate").cloned(),
                    described.get("VaultType").and_then(Value::as_str),
                ),
                (200, arn.clone(), creation_date, Some("BACKUP_VAULT")),
                "case {case:?}: the surviving vault must be the one its own create wrote, got \
                 {status} {described:?}"
            );
        }
    }
}

/// A repeat that REPLAYS the original request (same token, same parameters) is
/// answered from the stored vault, and leaves its tag set alone as well: it
/// stores nothing a second time.
#[tokio::test]
async fn a_replayed_create_reports_the_stored_vault_and_its_stored_tags() {
    let name = "replayed-bkp-vault";
    for (tag_case, tags) in tag_sets() {
        let router = router();
        let body = original_body(&tags);
        let (status, created) = create_vault(&router, name, &body).await;
        assert_eq!(
            status, 200,
            "case {tag_case:?}: expected CreateBackupVault 200, got {status} {created:?}"
        );

        let (status, replayed) = create_vault(&router, name, &body).await;
        assert_eq!(
            (
                status,
                string_at(&replayed, "BackupVaultArn"),
                replayed.get("CreationDate").cloned(),
            ),
            (
                200,
                string_at(&created, "BackupVaultArn"),
                created.get("CreationDate").cloned(),
            ),
            "case {tag_case:?}: a replay reports the stored vault, got {status} {replayed:?}"
        );

        let (status, read) = list_tags_in(&router, REGION, &string_at(&created, "BackupVaultArn"))
            .await;
        assert_eq!(
            (status, reported_tags(&read)),
            (200, tags.clone()),
            "case {tag_case:?}: a replay leaves the stored tag set alone, got {status} {read:?}"
        );
    }
}

/// A vault name belongs to ONE Region, so each Region's vault carries its own
/// create-time tags and a duplicate refused in one Region reaches neither set.
#[tokio::test]
async fn each_regions_vault_keeps_its_own_create_time_tags() {
    let name = "shared-bkp-vault";
    // Pairs of Regions the rule generalises over, including a Region of another
    // partition.
    let pairs = [
        (REGION, "us-east-1"),
        ("eu-central-1", "ap-southeast-2"),
        ("us-gov-west-1", "eu-west-1"),
    ];
    for (first, second) in pairs {
        let router = router();
        let first_tags = json!({"sim2real": "refarch-bkp", "region-role": "primary"});
        let second_tags = json!({"region-role": "secondary"});

        for (region, tags) in [(first, &first_tags), (second, &second_tags)] {
            let (status, created) =
                create_vault_in(&router, region, name, &original_body(tags)).await;
            assert_eq!(
                (status, string_at(&created, "BackupVaultArn")),
                (200, vault_arn(region, name)),
                "case {region:?}: expected the Region's own vault, got {status} {created:?}"
            );
        }

        // A duplicate in the FIRST Region is refused, and asks for tags of its
        // own on the way.
        let duplicate = json!({"BackupVaultTags": {"intruder": "yes"}}).to_string();
        let (status, refused) = create_vault_in(&router, first, name, &duplicate).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "AlreadyExistsException"),
            "case {first:?}: expected the duplicate to be refused, got {status} {refused:?}"
        );

        for (region, tags) in [(first, &first_tags), (second, &second_tags)] {
            let (status, read) = list_tags_in(&router, region, &vault_arn(region, name)).await;
            assert_eq!(
                (status, reported_tags(&read)),
                (200, tags.clone()),
                "case {region:?}: each Region's vault reports its own create-time tags, got \
                 {status} {read:?}"
            );
        }
    }
}

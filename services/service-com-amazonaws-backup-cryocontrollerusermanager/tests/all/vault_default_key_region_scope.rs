// SPDX-License-Identifier: CC0-1.0

//! Following a vault's published key into KMS by the two identifiers a client
//! has for it: the key ARN the vault reports, and the predefined alias NAME.
//!
//! The rule under test: the `EncryptionKeyArn` every read of a vault reports is
//! the identifier of a key KMS holds in that vault's account and Region — the
//! Amazon Web Services managed key of this service, which KMS keeps under the
//! predefined `alias/aws/backup` alias ("if you do not specify a key, Backup
//! creates a KMS key for you by default",
//! `smithy_query(query="doc",shape="CreateBackupVaultInput",member="EncryptionKeyArn")`;
//! an Amazon Web Services alias has "the format `aws/<service-name>`",
//! `smithy_query(query="doc",shape="ListAliases")`). So the two identifiers meet
//! at one key: `kms:ListAliases` of the published ARN reports that alias, with
//! the alias ARN of the vault's own Region and the published key as its target,
//! and `kms:DescribeKey` of the alias NAME answers the very key the vault
//! published — rather than `NotFoundException`, which KMS reserves for an
//! identifier naming no key.
//!
//! What the sibling suite [`super::vault_default_key_in_kms`] pins is that the
//! published key resolves and which kind of key it is; what this one adds is the
//! pair of identifiers agreeing — the alias NAME resolving to the very key the
//! vault published — and the alias entry's own members. (That the described and
//! the listed vault publish the same identifier is `vault_read_projection`'s
//! `described_and_listed_agree`, over every shared member, so it is not repeated
//! here.) The rows differ in the REGION the vault was created in, because a KMS
//! key and its alias are regional resources, so the alias ARN and the key behind
//! it are a Region's own. No key id is written here: every one is read off a
//! response, because the id is generated per run while the rule is not.

use serde_json::{json, Value};

use super::vault_and_backup_job::ACCOUNT;
use super::vault_default_key_in_kms::{
    kms, member, router_with_kms, vault_key, MANAGED_KEY_ALIAS,
};

/// The Regions the rule is exercised over. A Region reaches the handlers through
/// the request's SigV4 credential scope, so these are simply Regions a client
/// can sign for; they span three continents' prefixes so the Region is carried
/// through rather than matched.
const REGIONS: &[(&str, &str)] = &[
    ("a Region in the United States", "us-east-1"),
    ("a Region in Europe", "eu-west-1"),
    ("a Region in Asia Pacific", "ap-southeast-2"),
];

/// The key a vault published carries the predefined alias of its own Region, and
/// asking KMS for that alias NAME answers the same key.
#[tokio::test]
async fn the_published_key_and_the_predefined_alias_name_one_key_per_region() {
    let router = router_with_kms();

    for (case, region) in REGIONS {
        let key = vault_key(
            &router,
            region,
            "/backup-vaults",
            "default-key-reader",
            &json!({}),
        )
        .await;

        // The OTHER identifier a client holds — the alias NAME — names that same
        // key at that Region's endpoint, and KMS's own report of it is where the
        // key's id comes from, rather than this test taking the ARN apart.
        let (status, by_alias) = kms(
            &router,
            region,
            "DescribeKey",
            &json!({"KeyId": MANAGED_KEY_ALIAS}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected kms:DescribeKey({MANAGED_KEY_ALIAS}) in {region} to answer \
             200, got {status} {by_alias:?}"
        );
        assert_eq!(
            member(&by_alias["KeyMetadata"], "Arn"),
            key,
            "case {case:?}: expected {MANAGED_KEY_ALIAS} in {region} to name the key the vault \
             published, got {by_alias:?}"
        );
        let key_id = member(&by_alias["KeyMetadata"], "KeyId");

        // ...and the alias entry of that key reports the predefined name with
        // the alias ARN of this account and Region and that key as its target.
        // The entry is found BY NAME: `ListAliases` models no ordering of its
        // page, so the assertion is about which entry is there, not where.
        let (status, aliases) = kms(&router, region, "ListAliases", &json!({"KeyId": &key})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected kms:ListAliases({key}) to answer 200, got {status} \
             {aliases:?}"
        );
        let entries = aliases["Aliases"].as_array().cloned().unwrap_or_default();
        assert!(
            !entries.is_empty(),
            "case {case:?}: expected kms:ListAliases({key}) to report the key's alias, got an \
             empty list in {aliases:?}"
        );
        let entry = entries
            .iter()
            .find(|alias| member(alias, "AliasName") == MANAGED_KEY_ALIAS)
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            (
                member(&entry, "AliasName"),
                member(&entry, "AliasArn"),
                member(&entry, "TargetKeyId"),
            ),
            (
                MANAGED_KEY_ALIAS.to_string(),
                format!("arn:aws:kms:{region}:{ACCOUNT}:{MANAGED_KEY_ALIAS}"),
                key_id,
            ),
            "case {case:?}: expected the published key to carry {MANAGED_KEY_ALIAS} of {region}, \
             got {aliases:?}"
        );
    }
}

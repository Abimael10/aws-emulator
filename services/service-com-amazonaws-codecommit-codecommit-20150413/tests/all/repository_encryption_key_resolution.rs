// SPDX-License-Identifier: CC0-1.0

//! Which KEY a repository's `kmsKeyId` reports, with KMS mounted beside this
//! service on one shared engine so the answer is checked against the key KMS
//! itself holds — driven as awsJson1_1 requests through the router that serves the
//! real emulator.
//!
//! The rule under test: `RepositoryMetadata$kmsKeyId` is "The ID of the Key
//! Management Service encryption key used to encrypt and decrypt the repository"
//! (`smithy_query(query="doc",shape="RepositoryMetadata",member="kmsKeyId")`), so a
//! repository reports the KEY, resolved against KMS, whichever of the four forms
//! KMS documents for `KeyId` the request named it by — a key id, a key ARN, an
//! alias name or an alias ARN are four spellings of one key, and all four are
//! reported as that key's ARN. A request that names no key at all asks for the
//! Amazon Web Services managed key of this service, whose predefined alias
//! "exists in every account and Region" carrying no key until something asks for
//! it, when KMS associates one with it — so the create is what materialises it,
//! and every repository created without a key in that Region reports that same
//! key. A selector KMS holds no key for is held in the fullest form it names by
//! itself, which is the sibling suites' subject
//! (`crate::repository_encryption_key_projection`).
//!
//! The table spans what the rule generalises over: every selector form, keys of
//! both shapes KMS mints an id in, a key reached through its own alias, a request
//! naming no key, and selectors KMS resolves nothing for — on the create path and
//! on the rotation path alike.
//!
//! The READ path is its own subject here, because a client identifying the key an
//! existing repository is protected by reads it from `GetRepository`: what a read
//! reports is a key of the repository's own account and Region, in every Region a
//! repository can be created in, each field of it recoverable by a client that
//! parses the value; two repositories on two keys are told apart by it; and reading
//! neither changes it nor gives KMS another key, however often and through whichever
//! face it is asked.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{kms_key_arn_of, metadata_of, DEFAULT_ENCRYPTION_KEY_ALIAS};
use crate::repository_lifecycle::{is_uuid, ok, string_at, REGION};

/// The account every ARN this emulator mints is scoped to.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// This service beside KMS on one shared engine, so a repository's key is resolved
/// against the very keys and aliases KMS's own operations write.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codecommit_codecommit_20150413::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    crate::wire::hold_long_term_key(&engine, crate::wire::DEFAULT_ACCESS_KEY_ID);
    router
}

/// The access key id the KMS setup calls are signed with: NOT the key the account
/// holds for an IAM user ([`crate::wire::DEFAULT_ACCESS_KEY_ID`]), because that
/// user's identity-based policies are what decide `kms:CreateKey`, and this suite
/// is about the key a repository reports rather than about KMS authorization. A
/// key no store holds speaks for the account's own principal.
const KMS_CALLER: &str = "AKIDKMSSETUP";

/// One KMS call, the way an SDK sends it: awsJson1_1 under KMS's own target prefix
/// and signing name, at the endpoint of the Region every suite signs for.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    kms_in(router, REGION, operation, body).await
}

/// [`kms`], at `region`'s KMS endpoint: a KMS key is a regional resource, so which
/// key an identifier names is asked of the Region that holds it.
async fn kms_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={KMS_CALLER}/20260101/{region}/kms/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "KMS {operation} in {region}: expected 200, got {status} with {payload}"
    );
    payload
}

/// A KMS key of the account, answered as its key id and its ARN. `multi_region`
/// picks which of the two shapes KMS mints a key id in.
async fn given_key(router: &MultiServiceRouter, multi_region: bool) -> (String, String) {
    let created = kms(
        router,
        "CreateKey",
        &json!({
            "Description": "a key a repository is encrypted under",
            "MultiRegion": multi_region,
        }),
    )
    .await;
    (
        string_at(&created, "/KeyMetadata/KeyId"),
        string_at(&created, "/KeyMetadata/Arn"),
    )
}

/// An alias of the account for `key_id`, answered as the alias name.
async fn given_alias(router: &MultiServiceRouter, alias: &str, key_id: &str) -> String {
    kms(
        router,
        "CreateAlias",
        &json!({ "AliasName": alias, "TargetKeyId": key_id }),
    )
    .await;
    alias.to_string()
}

/// The ARN of the alias `alias` in the Region every suite signs for.
fn alias_arn(alias: &str) -> String {
    format!("arn:aws:kms:{REGION}:{}:{alias}", account())
}

/// Does `reported` name a KMS KEY of this account and Region — the identifier KMS
/// reports a key by — rather than a selector for one?
fn names_a_kms_key(reported: &str) -> bool {
    reported
        .strip_prefix(&format!("arn:aws:kms:{REGION}:{}:key/", account()))
        .is_some_and(is_uuid)
}

/// The `kmsKeyId` of a `CreateRepository` answer.
async fn created_with(router: &MultiServiceRouter, name: &str, selector: Option<&str>) -> String {
    let mut body = json!({ "repositoryName": name });
    if let Some(selector) = selector {
        body["kmsKeyId"] = json!(selector);
    }
    let created = ok(router, "CreateRepository", &body).await;
    string_at(&created, "/repositoryMetadata/kmsKeyId")
}

#[tokio::test]
async fn a_repository_created_with_no_key_reports_the_managed_key_kms_holds() {
    let router = emulator();

    // Two repositories, neither naming a key: both are encrypted under the one
    // Amazon Web Services managed key of this service in this Region.
    let first = created_with(&router, "managed-first", None).await;
    let second = created_with(&router, "managed-second", None).await;

    for (label, reported) in [("managed-first", &first), ("managed-second", &second)] {
        assert!(
            names_a_kms_key(reported),
            "case {label:?}: kmsKeyId must name a KMS key of this account and Region, got \
             {reported:?}"
        );
        assert_ne!(
            reported.as_str(), DEFAULT_ENCRYPTION_KEY_ALIAS,
            "case {label:?}: kmsKeyId must be the key the alias names, not the alias"
        );
    }
    assert_eq!(
        first, second,
        "two repositories created without a key must report the one managed key, got {first:?} \
         and {second:?}"
    );

    // The key is KMS's, not a value this service made up: KMS reports the same key
    // for the predefined alias of this service, and reports it as a key KMS itself
    // manages.
    let described = kms(
        &router,
        "DescribeKey",
        &json!({ "KeyId": DEFAULT_ENCRYPTION_KEY_ALIAS }),
    )
    .await;
    assert_eq!(
        string_at(&described, "/KeyMetadata/Arn"),
        first,
        "the repository's key must be the key KMS holds for {DEFAULT_ENCRYPTION_KEY_ALIAS}, got \
         {described}"
    );
    assert_eq!(
        described
            .pointer("/KeyMetadata/KeyManager")
            .and_then(Value::as_str),
        Some("AWS"),
        "the key behind a predefined alias must be an Amazon Web Services managed key, got \
         {described}"
    );

    // Stored, not just answered: every projection of the repository reports it.
    for name in ["managed-first", "managed-second"] {
        let read_back = metadata_of(&router, name).await;
        assert_eq!(
            read_back.get("kmsKeyId").and_then(Value::as_str),
            Some(first.as_str()),
            "case {name:?}: GetRepository must report the repository's key, got {read_back}"
        );
    }
}

#[tokio::test]
async fn every_selector_form_is_reported_as_the_key_it_names() {
    let router = emulator();

    let (single_id, single_arn) = given_key(&router, false).await;
    let single_alias = given_alias(&router, "alias/codecommit-single-key", &single_id).await;
    let (multi_id, multi_arn) = given_key(&router, true).await;
    let multi_alias = given_alias(&router, "alias/codecommit-multi-key", &multi_id).await;
    // A key id and an alias KMS holds nothing for: the key is the caller's own, in
    // an account or a key store this emulator does not keep.
    let unheld_id = "7c1f0b6e-4d2a-4f88-9c3b-5e6a7b8c9d01";
    let unheld_alias = "alias/codecommit-not-in-this-store";

    // Each row: why it is there, the `kmsKeyId` the request names, and the key the
    // service must report for it.
    let cases: Vec<(&str, String, String)> = vec![
        ("a bare key id", single_id.clone(), single_arn.clone()),
        ("a key ARN", single_arn.clone(), single_arn.clone()),
        ("an alias name", single_alias.clone(), single_arn.clone()),
        ("an alias ARN", alias_arn(&single_alias), single_arn.clone()),
        (
            "the multi-region shape of a key id",
            multi_id.clone(),
            multi_arn.clone(),
        ),
        (
            "an alias of a multi-region key",
            multi_alias.clone(),
            multi_arn.clone(),
        ),
        (
            "an alias ARN of a multi-region key",
            alias_arn(&multi_alias),
            multi_arn.clone(),
        ),
        (
            "a key id KMS holds no key for",
            unheld_id.to_string(),
            kms_key_arn_of(unheld_id),
        ),
        (
            "an alias KMS holds no key for",
            unheld_alias.to_string(),
            unheld_alias.to_string(),
        ),
    ];

    for (index, (why, selector, expected)) in cases.iter().enumerate() {
        // The create path: the key the selector names is what the answer and every
        // later read of the repository report.
        let name = format!("selector-{index}");
        let reported = created_with(&router, &name, Some(selector)).await;
        assert_eq!(
            &reported, expected,
            "case {why:?}: CreateRepository with kmsKeyId {selector:?} must report the key it \
             names, got {reported:?}"
        );
        let read_back = metadata_of(&router, &name).await;
        assert_eq!(
            read_back.get("kmsKeyId").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {why:?}: GetRepository must report the stored key, got {read_back}"
        );
        let batch = ok(
            &router,
            "BatchGetRepositories",
            &json!({ "repositoryNames": [name] }),
        )
        .await;
        assert_eq!(
            batch
                .pointer("/repositories/0/kmsKeyId")
                .and_then(Value::as_str),
            Some(expected.as_str()),
            "case {why:?}: BatchGetRepositories must report the stored key, got {batch}"
        );

        // The rotation path: the same resolution, onto a repository that was
        // created under the managed key.
        let rotated_name = format!("rotated-{index}");
        let managed = created_with(&router, &rotated_name, None).await;
        let rotated = ok(
            &router,
            "UpdateRepositoryEncryptionKey",
            &json!({ "repositoryName": rotated_name, "kmsKeyId": selector }),
        )
        .await;
        assert_eq!(
            rotated.get("kmsKeyId").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {why:?}: the rotation must report the key the selector names, got {rotated}"
        );
        assert_eq!(
            rotated.get("originalKmsKeyId").and_then(Value::as_str),
            Some(managed.as_str()),
            "case {why:?}: the rotation must report the managed key it left behind, got {rotated}"
        );
        let after = metadata_of(&router, &rotated_name).await;
        assert_eq!(
            after.get("kmsKeyId").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {why:?}: the rotated repository must report the new key, got {after}"
        );
    }

    // The invariant behind the table: the four forms KMS documents for `KeyId` are
    // four spellings of ONE key, so every repository named by one of them reports
    // the very same key.
    let spellings = [
        single_id.clone(),
        single_arn.clone(),
        single_alias.clone(),
        alias_arn(&single_alias),
    ];
    for (index, selector) in spellings.iter().enumerate() {
        let name = format!("spelling-{index}");
        let reported = created_with(&router, &name, Some(selector)).await;
        assert_eq!(
            reported, single_arn,
            "case {selector:?}: every spelling of one key must report that key, got {reported:?}"
        );
    }
}

/// The fields a KMS key ARN carries — `arn:<partition>:kms:<region>:<account>:key/<key id>`
/// — as the partition, the Region, the account and the key id, and `None` for a
/// value that names no key. The read a client makes of what `kmsKeyId` reports:
/// joining a repository to `kms:DescribeKey` or `kms:ListAliases` output by key,
/// and recovering the key's account, Region and key id from the value itself.
fn key_arn_fields(reported: &str) -> Option<(&str, &str, &str, &str)> {
    let [prefix, partition, vendor, region, account, resource] =
        reported.splitn(6, ':').collect::<Vec<_>>()[..]
    else {
        return None;
    };
    let key_id = resource.strip_prefix("key/")?;
    (prefix == "arn" && vendor == "kms").then_some((partition, region, account, key_id))
}

/// The `kmsKeyId` of a `CreateRepository` naming no key of its own, signed for
/// `region`'s endpoint — which is the Region the repository it creates lives in.
async fn created_in(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let created = crate::wire::ok_as_in_region(
        router,
        crate::wire::DEFAULT_ACCESS_KEY_ID,
        region,
        "CreateRepository",
        &json!({ "repositoryName": name }),
    )
    .await;
    string_at(&created, "/repositoryMetadata/kmsKeyId")
}

/// The `repositoryMetadata` a `GetRepository` at `region`'s endpoint reports —
/// [`crate::harness::metadata_of`] for a suite that names the Region, over the same
/// one request builder.
async fn metadata_in(router: &MultiServiceRouter, region: &str, name: &str) -> Value {
    let got = crate::wire::ok_as_in_region(
        router,
        crate::wire::DEFAULT_ACCESS_KEY_ID,
        region,
        "GetRepository",
        &json!({ "repositoryName": name }),
    )
    .await;
    got.get("repositoryMetadata")
        .unwrap_or_else(|| panic!("no repositoryMetadata for {name:?} in {got}"))
        .clone()
}

/// The key ARNs KMS holds at `region`'s endpoint, read through `kms:ListKeys` —
/// the keys an audit that joins repositories to KMS by key would have to match.
async fn kms_key_arns(router: &MultiServiceRouter, region: &str) -> BTreeSet<String> {
    let listed = kms_in(router, region, "ListKeys", &json!({})).await;
    listed
        .get("Keys")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("no Keys in {listed}"))
        .iter()
        .map(|entry| string_at(entry, "/KeyArn"))
        .collect()
}

#[tokio::test]
async fn a_read_reports_a_key_of_the_repositorys_own_account_and_region() {
    let router = emulator();

    // Each row is the Region a repository is created in, which a request names by
    // the credential scope it is signed under: a KMS key is a regional resource, so
    // the managed key a repository created without a key of its own is encrypted
    // under is a key of that repository's own Region.
    let mut per_region: BTreeMap<&str, String> = BTreeMap::new();
    for region in [REGION, "eu-west-1", "ap-southeast-2"] {
        let name = format!("regional-{region}");
        let reported = created_in(&router, region, &name).await;

        // The read path reports the stored key, which is the subject here: a client
        // identifying the key an EXISTING repository is protected by reads it from
        // `GetRepository`.
        let read_back = metadata_in(&router, region, &name).await;
        assert_eq!(
            read_back.get("kmsKeyId").and_then(Value::as_str),
            Some(reported.as_str()),
            "case {region:?}: GetRepository must report the repository's stored key, got \
             {read_back}"
        );

        // And what it reports is a KEY, in the account and Region of the repository
        // itself — so every field a client parses out of it is there.
        let Some((partition, key_region, key_account, key_id)) = key_arn_fields(&reported) else {
            panic!("case {region:?}: kmsKeyId must name a KMS key, got {reported:?}");
        };
        assert_eq!(
            partition, "aws",
            "case {region:?}: the key must be named in this partition, got {reported:?}"
        );
        assert_eq!(
            key_region, region,
            "case {region:?}: the key must be held in the repository's own Region, got {reported:?}"
        );
        assert_eq!(
            key_account,
            account(),
            "case {region:?}: the key must be named in the repository's own account, got \
             {reported:?}"
        );
        assert!(
            is_uuid(key_id),
            "case {region:?}: the key id must be an id KMS mints, got {reported:?}"
        );

        // The key is KMS's own rather than a value this service made up: that
        // Region's KMS endpoint answers the same key for this service's predefined
        // alias, as a key KMS itself manages.
        let described = kms_in(
            &router,
            region,
            "DescribeKey",
            &json!({ "KeyId": DEFAULT_ENCRYPTION_KEY_ALIAS }),
        )
        .await;
        assert_eq!(
            string_at(&described, "/KeyMetadata/Arn"),
            reported,
            "case {region:?}: the repository's key must be the key KMS holds for \
             {DEFAULT_ENCRYPTION_KEY_ALIAS} in that Region, got {described}"
        );
        assert_eq!(
            described
                .pointer("/KeyMetadata/KeyManager")
                .and_then(Value::as_str),
            Some("AWS"),
            "case {region:?}: the key behind a predefined alias must be an Amazon Web Services \
             managed key, got {described}"
        );
        per_region.insert(region, reported);
    }

    // Every Region's managed key is a key of its own, so two repositories in two
    // Regions are not reported as encrypted under one key.
    let distinct: BTreeSet<&String> = per_region.values().collect();
    assert_eq!(
        distinct.len(),
        per_region.len(),
        "each Region's managed key must be a key of that Region, got {per_region:?}"
    );
}

#[tokio::test]
async fn reads_report_one_stable_key_per_repository_and_mint_none() {
    let router = emulator();
    let (customer_id, customer_arn) = given_key(&router, false).await;

    // Two repositories on two different keys: the managed key of this service, and
    // a customer-managed key named by its key id.
    let managed = created_with(&router, "read-managed", None).await;
    let customer = created_with(&router, "read-customer", Some(&customer_id)).await;
    assert_eq!(
        customer, customer_arn,
        "the repository named by a key id must report that key, got {customer:?}"
    );
    assert_ne!(
        managed.as_str(), DEFAULT_ENCRYPTION_KEY_ALIAS,
        "a repository created with no key must report the key the alias names, not the alias"
    );
    assert_ne!(
        managed, customer,
        "two repositories on different keys must be told apart by what each reports, got \
         {managed:?} and {customer:?}"
    );

    // Every key reported is a key KMS holds, which is what makes the value joinable
    // to KMS's own output at all.
    let before = kms_key_arns(&router, REGION).await;
    for (name, reported) in [("read-managed", &managed), ("read-customer", &customer)] {
        assert!(
            before.contains(reported),
            "case {name:?}: the key reported must be one KMS holds, got {reported:?} against \
             {before:?}"
        );
    }

    // A read reports the same key however often it is asked, through either face,
    // and mints nothing: the keys KMS holds after the reads are the keys it held
    // before them.
    for round in 0..3 {
        for (name, expected) in [("read-managed", &managed), ("read-customer", &customer)] {
            let read_back = metadata_of(&router, name).await;
            assert_eq!(
                read_back.get("kmsKeyId").and_then(Value::as_str),
                Some(expected.as_str()),
                "case {name:?} round {round}: every read must report the repository's own key, \
                 got {read_back}"
            );
            let batch = ok(
                &router,
                "BatchGetRepositories",
                &json!({ "repositoryNames": [name] }),
            )
            .await;
            assert_eq!(
                batch
                    .pointer("/repositories/0/kmsKeyId")
                    .and_then(Value::as_str),
                Some(expected.as_str()),
                "case {name:?} round {round}: the batch read must report the same key, got {batch}"
            );
        }
    }
    assert_eq!(
        kms_key_arns(&router, REGION).await,
        before,
        "reading a repository must mint no KMS key"
    );

    // The key belongs to the repository rather than to the name it is addressed by,
    // so a rename carries it over untouched.
    ok(
        &router,
        "UpdateRepositoryName",
        &json!({ "oldName": "read-managed", "newName": "read-renamed" }),
    )
    .await;
    let renamed = metadata_of(&router, "read-renamed").await;
    assert_eq!(
        renamed.get("kmsKeyId").and_then(Value::as_str),
        Some(managed.as_str()),
        "a renamed repository must report the key it was created under, got {renamed}"
    );
}

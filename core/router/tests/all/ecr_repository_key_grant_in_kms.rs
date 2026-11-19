// SPDX-License-Identifier: CC0-1.0

//! A repository encrypted with a CUSTOMER MANAGED key holds a KMS grant on that key, and
//! deleting it retires the grant.
//!
//! RULE: encrypting a repository's contents under a KMS key is something Amazon ECR does on
//! the account's behalf, and it may use the account's key only with an authorization KMS
//! itself holds — so creating a repository whose `encryptionConfiguration` NAMES a key takes
//! grants on the key KMS resolves that name to, for the service principal ECR calls KMS as,
//! limited to the data-key operations encrypting and decrypting the repository's contents
//! needs ([`GRANT_OPERATIONS`]) and constrained to the one repository they were taken for
//! (`GrantConstraints$SourceArn` and the encryption context that use carries);
//! `kms:ListGrants` on
//! the key then reports them — [`GRANTS_PER_REPOSITORY`] of them, alike in everything the
//! request states and told apart by the `GrantId` KMS minted for each, since they carry no
//! `Name` — and deleting that repository RETIRES them. A repository that needs no
//! authorization on a key of the account's — one encrypted with an Amazon S3-managed key, one
//! whose KMS configuration names no key — takes none, on that key or any other. A key KMS
//! will not grant on refuses the create with the operation's modelled `KmsException` —
//! carrying the code KMS returned and the answer KMS gave, inside the envelope the SDK layer
//! that made the call states around an exception it caught — and no repository is created.
//!
//! A key owner reads WHO holds a grant on the key from one member whatever kind of grantee it
//! is: "if the grantee principal is an Amazon Web Services service, the GranteePrincipal
//! field contains an Amazon Web Services service principal"
//! (`smithy_query(query="doc",shape="GrantListEntry",member="GranteePrincipal")`), so these
//! grants report the service principal there as well as under `GranteeServicePrincipal`.
//!
//! The grant is taken through the in-process callback, so a single-service router has no KMS
//! to take it on: that is why this suite lives here rather than in the service's own crate.
//!
//! The rows span what the rule generalises over: the KMS encryption type (`KMS`, `KMS_DSSE`),
//! the FORM of the key selector the request names the key by (its ARN, its key id, an alias
//! of it — all three forms `EncryptionConfiguration$kmsKey` admits), and the configurations
//! that name no key of the account's at all. No row writes a grant id, a key id or a
//! creation date: every one of those is generated per run, so each is read from what KMS
//! reported.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use bytes::Bytes;

/// The region every request here is signed for. A KMS key is regional and ECR requires the
/// key to be in the repository's own Region, so it is also where the key is resolved.
const REGION: &str = "eu-west-1";

/// Another Region, where a key that is NOT this Region's is made.
const OTHER_REGION: &str = "us-east-1";

/// The account id every emulated ARN carries (`engine::emulator_account_id`), which is also
/// this registry's id.
const ACCOUNT: &str = "000000000000";

/// The `X-Amz-Target` prefix ECR routes on.
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The `X-Amz-Target` prefix KMS routes on (its service shape is `TrentService`).
const KMS_TARGET_PREFIX: &str = "TrentService";

/// The KMS operations the grants ECR takes must permit, spelled out here rather than imported
/// so a wrong list in the implementation fails this suite — exactly the two the recorded units
/// of this contract carry (`Operations ["Decrypt","GenerateDataKey"]`), which are the data-key
/// operations encrypting a repository's contents under the key needs, as `GrantOperation` names
/// them (`smithy_query(query="shape",shape="GrantOperation")` in the KMS model).
const GRANT_OPERATIONS: [&str; 2] = ["Decrypt", "GenerateDataKey"];

/// The encryption-context key the encrypted repository's ARN is named under in the grant's
/// `EncryptionContextSubset` constraint — what confines the grant to one repository.
const REPOSITORY_ENCRYPTION_CONTEXT_KEY: &str = "aws:ecr:arn";

/// The service principal ECR calls KMS as, for [`REGION`] — written out here for the same
/// reason.
const ECR_SERVICE_PRINCIPAL: &str = "ecr.eu-west-1.amazonaws.com";

/// How many grants the key carries for ONE repository encrypted with it, as AWS's own wire
/// reports them. Spelled out here rather than imported, so a service that takes another
/// number fails this suite.
const GRANTS_PER_REPOSITORY: usize = 2;

/// How long a poll keeps looking before it gives up. Generous on purpose: grant listing is
/// eventually consistent, and this only bounds a wait that would otherwise never end, so
/// neither a slow nor a loaded host decides the outcome.
const POLL_DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(10);

/// ECR and KMS over ONE shared engine, as `router/src/lib.rs` mounts them, so ECR's
/// in-process callback reaches this very KMS.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(engine),
    );
    router
}

/// One awsJson1_1 call, signed for `region`, answered with the status and the JSON body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    host: &str,
    signing_name: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One ECR call.
async fn ecr(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    call(
        router,
        REGION,
        &format!("api.ecr.{REGION}.amazonaws.com"),
        "ecr",
        &format!("{ECR_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await
}

/// One KMS call, at the endpoint of [`REGION`] — the Region a repository of these suites is
/// created in, and therefore the one its key is resolved in.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    kms_in(router, REGION, operation, body).await
}

/// One KMS call at `region`'s endpoint: a KMS key is a regional resource, so a suite that
/// needs a key of ANOTHER Region makes it there.
async fn kms_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    call(
        router,
        region,
        &format!("kms.{region}.amazonaws.com"),
        "kms",
        &format!("{KMS_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await
}

/// A successful call, or a panic naming the case and what came back instead.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    service: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answer) = if service == "ecr" {
        ecr(router, operation, body).await
    } else {
        kms(router, operation, body).await
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {service}:{operation}, got {status} with {answer}"
    );
    answer
}

/// A member of `value` as a string, or `"<unset>"` when it is absent — so an assertion
/// reports the missing member rather than panicking on it.
fn member(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("<unset>")
        .to_string()
}

/// What one grant STATES — everything the `CreateGrant` request that took it fixed, without
/// the two members KMS fills in per call (the `GrantId` it mints and the instant it was
/// created). Two grants taken for the same repository must be alike in exactly this.
fn stated(grant: &Value) -> Value {
    let mut stated = grant.clone();
    if let Some(members) = stated.as_object_mut() {
        members.remove("GrantId");
        members.remove("CreationDate");
    }
    stated
}

/// Every grant `kms:ListGrants` reports on the key `key` names.
async fn grants(router: &MultiServiceRouter, case: &str, key: &str) -> Vec<Value> {
    let listed = ok(router, case, "kms", "ListGrants", &json!({"KeyId": key})).await;
    listed["Grants"].as_array().cloned().unwrap_or_default()
}

/// The grants on `key` whose `SourceArn` constraint names `repository_arn` — the grants taken
/// FOR that one repository.
async fn grants_for(
    router: &MultiServiceRouter,
    case: &str,
    key: &str,
    repository_arn: &str,
) -> Vec<Value> {
    grants(router, case, key)
        .await
        .into_iter()
        .filter(|grant| member(&grant["Constraints"], "SourceArn") == repository_arn)
        .collect()
}

/// Look at the grants `key` carries for `repository_arn` again and again until `settled`
/// holds of them, and answer them then.
///
/// Grant listing is eventually consistent, so the CONDITION is waited for rather than a span
/// of wall clock: the deadline is generous and only bounds a wait that would never end.
async fn grants_until(
    router: &MultiServiceRouter,
    case: &str,
    key: &str,
    repository_arn: &str,
    expectation: &str,
    settled: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let since = ::std::time::Instant::now();
    loop {
        let found = grants_for(router, case, key, repository_arn).await;
        if settled(&found) {
            return found;
        }
        assert!(
            since.elapsed() < POLL_DEADLINE,
            "case {case:?}: kms:ListGrants never reported {expectation} for {repository_arn}; \
             it last reported {found:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The ARN of the repository `name` in this account's registry.
fn repository_arn(name: &str) -> String {
    format!("arn:aws:ecr:{REGION}:{ACCOUNT}:repository/{name}")
}

/// RULE: a repository whose encryption configuration names a key of the account's takes its
/// grants on the key KMS resolves that name to, whichever selector form named it and
/// whichever KMS encryption type was asked for — and deleting the repository retires them.
#[tokio::test]
async fn a_key_encrypted_repository_takes_and_retires_its_grants_on_the_key() {
    let router = emulator();

    // The key the rows name, made through KMS's own wire so every row names a key that
    // service really holds rather than an ARN this suite wrote.
    let created = ok(&router, "setup", "kms", "CreateKey", &json!({})).await;
    let key_arn = member(&created["KeyMetadata"], "Arn");
    let key_id = member(&created["KeyMetadata"], "KeyId");
    let alias = "alias/ecr-suite-key";
    ok(
        &router,
        "setup",
        "kms",
        "CreateAlias",
        &json!({"AliasName": alias, "TargetKeyId": key_id}),
    )
    .await;
    let baseline = grants(&router, "setup", &key_arn).await;
    assert!(
        baseline.is_empty(),
        "setup: a freshly created key must carry no grants, got {baseline:?}"
    );

    // (case, repository name, encryption type, the selector the request names the key by)
    let rows: Vec<(&str, &str, &str, String)> = vec![
        (
            "KMS encryption naming the key by its ARN",
            "grant-by-key-arn",
            "KMS",
            key_arn.clone(),
        ),
        (
            "KMS encryption naming the key by its key id",
            "grant-by-key-id",
            "KMS",
            key_id.clone(),
        ),
        (
            "KMS encryption naming the key by an alias of it",
            "grant-by-alias",
            "KMS",
            alias.to_string(),
        ),
        (
            "dual-layer KMS encryption naming the key by its ARN",
            "grant-dsse-by-key-arn",
            "KMS_DSSE",
            key_arn.clone(),
        ),
    ];

    for (case, name, encryption_type, selector) in rows {
        let arn = repository_arn(name);
        let created = ok(
            &router,
            case,
            "ecr",
            "CreateRepository",
            &json!({
                "repositoryName": name,
                "encryptionConfiguration": {
                    "encryptionType": encryption_type,
                    "kmsKey": selector,
                },
            }),
        )
        .await;
        assert_eq!(
            member(&created["repository"], "repositoryArn"),
            arn,
            "case {case:?}: expected the create to report the repository's ARN, got {created}"
        );

        let held = grants_until(
            &router,
            case,
            &key_arn,
            &arn,
            "the grants ECR takes on a key it encrypts a repository with",
            |found| found.len() >= GRANTS_PER_REPOSITORY,
        )
        .await;
        assert_eq!(
            held.len(),
            GRANTS_PER_REPOSITORY,
            "case {case:?}: expected exactly {GRANTS_PER_REPOSITORY} grants for {arn}, got \
             {held:?}"
        );
        // The grants are alike in everything the request states and told apart only by the
        // id KMS minted for each.
        let ids: BTreeSet<String> = held.iter().map(|grant| member(grant, "GrantId")).collect();
        assert_eq!(
            ids.len(),
            held.len(),
            "case {case:?}: expected every grant to carry its own GrantId, got {held:?}"
        );
        for grant in &held {
            assert_eq!(
                stated(grant),
                stated(&held[0]),
                "case {case:?}: expected the grants to state the same authorization, got \
                 {held:?}"
            );
        }
        let grant = &held[0];

        // The grant is on the KEY the selector resolved to, whichever form named it.
        assert_eq!(
            member(grant, "KeyId"),
            key_arn,
            "case {case:?}: expected the grant to be on the key {key_arn}, got {grant}"
        );
        // It is confined to the ONE repository it was taken for, in both terms KMS confines
        // a grant by: the resource the key may be used on behalf of, and the encryption
        // context that use carries.
        assert_eq!(
            (
                member(&grant["Constraints"], "SourceArn"),
                member(
                    &grant["Constraints"]["EncryptionContextSubset"],
                    REPOSITORY_ENCRYPTION_CONTEXT_KEY,
                ),
            ),
            (arn.clone(), arn.clone()),
            "case {case:?}: expected the grant to be constrained to {arn}, got {grant}"
        );
        // Its grantee is the service principal ECR calls KMS as — reported as the grant's
        // grantee identity and as the service it is — and the principal that may retire it
        // is one that can: a grant nothing could retire would outlive the repository.
        assert_eq!(
            (
                member(grant, "GranteePrincipal"),
                member(grant, "GranteeServicePrincipal"),
                member(grant, "RetiringServicePrincipal"),
            ),
            (
                ECR_SERVICE_PRINCIPAL.to_string(),
                ECR_SERVICE_PRINCIPAL.to_string(),
                ECR_SERVICE_PRINCIPAL.to_string(),
            ),
            "case {case:?}: expected the grant's grantee and retiring principal to be \
             {ECR_SERVICE_PRINCIPAL}, got {grant}"
        );
        assert_eq!(
            member(grant, "IssuingAccount"),
            format!("arn:aws:iam::{ACCOUNT}:root"),
            "case {case:?}: expected the grant to be issued under this account, got {grant}"
        );
        let permitted: Vec<String> = grant["Operations"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|operation| operation.as_str().map(str::to_string))
            .collect();
        assert_eq!(
            permitted,
            GRANT_OPERATIONS.to_vec(),
            "case {case:?}: expected the grant to permit exactly the data-key operations \
             encrypting the repository needs, got {permitted:?}"
        );
        // Every grant carries its own id, so two repositories on one key are told apart.
        assert!(
            !member(grant, "GrantId").is_empty() && member(grant, "GrantId") != "<unset>",
            "case {case:?}: expected the grant to carry a GrantId, got {grant}"
        );

        // A key owner asking WHO holds a grant on the key is answered these grants by the
        // grantee they report: what the reader reports is what it filters on.
        let filtered = ok(
            &router,
            case,
            "kms",
            "ListGrants",
            &json!({"KeyId": key_arn, "GranteePrincipal": ECR_SERVICE_PRINCIPAL}),
        )
        .await;
        let filtered: BTreeSet<String> = filtered["Grants"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|grant| member(grant, "GrantId"))
            .collect();
        assert_eq!(
            filtered, ids,
            "case {case:?}: expected the grants filtered by their own grantee to be the \
             grants this repository took, got {filtered:?}"
        );

        // Deleting the repository retires every authorization taken for it.
        ok(
            &router,
            case,
            "ecr",
            "DeleteRepository",
            &json!({"repositoryName": name, "force": true}),
        )
        .await;
        grants_until(
            &router,
            case,
            &key_arn,
            &arn,
            "no grant at all, the delete having retired them",
            <[Value]>::is_empty,
        )
        .await;
    }

    // Every row's grants were retired with its repository, so the key is back to carrying
    // none.
    let left = grants(&router, "every row deleted", &key_arn).await;
    assert!(
        left.is_empty(),
        "every row deleted: expected the key to carry no grant once every repository taken on \
         it is gone, got {left:?}"
    );
}

/// RULE: a repository that needs no authorization on a key of the account's takes none — on
/// the key another repository is encrypted with, or on any other.
#[tokio::test]
async fn a_repository_that_names_no_key_of_the_accounts_takes_no_grant() {
    let router = emulator();
    let created = ok(&router, "setup", "kms", "CreateKey", &json!({})).await;
    let key_arn = member(&created["KeyMetadata"], "Arn");

    // One repository that DOES take its grants on the key, so the rows below are judged
    // against a key that already carries some: a row must add none, not merely leave an empty
    // list empty.
    let encrypted = "takes-the-only-grants";
    ok(
        &router,
        "setup",
        "ecr",
        "CreateRepository",
        &json!({
            "repositoryName": encrypted,
            "encryptionConfiguration": {"encryptionType": "KMS", "kmsKey": key_arn},
        }),
    )
    .await;
    let expected = repository_arn(encrypted);
    grants_until(
        &router,
        "setup",
        &key_arn,
        &expected,
        "the grants of the repository encrypted with this key",
        |found| found.len() == GRANTS_PER_REPOSITORY,
    )
    .await;

    // (case, repository name, the encryption configuration the create carries)
    let rows: Vec<(&str, &str, Option<Value>)> = vec![
        (
            "the Amazon S3-managed encryption type on the same key's account",
            "s3-managed-encryption",
            Some(json!({"encryptionType": "AES256"})),
        ),
        (
            "KMS encryption naming no key, which is the Amazon Web Services managed key",
            "kms-with-no-key",
            Some(json!({"encryptionType": "KMS"})),
        ),
        (
            "dual-layer KMS encryption naming no key",
            "kms-dsse-with-no-key",
            Some(json!({"encryptionType": "KMS_DSSE"})),
        ),
        (
            "no encryption configuration at all",
            "no-encryption-configuration",
            None,
        ),
    ];

    for (case, name, encryption) in rows {
        let mut request = json!({"repositoryName": name});
        if let Some(encryption) = encryption {
            request["encryptionConfiguration"] = encryption;
        }
        ok(&router, case, "ecr", "CreateRepository", &request).await;

        let held = grants(&router, case, &key_arn).await;
        assert_eq!(
            held.len(),
            GRANTS_PER_REPOSITORY,
            "case {case:?}: expected the key to carry only the grants of the repository \
             encrypted with it, got {held:?}"
        );
        for grant in &held {
            assert_eq!(
                member(&grant["Constraints"], "SourceArn"),
                expected,
                "case {case:?}: expected every grant on the key to be one taken for \
                 {expected}, got {held:?}"
            );
        }
    }
}

/// RULE: a key this Region's KMS cannot grant on refuses the create with the operation's
/// modelled `KmsException`, carrying the code KMS returned and the answer KMS gave, and no
/// repository is created.
///
/// The rows differ in WHY the key cannot be granted on — a key identifier this Region holds
/// no key for, a key ARN of another Region (which this Region's endpoint can never hold), and
/// a key KMS holds but that is disabled, where it is taking the grant rather than resolving
/// the key that is refused.
#[tokio::test]
async fn a_key_that_cannot_be_granted_on_refuses_the_create() {
    let router = emulator();

    // A key of ANOTHER Region, made at that Region's own endpoint, so the row names a key
    // this suite did not write the ARN of.
    let (status, elsewhere) = kms_in(&router, OTHER_REGION, "CreateKey", &json!({})).await;
    assert_eq!(
        status, 200,
        "setup: expected kms:CreateKey in {OTHER_REGION} to answer 200, got {elsewhere}"
    );
    let foreign_key = member(&elsewhere["KeyMetadata"], "Arn");

    // A key of this Region that the owner has disabled: KMS resolves it, and refuses to
    // grant on it.
    let disabled = ok(&router, "setup", "kms", "CreateKey", &json!({})).await;
    let disabled_key = member(&disabled["KeyMetadata"], "Arn");
    ok(
        &router,
        "setup",
        "kms",
        "DisableKey",
        &json!({"KeyId": disabled_key}),
    )
    .await;

    // A well-formed key ARN of this account and Region that KMS resolves no key for: its
    // key id is this suite's own text, never one KMS minted.
    let absent = format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/00000000-0000-4000-8000-000000000000");

    // (case, repository name, the selector the create names the key by, the KMS error code
    //  the refusal must carry)
    let rows: Vec<(&str, &str, String, &str)> = vec![
        (
            "a key ARN of this Region that KMS resolves no key for",
            "refused-absent-key",
            absent,
            "NotFoundException",
        ),
        (
            "a key ARN of another Region, which this Region's endpoint cannot hold",
            "refused-foreign-key",
            foreign_key,
            "NotFoundException",
        ),
        (
            "a key of this Region the owner has disabled",
            "refused-disabled-key",
            disabled_key.clone(),
            "DisabledException",
        ),
    ];

    for (case, name, selector, code) in rows {
        let (status, refused) = ecr(
            &router,
            "CreateRepository",
            &json!({
                "repositoryName": name,
                "encryptionConfiguration": {"encryptionType": "KMS", "kmsKey": selector},
            }),
        )
        .await;
        assert_eq!(
            (
                status,
                member(&refused, "__type").contains("KmsException"),
                member(&refused, "kmsError"),
            ),
            (400, true, code.to_string()),
            "case {case:?}: expected HTTP 400 KmsException carrying kmsError {code}, got \
             {status} with {refused}"
        );

        // The message is the KMS answer as the SDK layer that made the call renders the
        // exception it caught: the caught class, named by the code KMS answered, the sentence
        // KMS itself worded, and the clause naming that answer — down to the status KMS
        // answered with. Both the sentence and that status are read from KMS rather than
        // written here, so no wording of this suite's own can pass for the dependency's; the
        // request id is the answer's and is asserted only by its place.
        let message = member(&refused, "message");
        let (kms_status, said) = kms_said(&router, case, &selector).await;
        let opening = format!("com.amazonaws.services.kms.model.{code}: {said} ");
        let clause = format!(
            "(Service: AWSKMS; Status Code: {kms_status}; Error Code: {code}; Request ID: "
        );
        assert!(
            message.starts_with(&opening),
            "case {case:?}: expected the refusal to open {opening:?}, got {message:?}"
        );
        assert!(
            message.contains(&clause) && message.ends_with("; Proxy: null)"),
            "case {case:?}: expected the refusal to name the KMS answer it relays with \
             {clause:?} and to end in the proxy it came through, got {message:?}"
        );

        // Nothing was created: a repository whose contents nothing may encrypt is no
        // repository.
        let (status, read) = ecr(
            &router,
            "DescribeRepositories",
            &json!({"repositoryNames": [name]}),
        )
        .await;
        assert_eq!(
            (
                status,
                member(&read, "__type").contains("RepositoryNotFoundException"),
            ),
            (400, true),
            "case {case:?}: expected the refused repository not to exist, got {status} with \
             {read}"
        );
    }

    // Not one grant was taken on the key that exists: a create that is refused leaves no
    // authorization behind on anybody's key.
    let held = grants(&router, "the disabled key", &disabled_key).await;
    assert!(
        held.is_empty(),
        "expected a refused create to leave no grant on the key, got {held:?}"
    );
}

/// The status and the sentence of KMS's OWN refusal about the key `selector` names — read
/// through the very call ECR makes to resolve the key, and through the grant it then takes
/// when the key resolves.
async fn kms_said(router: &MultiServiceRouter, case: &str, selector: &str) -> (u16, String) {
    let (status, described) = kms(router, "DescribeKey", &json!({"KeyId": selector})).await;
    if status != 200 {
        return (status, member(&described, "message"));
    }
    // The key resolves, so what refuses is taking the grant on it. The grantee is an
    // identity of this account, which is beside the point for a key that cannot carry a
    // grant at all: the sentence states the key's own condition.
    let (status, refused) = kms(
        router,
        "CreateGrant",
        &json!({
            "KeyId": selector,
            "GranteePrincipal": format!("arn:aws:iam::{ACCOUNT}:role/KeyCondition"),
            "Operations": ["Decrypt"],
        }),
    )
    .await;
    assert_ne!(
        status, 200,
        "case {case:?}: kms:CreateGrant on {selector} must be refused for this row, got \
         {refused}"
    );
    (status, member(&refused, "message"))
}

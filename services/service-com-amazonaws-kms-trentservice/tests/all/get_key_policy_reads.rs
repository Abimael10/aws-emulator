// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the READER of a KMS key's key policy, `GetKeyPolicy`, driven
//! over the wire through the shared [`super::harness`].
//!
//! The sibling [`super::key_policy`] suite drives this reader from the WRITER of the
//! document (`PutKeyPolicy`, and the policy `CreateKey` attaches); this one is about
//! the reader's own rules:
//!
//! 1. "Gets a key policy attached to the specified KMS key"
//!    (`smithy_query(query="doc",shape="GetKeyPolicy")`) — so it is a pure REPORT of
//!    the document the key holds: repeated reads answer the same document under the
//!    same name, and reading changes nothing a later caller sees.
//! 2. The document is a property of the key alone, so it does not move when the key is
//!    written for any other reason, nor when the key moves through its key states.
//! 3. "If no policy name is specified, the default value is default"
//!    (`smithy_query(query="doc",shape="GetKeyPolicyRequest",member="PolicyName")`), so
//!    omitting the member and naming `default` are the same request.
//! 4. "Specify the key ID or key ARN of the KMS key"
//!    (`smithy_query(query="doc",shape="GetKeyPolicyRequest",member="KeyId")`) — the
//!    member takes a KEY identifier only, so an identifier that names an alias is
//!    refused for its KIND before anything is resolved, rather than being resolved as
//!    an alias and reported absent.

use serde_json::{json, Value};

use super::harness::{
    alias_arn, b64, create_alias, create_key, describe, ok, refused, reported_policy, router, text,
    REGION,
};

/// The only key policy name a KMS key has a policy under: "The only valid value is
/// default" (`smithy_query(query="doc",shape="GetKeyPolicyResponse",member="PolicyName")`).
const DEFAULT_POLICY_NAME: &str = "default";

/// How an identifier of the wrong KIND is refused: the fixed sentence KMS words this
/// refusal with, which carries nothing taken from the request or from state.
const ALIASES_NOT_SUPPORTED: &str = "Key Aliases are not supported for this operation.";

/// The whole `GetKeyPolicy` answer for `identifier` — both the document and the name it
/// is served under — with `PolicyName` carried only when `policy_name` names it.
async fn read_policy(
    router: &smithy_server_runtime::MultiServiceRouter,
    identifier: &str,
    policy_name: Option<&str>,
) -> Value {
    let mut request = json!({"KeyId": identifier});
    if let Some(policy_name) = policy_name {
        request["PolicyName"] = json!(policy_name);
    }
    ok(router, "GetKeyPolicy", &request).await
}

/// Assert an answer carries a non-empty document served under the one policy name, and
/// report that document.
fn served_document(case: &str, answer: &Value) -> String {
    let name = text(case, answer, "PolicyName");
    assert_eq!(
        name, DEFAULT_POLICY_NAME,
        "case {case:?}: a key serves its policy under the name {DEFAULT_POLICY_NAME}, got \
         {name:?}"
    );
    let document = text(case, answer, "Policy");
    assert!(
        !document.is_empty(),
        "case {case:?}: expected the key's policy document, got {document:?}"
    );
    document
}

/// The policy names `ListKeyPolicies` reports for a key, which must not move either.
async fn policy_names(
    router: &smithy_server_runtime::MultiServiceRouter,
    identifier: &str,
) -> Value {
    ok(router, "ListKeyPolicies", &json!({"KeyId": identifier})).await["PolicyNames"].clone()
}

/// Rule 1 and rule 3: the read is a report. Every read of a key answers the same
/// document under the same name, whether the request names the policy or leaves the
/// member out, and the reads leave the key exactly as it was — the metadata
/// `DescribeKey` reports and the names `ListKeyPolicies` reports are unchanged
/// afterwards.
///
/// The rows differ in the kind of key holding the policy — the `KeySpec`/`KeyUsage`
/// combinations this service mints, a multi-Region primary, and a key created with no
/// key material of its own — because a key policy is not a property of any of them.
#[tokio::test]
async fn reading_a_key_policy_changes_nothing_a_later_caller_sees() {
    let router = router();

    // Rows: a key kind and the rest of its `CreateKey` request.
    let kinds: [(&str, Value); 6] = [
        ("the default symmetric encryption key", json!({})),
        ("a multi-Region primary key", json!({"MultiRegion": true})),
        (
            "an HMAC_384 key",
            json!({"KeySpec": "HMAC_384", "KeyUsage": "GENERATE_VERIFY_MAC"}),
        ),
        (
            "an RSA_3072 encryption key",
            json!({"KeySpec": "RSA_3072", "KeyUsage": "ENCRYPT_DECRYPT"}),
        ),
        (
            "an ECC_NIST_P256 signing key",
            json!({"KeySpec": "ECC_NIST_P256", "KeyUsage": "SIGN_VERIFY"}),
        ),
        ("a key created with no key material", json!({"Origin": "EXTERNAL"})),
    ];

    for (described, request) in kinds {
        let metadata = create_key(&router, &request).await;
        let key_id = text(described, &metadata, "KeyId");
        let before_metadata = describe(&router, &key_id).await;
        let before_names = policy_names(&router, &key_id).await;

        let first = read_policy(&router, &key_id, None).await;
        let document = served_document(described, &first);

        // Rows: how the request spells the policy name, over several rounds — an
        // omitted member and the one valid name are the same request, and neither
        // answer moves between rounds.
        for round in 1..=3 {
            for spelling in [None, Some(DEFAULT_POLICY_NAME)] {
                let case = format!("{described}, read {round} with PolicyName {spelling:?}");
                let answer = read_policy(&router, &key_id, spelling).await;
                assert_eq!(
                    answer, first,
                    "case {case:?}: every read must answer the same document under the same \
                     name, got {answer} (was {first})"
                );
            }
        }

        let after_metadata = describe(&router, &key_id).await;
        assert_eq!(
            after_metadata, before_metadata,
            "case {described:?}: reading the key policy must persist nothing, got \
             {after_metadata} (was {before_metadata})"
        );
        let after_names = policy_names(&router, &key_id).await;
        assert_eq!(
            after_names, before_names,
            "case {described:?}: reading the key policy must leave the policy listing as it \
             was, got {after_names} (was {before_names})"
        );
        assert_eq!(
            after_names,
            json!([DEFAULT_POLICY_NAME]),
            "case {described:?}: a key holds one policy, named {DEFAULT_POLICY_NAME}, got \
             {after_names}"
        );
        assert!(
            document.contains("Statement"),
            "case {described:?}: the served document must be an IAM policy document, got \
             {document:?}"
        );
    }
}

/// Rule 2: the document is the key's own, so writing the key for any OTHER reason —
/// and moving it through its key states — leaves the answer byte for byte as it was.
///
/// The rows are the writes this slice can make to a key without touching its policy:
/// its description, its tags, an alias, uses of its key material, its rotation
/// schedule, and the key-state moves `DisableKey`/`EnableKey` and
/// `ScheduleKeyDeletion`/`CancelKeyDeletion` — a key pending deletion still serves the
/// read, as `DescribeKey` still reports its metadata.
#[tokio::test]
async fn the_served_document_does_not_move_when_the_key_is_written_for_another_reason() {
    let router = router();
    let account = engine::emulator_account_id();
    let metadata = create_key(&router, &json!({"Description": "policy reader"})).await;
    let key_id = text("setup", &metadata, "KeyId");
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowRoot",
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{account}:root")},
            "Action": "kms:*",
            "Resource": "*"
        }]
    })
    .to_string();
    ok(
        &router,
        "PutKeyPolicy",
        &json!({"KeyId": key_id, "Policy": document}),
    )
    .await;

    let served = read_policy(&router, &key_id, None).await;
    assert_eq!(
        served_document("setup", &served),
        reported_policy(&document),
        "case setup: the reader must serve the attached document in KMS's own rendering of \
         it, got {served}"
    );

    // Rows: a call that writes the key, or uses it, without touching its policy. The
    // order matters only in that a use of the key material comes before the key is
    // disabled and the deletion moves come last.
    let writes: [(&str, &str, Value); 12] = [
        (
            "a new description",
            "UpdateKeyDescription",
            json!({"KeyId": key_id, "Description": "renamed"}),
        ),
        (
            "a new tag",
            "TagResource",
            json!({"KeyId": key_id, "Tags": [{"TagKey": "env", "TagValue": "prod"}]}),
        ),
        (
            "a tag removed",
            "UntagResource",
            json!({"KeyId": key_id, "TagKeys": ["env"]}),
        ),
        (
            "an alias added",
            "CreateAlias",
            json!({"AliasName": "alias/policy-reader", "TargetKeyId": key_id}),
        ),
        (
            "the key material used to encrypt",
            "Encrypt",
            json!({"KeyId": key_id, "Plaintext": b64(b"key policy")}),
        ),
        (
            "a data key generated under the key",
            "GenerateDataKeyWithoutPlaintext",
            json!({"KeyId": key_id, "KeySpec": "AES_256"}),
        ),
        (
            "automatic rotation configured",
            "EnableKeyRotation",
            json!({"KeyId": key_id, "RotationPeriodInDays": 120}),
        ),
        (
            "automatic rotation turned off",
            "DisableKeyRotation",
            json!({"KeyId": key_id}),
        ),
        ("the key disabled", "DisableKey", json!({"KeyId": key_id})),
        ("the key enabled again", "EnableKey", json!({"KeyId": key_id})),
        (
            "the key pending deletion",
            "ScheduleKeyDeletion",
            json!({"KeyId": key_id, "PendingWindowInDays": 7}),
        ),
        (
            "the deletion cancelled",
            "CancelKeyDeletion",
            json!({"KeyId": key_id}),
        ),
    ];

    for (case, operation, request) in writes {
        ok(&router, operation, &request).await;
        let answer = read_policy(&router, &key_id, None).await;
        assert_eq!(
            answer, served,
            "case {case:?}: {operation} must leave the key policy answer exactly as it was, \
             got {answer} (was {served})"
        );
    }
}

/// Rule 4: the reader's `KeyId` takes a KEY identifier — a key id or a key ARN — so an
/// identifier that names an ALIAS is of the wrong KIND for the member, and is refused
/// `InvalidArnException` with the fixed sentence BEFORE any alias or key is looked up.
/// Because the judgement precedes the lookup, every alias identifier is refused
/// identically: one naming a key this reader would happily serve, one naming nothing, one
/// pinned by its ARN to another Region or account, and one under the namespace an Amazon
/// Web Services managed key's alias lives in all get the same answer, and none of them
/// discloses whether an alias of that name exists. A value that IS a key identifier but
/// names no key is still reported as the missing key it is (`NotFoundException`), so the
/// classification follows the identifier's kind and not merely its failure to resolve.
///
/// The rows differ in the form of the alias identifier (bare `alias/<name>` and an alias
/// ARN), in the characters and length of the name, in the Region and account an ARN pins
/// it to, in whether an alias of that name exists and what it targets, in how the request
/// spells `PolicyName`, and in the key state of the key behind the alias.
///
/// Scope: this suite is about THIS reader, so the dimensions above are its own. That the
/// same judgement is every such operation's — the span over operations rather than over
/// identifiers — is [`super::refusal_fidelity`]'s
/// `an_alias_identifier_is_refused_for_its_kind_before_anything_is_resolved`.
#[tokio::test]
async fn an_alias_identifier_is_refused_for_its_kind_rather_than_resolved() {
    let router = router();
    let account = engine::emulator_account_id();
    // A 12-digit value that is an account id (`AccountIdType`) other than this one.
    let other_account = if account == "210987654321" {
        "123456789012"
    } else {
        "210987654321"
    };
    let metadata = create_key(&router, &json!({"Description": "alias target"})).await;
    let key_id = text("setup", &metadata, "KeyId");
    let key_arn = text("setup", &metadata, "Arn");

    // Aliases this endpoint really holds, spanning the characters `AliasNameType`'s
    // `^[a-zA-Z0-9:/_-]+$` pattern allows and its 256-character maximum
    // (`smithy_query(query="shape",shape="AliasNameType")`).
    let longest = format!("alias/{}", "a".repeat(250));
    let held: [&str; 3] = ["alias/policy-target", "alias/Team_1-2:3/4", &longest];
    for alias in held {
        create_alias(&router, "setup", alias, &key_id).await;
    }

    // Rows: the case, and the identifier the request puts in `KeyId`.
    let mut rows: Vec<(String, String)> = Vec::new();
    for alias in held {
        rows.push((format!("the bare name {alias}"), alias.to_string()));
        rows.push((
            format!("the alias ARN of {alias}"),
            alias_arn(REGION, alias),
        ));
    }
    rows.push((
        "a bare name no alias of this endpoint answers to".to_string(),
        "alias/absent".to_string(),
    ));
    rows.push((
        "the alias ARN of a name no alias of this endpoint answers to".to_string(),
        alias_arn(REGION, "alias/absent"),
    ));
    // A Region other than the one the request is signed for, derived from [`REGION`] so
    // the row cannot degenerate into a same-Region one if that constant moves.
    let other_region = if REGION == "us-west-2" {
        "eu-west-1"
    } else {
        "us-west-2"
    };
    rows.push((
        "an alias ARN pinned to another Region".to_string(),
        format!("arn:aws:kms:{other_region}:{account}:alias/policy-target"),
    ));
    rows.push((
        "an alias ARN pinned to another account".to_string(),
        format!("arn:aws:kms:{REGION}:{other_account}:alias/policy-target"),
    ));
    rows.push((
        "an alias under the aws/ namespace a managed key's alias lives in".to_string(),
        "alias/aws/kms".to_string(),
    ));
    // The length of the NAME is immaterial to the kind of identifier it is: this member
    // is bounded at 2048 characters of its own
    // (`smithy_query(query="operation",operation="GetKeyPolicy")`), so a name one
    // character past `AliasNameType`'s maximum still reaches this judgement.
    rows.push((
        "a bare name one character past AliasNameType's maximum".to_string(),
        format!("alias/{}", "b".repeat(251)),
    ));

    // The positive control FIRST, so the rows below are known to be refused for the
    // identifier rather than for the state of the service: both key identifier forms of
    // the very key the aliases target serve the key's policy.
    for identifier in [&key_id, &key_arn] {
        let case = format!("the key named by {identifier}");
        let answer = read_policy(&router, identifier, None).await;
        served_document(&case, &answer);
    }

    for (case, identifier) in &rows {
        // Rows: how the request spells `PolicyName` — left out, the one valid name, and
        // another name — none of which is reached, the identifier being judged first.
        for spelling in [None, Some(DEFAULT_POLICY_NAME), Some("Custom")] {
            let case = format!("{case} with PolicyName {spelling:?}");
            let mut request = json!({"KeyId": identifier});
            if let Some(policy_name) = spelling {
                request["PolicyName"] = json!(policy_name);
            }
            let (status, code, message) = refused(&router, REGION, "GetKeyPolicy", &request).await;
            assert_eq!(
                (status, code.as_str()),
                (400, "InvalidArnException"),
                "case {case:?}: expected HTTP 400 InvalidArnException, got {status} {code} \
                 ({message:?})"
            );
            assert_eq!(
                message, ALIASES_NOT_SUPPORTED,
                "case {case:?}: expected {ALIASES_NOT_SUPPORTED:?}, got {message:?}"
            );
        }
    }

    // A value that IS a key identifier but names no key of this endpoint is the missing
    // KEY it is — the error class the alias rows above must never be given. What is
    // asserted here is that CLASS and nothing else: the status a key-not-found refusal
    // carries is the subject of [`super::client_error_status`], which owns it for every
    // operation that resolves a `KeyId`, so this suite does not restate it.
    for (case, identifier) in [
        (
            "an unknown key id",
            "00000000-1111-2222-3333-444444444444".to_string(),
        ),
        (
            "an unknown key ARN",
            format!("arn:aws:kms:{REGION}:{account}:key/00000000-1111-2222-3333-444444444444"),
        ),
    ] {
        let (_status, code, message) =
            refused(&router, REGION, "GetKeyPolicy", &json!({"KeyId": identifier})).await;
        assert_eq!(
            code, "NotFoundException",
            "case {case:?}: a key identifier that names no key is the missing key it is, got \
             {code} ({message:?})"
        );
    }

    // The key state of the key behind the alias is not reached either: the same alias
    // identifier is refused the same way while that key is disabled and while it is
    // pending deletion — states in which the reader still serves the key's own
    // identifiers.
    for (case, operation, request) in [
        (
            "the target key disabled",
            "DisableKey",
            json!({"KeyId": key_id}),
        ),
        (
            "the target key pending deletion",
            "ScheduleKeyDeletion",
            json!({"KeyId": key_id, "PendingWindowInDays": 7}),
        ),
    ] {
        ok(&router, operation, &request).await;
        served_document(case, &read_policy(&router, &key_id, None).await);
        let (status, code, message) = refused(
            &router,
            REGION,
            "GetKeyPolicy",
            &json!({"KeyId": "alias/policy-target"}),
        )
        .await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "InvalidArnException", ALIASES_NOT_SUPPORTED),
            "case {case:?}: an alias identifier must be refused for its kind whatever the \
             state of the key behind it, got {status} {code} ({message:?})"
        );
    }
}

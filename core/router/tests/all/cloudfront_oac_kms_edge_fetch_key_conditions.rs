// SPDX-License-Identifier: CC0-1.0

//! Which CONDITION VALUES an OAC-signed CloudFront origin fetch of an SSE-KMS object carries to
//! the object's KMS key, and how Amazon S3 words the refusal when a grant scoped by one of them
//! does not hold — CloudFront, Amazon S3 and KMS on one engine.
//!
//! Two halves of one rule:
//!
//! * serving an SSE-KMS object's bytes performs `kms:Decrypt` on the customer-managed key the
//!   write stored it under, and that call is decided with the values the request itself carries,
//!   whoever made it: the ENCRYPTION CONTEXT S3 binds the object's data key to — the `aws:s3:arn`
//!   entry naming the resource that was encrypted, the object's own ARN, or the bucket's for an
//!   object stored under an S3 Bucket Key (Amazon S3 User Guide, *Using server-side encryption
//!   with KMS keys*, "Encryption context") — the service the call reached the key THROUGH
//!   (`kms:ViaService`, which "limits use of a KMS key to requests from particular Amazon Web
//!   Services services"), and, for a call another service makes as its own service principal, the
//!   account of the resource it is acting for (`aws:SourceAccount`, "the account the resource
//!   making a service-to-service request belongs to"). A grant scoped by one of them therefore
//!   governs the fetch exactly where its value holds, and nowhere else: under one key, one key
//!   policy and one distribution, the object the grant's context names is served and its sibling
//!   is not.
//! * a refusal S3 answers such a fetch with is worded in S3's OWN published text — HTTP 403
//!   `AccessDenied`, message "Access Denied" — and names neither the principal, nor the key, nor
//!   the KMS action, whichever of those conditions withheld the decrypt: there is no IAM identity
//!   of a caller's to name in one, and whoever reads the answer is the viewer at the edge rather
//!   than the principal the decision was taken against.
//!
//! The rows differ along which condition key the grant is scoped by and whether the fetch's own
//! value satisfies it: the encryption context of the object asked for, the context of the BUCKET
//! (which an object not stored under a Bucket Key is not bound to), the context's entry NAME under
//! a set qualifier, this service's regional endpoint and another service's, and the resource
//! account of this distribution and of another account. Each row fetches BOTH objects of its
//! bucket, so a row whose grant admits one of them states in the same instant that it admits the
//! other or does not.
//!
//! What the siblings leave to this suite: `cloudfront_oac_kms_s3_origin_edge` tables the PRINCIPAL
//! and the ACTION halves of the same key-policy decision, and
//! `s3_sse_kms_service_principal_refusal_wording` tables the wording over the reasons a policy
//! withholds the action outright; neither scopes a grant by a value the fetch itself carries,
//! which is the only thing a served row here can be served by. The edge harness is the shared one
//! (`cloudfront_oac_s3_origin_edge`); the KMS calls and the SSE-KMS write are local, since the
//! suites that have them keep theirs private.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_oac_s3_origin_edge::{
    awaited_deployment, call, create_bucket, create_distribution, create_origin_access_control,
    policy, put_bucket_policy, regional_endpoint, view, Answer, CLOUDFRONT_PRINCIPAL, REGION,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every viewer
/// request under it reaches the origin, so every answer below is that fetch's own.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The `ServerSideEncryption` value whose data key belongs to a KMS key, and the two headers an
/// object write names it and its key with
/// (`smithy_query(query="operation",operation="PutObject")` binds `ServerSideEncryption` to
/// `x-amz-server-side-encryption` and `SSEKMSKeyId` to
/// `x-amz-server-side-encryption-aws-kms-key-id`).
const SSE_KMS: &str = "aws:kms";
const ALGORITHM_HEADER: &str = "x-amz-server-side-encryption";
const KEY_ID_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The code S3 refuses a read nothing in force granted with, and the TEXT Amazon S3 "Error
/// responses" publishes for it — the whole of what a refused service-principal call reads.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";

/// The status that code carries ("403 Forbidden", same published list).
const ACCESS_DENIED_STATUS: u16 = 403;

/// The IAM action a read that serves an SSE-KMS object's bytes performs on the object's key, and
/// the opening of the sentence AWS words such a refusal to an IAM identity in — neither of which
/// belongs in the answer a viewer at another service's edge reads.
const DECRYPT_ACTION: &str = "kms:Decrypt";
const NOT_AUTHORIZED: &str = "is not authorized to perform:";

/// The encryption-context ENTRY NAME S3 binds an object's data key under (the user-guide section
/// quoted in the module docs), which is what `kms:EncryptionContextKeys` carries for such a read.
const ARN_CONTEXT_ENTRY: &str = "aws:s3:arn";

/// An account id that is not the one this emulator serves, for a grant scoped to another
/// account's resources.
const ANOTHER_ACCOUNT: &str = "111111111111";

/// The object every row's viewer asks for, and the sibling stored beside it under the SAME key:
/// two objects whose data keys are bound to their own ARNs, so one encryption context names one of
/// them and not the other.
const FETCHED_KEY: &str = "reports/current.csv";
const SIBLING_KEY: &str = "reports/sibling.csv";

/// The bytes each of them holds. A key policy is about who may have an object's plaintext, not
/// about what is in it, so one payload per object serves every row — and they differ from each
/// other, so a served answer cannot be the wrong object's.
const FETCHED_BODY: &str = "the object the grant names";
const SIBLING_BODY: &str = "the object beside it";

/// CloudFront, Amazon S3 and KMS mounted on one `engine` exactly as the router mounts them: the
/// distribution and its origin access control are CloudFront's, the bucket and the object's
/// encryption are S3's, and the key and its policy are KMS's.
///
/// The shared harness mounts the first two ([`super::cloudfront_oac_s3_origin_edge::emulator`]);
/// the key an SSE-KMS object is stored under lives in the third, so this suite mounts all three.
/// Request authentication is off, as it is for every setup plane in this test crate: a call
/// carries a credential scope, which is what names the Region, and a VIEWER carries none at all.
fn emulator_with_kms(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(engine),
    ));
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(engine)),
    );
    router
}

/// One KMS call over the shared engine. KMS is an awsJson1_1 service reached by
/// `X-Amz-Target: TrentService.<Operation>`.
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answer {
    let host = format!("kms.{REGION}.amazonaws.com");
    let target = format!("TrentService.{operation}");
    call(
        router,
        "kms",
        &host,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// `kms:CreateKey`, answering the new key's ARN — the customer-managed key an origin object is
/// stored under, created with KMS's own operation rather than planted in a store.
async fn created_key(router: &MultiServiceRouter, case: &str) -> String {
    let created = kms(
        router,
        "CreateKey",
        &json!({"Description": "the CMK an origin object is stored under"}),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: CreateKey must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let payload: Value = serde_json::from_slice(&created.body).unwrap_or(Value::Null);
    let arn = payload["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !arn.is_empty(),
        "case {case:?}: CreateKey reports no key ARN: {:?}",
        created.text()
    );
    arn
}

/// `kms:PutKeyPolicy`, replacing the key's own document with the one the row is about.
async fn put_key_policy(router: &MultiServiceRouter, case: &str, key_arn: &str, document: &str) {
    let stored = kms(
        router,
        "PutKeyPolicy",
        &json!({"KeyId": key_arn, "PolicyName": "default", "Policy": document}),
    )
    .await;
    assert_eq!(
        stored.status,
        200,
        "case {case:?}: PutKeyPolicy must succeed, got {} with {:?}",
        stored.status,
        stored.text()
    );
}

/// `s3:PutObject` storing `key` under SSE-KMS with `key_arn`, asserting the write really recorded
/// that envelope: a row whose object was stored under any other encryption would ask the key
/// nothing, and every served answer below would say nothing about the key policy.
async fn put_sse_kms_object(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    key: &str,
    body: &str,
    key_arn: &str,
) {
    let written = call(
        router,
        "s3",
        &regional_endpoint(),
        "PUT",
        &format!("/{bucket}/{key}"),
        &[(ALGORITHM_HEADER, SSE_KMS), (KEY_ID_HEADER, key_arn)],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "case {case:?}: PutObject({key:?}) must succeed, got {} with {:?}",
        written.status,
        written.text()
    );
    assert_eq!(
        written.header(ALGORITHM_HEADER),
        SSE_KMS,
        "case {case:?}: expected {key:?} stored under {SSE_KMS}, got {:?}",
        written.header(ALGORITHM_HEADER)
    );
    assert_eq!(
        written.header(KEY_ID_HEADER),
        key_arn,
        "case {case:?}: expected {key:?} stored under the row's own key, got {:?}",
        written.header(KEY_ID_HEADER)
    );
}

/// The account this emulator serves, named the way a resource policy names a whole account: the
/// principal a key's DEFAULT policy delegates to, which every document below keeps so the key
/// stays manageable and the account's own writes go through.
fn account_root() -> String {
    format!("arn:aws:iam::{}:root", engine::emulator_account_id())
}

/// The `Enable IAM User Permissions` statement a key's default policy carries.
fn root_statement() -> Value {
    json!({
        "Sid": "Enable IAM User Permissions",
        "Effect": "Allow",
        "Principal": {"AWS": account_root()},
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// A key policy of [`root_statement`] plus the one statement the row is about.
fn key_policy(grant: Value) -> String {
    json!({"Version": "2012-10-17", "Statement": [root_statement(), grant]}).to_string()
}

/// The grant a row scopes: the CloudFront service principal, the decrypt a read performs, and the
/// `Condition` whose values decide which fetches it governs.
fn edge_grant(condition: Value) -> Value {
    json!({
        "Effect": "Allow",
        "Principal": {"Service": CLOUDFRONT_PRINCIPAL},
        "Action": DECRYPT_ACTION,
        "Resource": "*",
        "Condition": condition,
    })
}

/// The ARN of an S3 object: the bucket's own ARN, which carries neither Region nor account because
/// a bucket name is globally unique, with the key as the rest of the resource part.
fn object_arn(bucket: &str, key: &str) -> String {
    format!("arn:aws:s3:::{bucket}/{key}")
}

/// The ARN of the bucket itself — the resource an object stored under an S3 Bucket Key would have
/// its data key bound to instead.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// The regional endpoint name of an AWS service, which is the value `kms:ViaService` carries for a
/// cryptographic operation that service performed as part of serving a call.
fn endpoint(service: &str) -> String {
    format!("{service}.{REGION}.amazonaws.com")
}

/// What the viewer must be answered for one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The origin's object, byte for byte.
    Served,
    /// S3's own refusal: 403 `AccessDenied` / "Access Denied", with nothing of the principal, the
    /// key or the KMS action in it.
    Refused,
}

/// The values a row's `Condition` can be written over, resolved once its bucket and its
/// distribution exist.
struct Scope {
    bucket: String,
    distribution_account: String,
}

impl Scope {
    /// The encryption context entry of the object the viewer asks for.
    fn fetched_object_arn(&self) -> String {
        object_arn(&self.bucket, FETCHED_KEY)
    }
}

/// One row: a bucket holding two SSE-KMS objects under one customer-managed key, a distribution
/// over it under an origin access control, the one key-policy statement that can admit the fetch,
/// and what the viewer must then be answered for each of the two objects.
struct Row {
    case: &'static str,
    /// This row's own bucket, so one row's grants cannot admit another's fetch.
    bucket: &'static str,
    /// The `Condition` of the grant, built from the values the row scopes it by.
    condition: fn(&Scope) -> Value,
    /// What the viewer must be answered for [`FETCHED_KEY`], and for [`SIBLING_KEY`].
    fetched: Expected,
    sibling: Expected,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the grant is scoped to the encryption context of the object asked for",
            bucket: "edge-key-context-this-object",
            condition: |scope| {
                json!({"StringEquals": {
                    format!("kms:EncryptionContext:{ARN_CONTEXT_ENTRY}"): scope
                        .fetched_object_arn(),
                }})
            },
            fetched: Expected::Served,
            sibling: Expected::Refused,
        },
        Row {
            case: "the grant is scoped to the BUCKET's context, which neither object is bound to",
            bucket: "edge-key-context-the-bucket",
            condition: |scope| {
                json!({"StringEquals": {
                    format!("kms:EncryptionContext:{ARN_CONTEXT_ENTRY}"): bucket_arn(&scope.bucket),
                }})
            },
            fetched: Expected::Refused,
            sibling: Expected::Refused,
        },
        Row {
            case: "the grant is scoped by the context ENTRY NAME S3 binds",
            bucket: "edge-key-context-entry-name",
            condition: |_scope| {
                json!({"ForAnyValue:StringEquals": {
                    "kms:EncryptionContextKeys": ARN_CONTEXT_ENTRY,
                }})
            },
            fetched: Expected::Served,
            sibling: Expected::Served,
        },
        Row {
            case: "the grant is scoped by kms:ViaService naming this service's endpoint",
            bucket: "edge-key-via-this-service",
            condition: |_scope| json!({"StringEquals": {"kms:ViaService": endpoint("s3")}}),
            fetched: Expected::Served,
            sibling: Expected::Served,
        },
        Row {
            case: "the grant is scoped by kms:ViaService naming ANOTHER service's endpoint",
            bucket: "edge-key-via-another-service",
            condition: |_scope| json!({"StringEquals": {"kms:ViaService": endpoint("dynamodb")}}),
            fetched: Expected::Refused,
            sibling: Expected::Refused,
        },
        Row {
            case: "the grant is scoped by the resource account the fetch acts for",
            bucket: "edge-key-source-account",
            condition: |scope| {
                json!({"StringEquals": {"aws:SourceAccount": scope.distribution_account.clone()}})
            },
            fetched: Expected::Served,
            sibling: Expected::Served,
        },
        Row {
            case: "the grant is scoped by ANOTHER account's resources",
            bucket: "edge-key-other-source-account",
            condition: |_scope| json!({"StringEquals": {"aws:SourceAccount": ANOTHER_ACCOUNT}}),
            fetched: Expected::Refused,
            sibling: Expected::Refused,
        },
    ]
}

/// What one row left behind for the viewer phase.
struct Prepared {
    case: &'static str,
    bucket: &'static str,
    key_arn: String,
    domain: String,
    id: String,
    fetched: Expected,
    sibling: Expected,
}

/// Assert one viewer answer against what the row expects of it.
fn assert_answer(
    case: &str,
    object: &str,
    expected: Expected,
    answered: &Answer,
    body: &str,
    key_arn: &str,
) {
    match expected {
        Expected::Served => {
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: expected {object:?} through the edge, got {} with {:?}",
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.text(),
                body,
                "case {case:?}: expected the ORIGIN's own bytes for {object:?}, got {:?}",
                answered.text()
            );
        }
        Expected::Refused => {
            assert_eq!(
                answered.status,
                ACCESS_DENIED_STATUS,
                "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} for {object:?}, got {} with \
                 {:?}",
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.element("Code"),
                ACCESS_DENIED,
                "case {case:?}: expected S3's own {ACCESS_DENIED} code for {object:?}, got {:?}",
                answered.text()
            );
            assert_eq!(
                answered.element("Message"),
                ACCESS_DENIED_MESSAGE,
                "case {case:?}: expected {object:?} refused with the message \
                 {ACCESS_DENIED_MESSAGE:?}, got {:?}",
                answered.element("Message")
            );
            for leaked in [key_arn, DECRYPT_ACTION, CLOUDFRONT_PRINCIPAL, NOT_AUTHORIZED] {
                assert!(
                    !answered.text().contains(leaked),
                    "case {case:?}: a refusal of a service principal's call must not report \
                     {leaked:?}, got {:?}",
                    answered.text()
                );
            }
            assert!(
                !answered.text().contains(body),
                "case {case:?}: a refused read must not carry {object:?}'s bytes, got {:?}",
                answered.text()
            );
        }
    }
}

#[tokio::test]
async fn an_edge_fetch_is_decided_by_the_values_it_carries_and_refused_in_s3s_own_wording() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_with_kms(&engine);

    let mut prepared: Vec<Prepared> = Vec::new();
    for (index, row) in rows().into_iter().enumerate() {
        create_bucket(&router, row.case, row.bucket).await;
        let key_arn = created_key(&router, row.case).await;
        // Both objects are written while the key still carries the policy it was created with,
        // which delegates to this account: the write's own `kms:GenerateDataKey` is the account's,
        // not the fetch's.
        for (key, body) in [(FETCHED_KEY, FETCHED_BODY), (SIBLING_KEY, SIBLING_BODY)] {
            put_sse_kms_object(&router, row.case, row.bucket, key, body, &key_arn).await;
        }
        let control = create_origin_access_control(
            &router,
            row.case,
            &format!("edge-key-conditions-{index}"),
            "always",
        )
        .await;
        let (id, domain, arn) = create_distribution(
            &router,
            row.case,
            &format!("edge-key-conditions-reference-{index}"),
            row.bucket,
            &control,
            CACHING_DISABLED,
        )
        .await;
        // Both documents are written AFTER the distribution exists, because each is scoped to a
        // value that distribution's own ARN carries — the order a client has to follow.
        put_bucket_policy(
            &router,
            row.case,
            row.bucket,
            &policy(
                CLOUDFRONT_PRINCIPAL,
                "s3:GetObject",
                &format!("{}/*", bucket_arn(row.bucket)),
                Some(&json!({"StringEquals": {"AWS:SourceArn": arn.clone()}}).to_string()),
            ),
        )
        .await;
        let scope = Scope {
            bucket: row.bucket.to_string(),
            distribution_account: engine::emulator_account_id().to_string(),
        };
        put_key_policy(
            &router,
            row.case,
            &key_arn,
            &key_policy(edge_grant((row.condition)(&scope))),
        )
        .await;
        prepared.push(Prepared {
            case: row.case,
            bucket: row.bucket,
            key_arn,
            domain,
            id,
            fetched: row.fetched,
            sibling: row.sibling,
        });
    }

    for row in &prepared {
        // The configuration has to reach the edge locations before a viewer request is served:
        // what this waits for is that CONDITION, polled through CloudFront's own read.
        awaited_deployment(&router, row.case, &row.id).await;
        for (key, body, expected) in [
            (FETCHED_KEY, FETCHED_BODY, row.fetched),
            (SIBLING_KEY, SIBLING_BODY, row.sibling),
        ] {
            let viewed = view(&router, &row.domain, &format!("/{key}")).await;
            assert_answer(row.case, key, expected, &viewed, body, &row.key_arn);
        }
        // The same two objects are still the account's own to read, whatever the key policy
        // withheld from the fetch: the grants above are about the service principal, so a row
        // whose fetch was refused has refused nothing else.
        for (key, body) in [(FETCHED_KEY, FETCHED_BODY), (SIBLING_KEY, SIBLING_BODY)] {
            let direct = call(
                &router,
                "s3",
                &regional_endpoint(),
                "GET",
                &format!("/{}/{key}", row.bucket),
                &[],
                Bytes::new(),
            )
            .await;
            assert_eq!(
                direct.status,
                200,
                "case {:?}: the account's own read of {key:?} must be served, got {} with {:?}",
                row.case,
                direct.status,
                direct.text()
            );
            assert_eq!(
                direct.text(),
                body,
                "case {:?}: expected the stored bytes of {key:?}, got {:?}",
                row.case,
                direct.text()
            );
        }
    }
}

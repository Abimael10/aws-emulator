// SPDX-License-Identifier: CC0-1.0

//! A client that follows CloudFront's documented contract — poll `GetDistribution` until the
//! distribution is deployed, then dial the `DomainName` it published — reaches a verdict about
//! an SSE-KMS origin object: CloudFront, Amazon S3 and KMS on one engine.
//!
//! RULE: a distribution a create reports is converged to its deployed status by the CLOCK
//! alone — `Distribution$Status` is "the distribution's status … when the status is `Deployed`,
//! the distribution's information is fully propagated to all CloudFront edge locations", so
//! nothing but time is needed to satisfy a poll waiting on it — and from then on the address
//! the create published as `DomainName` answers a viewer that DIALS it with that distribution's
//! own edge: the bytes of an origin object every document deciding the origin-access-control
//! signed fetch admits, and the origin's own refusal (HTTP 403, `AccessDenied`, "Access
//! Denied") for one any of them withholds. Both halves are observable within a bounded number
//! of looks: a client never has to wait on a transition no call will make, nor on an address
//! nothing answers at.
//!
//! What makes an SSE-KMS object the sharp case for this is that serving its bytes needs a
//! second decision beyond the bucket's: the `kms:Decrypt` the origin performs on the
//! customer-managed key the object was stored under, taken for the CloudFront service
//! principal acting for this distribution. So under ONE bucket document and ONE dialled
//! address, rewriting the key's policy alone moves the viewer's answer between the object's
//! bytes and S3's refusal — and the SSE-S3 object stored beside it, whose read asks KMS
//! nothing, is served through every one of those rewrites.
//!
//! Only a cross-service suite can state this: the published address, the deployment status and
//! the edge are CloudFront's, the objects and their encryption are Amazon S3's, the grant is
//! KMS's, and the subject is that a viewer holding nothing but the published name reaches a
//! verdict about all three. What the siblings leave to it:
//! `cloudfront_dialled_edge_address_over_a_private_s3_origin` dials a published address over a
//! bucket document alone and asserts nothing about a key policy;
//! `cloudfront_oac_kms_s3_origin_edge` and `cloudfront_oac_kms_edge_fetch_key_conditions` table
//! the key-policy decision over requests DISPATCHED in process, which says nothing about
//! whether the published name is an address a client can open a connection to. Here the two
//! meet: the decision is read off a real socket, at the address the create published, after the
//! status the documented poll waits on.
//!
//! The beats differ along what the key's policy grants the fetch, which is what the rule
//! generalises over: the account root alone (the shape a key carries when nothing has been
//! granted to the service principal), the decrypt scoped by `StringEquals` to THIS
//! distribution's ARN, the same grant re-scoped to another distribution's ARN, the grant under
//! no condition at all, and the grant under an `ArnLike` pattern that this distribution's ARN
//! matches. They run in one order on one distribution, alternating served and refused, so each
//! beat also states that the rewrite before it took effect at the NEXT fetch rather than being
//! answered from what the edge held.
//!
//! The requests are driven with request authentication off, as every suite in this test crate
//! drives its setup plane: a call carries a credential scope, which is what names the Region.
//! The VIEWER carries no credential at all and does not go through the router: it opens a
//! connection to the published address and speaks HTTP/1.1 on it.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_dialled_edge_address_over_a_private_s3_origin::{dial, is_dialable};
use super::cloudfront_oac_s3_origin_edge::{
    assert_deployed, awaited_deployment, call, create_bucket, create_distribution,
    create_origin_access_control, edge_listener_authority, is_published_edge_address, policy,
    put_bucket_policy, put_object, Answer, CLOUDFRONT_PRINCIPAL, REGION,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each beat's answer is that fetch's own rather
/// than one the edge held from the beat before.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The bucket both objects live in, and the one origin of the one distribution below.
const BUCKET: &str = "dialled-edge-sse-kms-origin";

/// The two objects the bucket holds: one stored under the customer-managed key whose policy
/// each beat rewrites, and one stored under S3's own keys, whose read asks KMS nothing.
const KMS_KEY_OBJECT: &str = "kms/asset.txt";
const KMS_KEY_BODY: &str = "the object whose bytes the key policy decides";
const SSE_S3_OBJECT: &str = "sse-s3/asset.txt";
const SSE_S3_BODY: &str = "the object stored under S3's own keys";

/// The `ServerSideEncryption` values an object is written under, and the two headers a write
/// names the algorithm and the customer key with
/// (`smithy_query(query="operation",operation="PutObject")` binds `ServerSideEncryption` to
/// `x-amz-server-side-encryption` and `SSEKMSKeyId` to
/// `x-amz-server-side-encryption-aws-kms-key-id`).
const SSE_KMS: &str = "aws:kms";
const SSE_S3: &str = "AES256";
const ALGORITHM_HEADER: &str = "x-amz-server-side-encryption";
const KEY_ID_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// The IAM action a read that serves an SSE-KMS object's bytes performs on the object's key.
const DECRYPT_ACTION: &str = "kms:Decrypt";

/// The code and the message Amazon S3 refuses a read nothing in force granted with (Amazon S3
/// "Error responses"), and the status that code carries ("403 Forbidden", same published list).
/// Both texts are entirely fixed: such a refusal names no principal, no key and no action.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// What the edge reports as the outcome of an answer the ORIGIN produced for this request.
const MISS: &str = "Miss from cloudfront";

/// CloudFront, Amazon S3 and KMS mounted on one engine exactly as the router mounts them: the
/// distribution, its origin access control and its published address are CloudFront's, the
/// bucket and the objects' encryption are S3's, and the key and its policy are KMS's.
///
/// The requests the published address accepts are handed back to THIS emulator (the create runs
/// through it, and the listener it opens carries that request's own handle into it), so the
/// origin fetch the edge makes reaches the S3 mounted here and the decrypt it needs reaches
/// this KMS.
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

/// `kms:CreateKey`, answering the new key's ARN — the customer-managed key the origin object is
/// stored under, created through KMS's own operation rather than planted in a store.
async fn created_key(router: &MultiServiceRouter, case: &str) -> String {
    let created = kms(
        router,
        "CreateKey",
        &json!({"Description": "the CMK the origin object is stored under"}),
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
        "case {case:?}: CreateKey reports no key ARN, got {:?}",
        created.text()
    );
    arn
}

/// `kms:PutKeyPolicy`, replacing the key's own document with the one this beat is about.
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

/// The account this emulator serves, named the way a resource policy names a whole account.
fn account_root() -> String {
    format!("arn:aws:iam::{}:root", engine::emulator_account_id())
}

/// The `Enable IAM User Permissions` statement a key's default policy carries, kept by every
/// beat so the key stays manageable and the account's own writes go through.
fn root_statement() -> Value {
    json!({
        "Sid": "Enable IAM User Permissions",
        "Effect": "Allow",
        "Principal": {"AWS": account_root()},
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// A key policy of [`root_statement`] plus whichever grant this beat adds beside it.
fn key_policy(grant: Option<Value>) -> String {
    let statements: Vec<Value> = match grant {
        Some(grant) => vec![root_statement(), grant],
        None => vec![root_statement()],
    };
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// The grant a beat writes: the CloudFront service principal, the decrypt a read of an SSE-KMS
/// object performs, and the `Condition` (when the beat scopes it) that decides which fetches it
/// governs.
fn edge_grant(condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": {"Service": CLOUDFRONT_PRINCIPAL},
        "Action": DECRYPT_ACTION,
        "Resource": "*",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    statement
}

/// A distribution that is not the one this suite creates: the ARN the re-scoped beat binds its
/// grant to.
fn another_distribution_arn() -> String {
    format!(
        "arn:aws:cloudfront::{}:distribution/E2NOTTHISONE",
        engine::emulator_account_id()
    )
}

/// The text of `<name>…</name>` in a dialled answer's body, or the empty string — the members
/// of the origin's own `<Error>` document.
fn element(body: &str, name: &str) -> String {
    body.split_once(&format!("<{name}>"))
        .and_then(|(_before, rest)| rest.split_once(&format!("</{name}>")))
        .map_or(String::new(), |(text, _after)| text.to_string())
}

/// What the viewer dialling the published address must be answered for one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The origin object with these exact bytes, under HTTP 200.
    Object(&'static str),
    /// The origin's own refusal, and none of the object's bytes.
    OriginRefusal,
}

/// One beat: the key policy in force, and what the dialled address then answers for each of the
/// two objects.
struct Beat {
    case: &'static str,
    /// The grant written beside the account-root statement, built from this distribution's own
    /// ARN, or `None` for a policy carrying no grant to the service principal at all.
    grant: fn(&str) -> Option<Value>,
    /// What the viewer must be served for the SSE-KMS object under that policy. The SSE-S3
    /// object beside it is served in every beat, so what it expects is not a column.
    kms_object: Expected,
}

fn beats() -> Vec<Beat> {
    vec![
        Beat {
            case: "the key policy admits the account root and nothing else",
            grant: |_arn| None,
            kms_object: Expected::OriginRefusal,
        },
        Beat {
            case: "the decrypt is granted to the service principal for this distribution's ARN",
            grant: |arn| {
                Some(edge_grant(Some(
                    json!({"StringEquals": {"AWS:SourceArn": arn}}),
                )))
            },
            kms_object: Expected::Object(KMS_KEY_BODY),
        },
        Beat {
            case: "the same grant re-scoped to ANOTHER distribution's ARN",
            grant: |_arn| {
                Some(edge_grant(Some(
                    json!({"StringEquals": {"AWS:SourceArn": another_distribution_arn()}}),
                )))
            },
            kms_object: Expected::OriginRefusal,
        },
        Beat {
            case: "the decrypt is granted to the service principal under no condition at all",
            grant: |_arn| Some(edge_grant(None)),
            kms_object: Expected::Object(KMS_KEY_BODY),
        },
        Beat {
            case: "the grant is scoped by an ArnLike pattern this distribution's ARN matches",
            grant: |arn| {
                let pattern = match arn.rsplit_once('/') {
                    Some((prefix, _id)) => format!("{prefix}/*"),
                    None => arn.to_string(),
                };
                Some(edge_grant(Some(
                    json!({"ArnLike": {"AWS:SourceArn": pattern}}),
                )))
            },
            kms_object: Expected::Object(KMS_KEY_BODY),
        },
    ]
}

/// Assert one dialled answer against what the beat expects of the object it asked for.
fn assert_answer(case: &str, object: &str, expected: Expected, status: u16, body: &str, raw: &str) {
    match expected {
        Expected::Object(bytes) => {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the origin's object {object:?} at the published \
                 address, got {status} with {body:?}"
            );
            assert_eq!(
                body, bytes,
                "case {case:?}: expected the ORIGIN's own bytes for {object:?}, got {body:?}"
            );
        }
        Expected::OriginRefusal => {
            assert_eq!(
                status, ACCESS_DENIED_STATUS,
                "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} for {object:?}, got {status} \
                 with {body:?}"
            );
            assert_eq!(
                element(body, "Code"),
                ACCESS_DENIED,
                "case {case:?}: expected the origin's own {ACCESS_DENIED} document for \
                 {object:?}, got {body:?}"
            );
            assert_eq!(
                element(body, "Message"),
                ACCESS_DENIED_MESSAGE,
                "case {case:?}: expected the message {ACCESS_DENIED_MESSAGE:?} for {object:?}, \
                 got {:?}",
                element(body, "Message")
            );
            for leaked in [KMS_KEY_BODY, SSE_S3_BODY] {
                assert!(
                    !body.contains(leaked),
                    "case {case:?}: a refused request for {object:?} must carry none of the \
                     bucket's bytes, got {body:?}"
                );
            }
        }
    }
    // Whatever the answer is, it is the EDGE's: CloudFront identifies the request it served,
    // which is what says the dialled address is this distribution's data plane rather than some
    // other listener that happened to accept the connection.
    assert!(
        raw.to_ascii_lowercase().contains("x-amz-cf-id"),
        "case {case:?}: expected the edge's own X-Amz-Cf-Id on the answer for {object:?}, got \
         {raw:?}"
    );
}

#[tokio::test]
async fn a_dialled_published_address_reaches_a_verdict_on_an_sse_kms_origin_object() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_with_kms(&engine);
    let setup = "the distribution over an SSE-KMS origin";

    create_bucket(&router, setup, BUCKET).await;
    let key_arn = created_key(&router, setup).await;
    // Both objects are written while the key still carries the policy it was created with,
    // which delegates to this account: the write's own data-key call is the account's, not the
    // fetch's.
    put_object(
        &router,
        setup,
        BUCKET,
        KMS_KEY_OBJECT,
        KMS_KEY_BODY,
        &[(ALGORITHM_HEADER, SSE_KMS), (KEY_ID_HEADER, &key_arn)],
    )
    .await;
    put_object(
        &router,
        setup,
        BUCKET,
        SSE_S3_OBJECT,
        SSE_S3_BODY,
        &[(ALGORITHM_HEADER, SSE_S3)],
    )
    .await;

    let control =
        create_origin_access_control(&router, setup, "dialled-edge-sse-kms", "always").await;
    let (id, domain, arn) = create_distribution(
        &router,
        setup,
        "dialled-edge-sse-kms-reference",
        BUCKET,
        &control,
        CACHING_DISABLED,
    )
    .await;
    // The published value is the address a client on this machine reaches the edge at — or
    // there is nothing for a viewer to dial at all.
    assert!(
        is_published_edge_address(&domain),
        "case {setup:?}: expected an address this emulator answers on (host:port) as the \
         published DomainName, got {domain:?}"
    );
    let address = edge_listener_authority(&engine, &id).unwrap_or_else(|| {
        panic!("case {setup:?}: this emulator opened no address for the distribution {id}")
    });
    assert!(
        is_dialable(&address),
        "case {setup:?}: expected a host:port a client on this machine can dial, got {address:?}"
    );
    // The bucket's own document is written AFTER the distribution exists, because it is scoped
    // to that distribution's ARN — the order a client has to follow. It admits BOTH objects, so
    // the key policy is the only thing the beats below vary.
    put_bucket_policy(
        &router,
        setup,
        BUCKET,
        &policy(
            CLOUDFRONT_PRINCIPAL,
            "s3:GetObject",
            &format!("arn:aws:s3:::{BUCKET}/*"),
            Some(&json!({"StringEquals": {"AWS:SourceArn": arn.clone()}}).to_string()),
        ),
    )
    .await;

    // The first half of the documented contract: the status the client's poll waits on is
    // reached by the clock alone, within a bounded number of looks and with no further call
    // made to bring it about. Everything after this depends on it.
    awaited_deployment(&router, setup, &id).await;
    assert_deployed(&router, setup, &id).await;

    for beat in beats() {
        put_key_policy(
            &router,
            beat.case,
            &key_arn,
            &key_policy((beat.grant)(&arn)),
        )
        .await;
        for (object, expected) in [
            (KMS_KEY_OBJECT, beat.kms_object),
            // The object stored under S3's own keys asks KMS nothing, so no rewrite of the key
            // policy can decide it: it is served in every beat, which is what says a refusal
            // above is the KEY's and not the address falling over.
            (SSE_S3_OBJECT, Expected::Object(SSE_S3_BODY)),
        ] {
            let answered = dial(beat.case, &address, &domain, &format!("/{object}")).await;
            assert_answer(
                beat.case,
                object,
                expected,
                answered.status,
                &answered.body,
                &answered.raw,
            );
            if matches!(expected, Expected::Object(_)) {
                assert_eq!(
                    answered.header("x-cache"),
                    MISS,
                    "case {:?}: expected the answer for {object:?} to be the origin's, got \
                     X-Cache {:?}",
                    beat.case,
                    answered.header("x-cache")
                );
            }
        }
    }
}

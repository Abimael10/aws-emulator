// SPDX-License-Identifier: CC0-1.0

//! How Amazon S3 WORDS a refusal of a read another AWS service made as its own service
//! principal, across CloudFront, Amazon S3 and KMS on one engine.
//!
//! RULE: a refusal S3 answers a call another AWS service made as its own SERVICE PRINCIPAL —
//! an origin fetch a distribution's origin access control signed — carries S3's own published
//! `AccessDenied` text and nothing else: no principal, no resource and no action, because
//! there is no IAM identity of a caller's to name in one and whoever reads that answer (the
//! viewer at the edge) is not the principal the decision was taken against. A caller
//! presenting a credential of its OWN is refused in the sentence the deciding service words
//! about that identity, which names it, the resource and the action.
//!
//! The refusal this suite drives is the one a read of an SSE-KMS object needs beside the
//! bucket's: serving such an object's bytes performs `kms:Decrypt` on the customer-managed key
//! it was stored under, so a key policy that does not admit the fetch's principal refuses it
//! inside KMS and S3 answers the viewer. Only a cross-service suite can pin it: the grant is
//! written in KMS, the object and its encryption are S3's, the distribution and its origin
//! access control are CloudFront's, and the subject is how S3 words what the three of them
//! decided.
//!
//! The rows span WHY the key refuses the fetch — a policy naming no CloudFront principal at
//! all, a grant scoped to another distribution's ARN, a grant naming another service
//! principal, a grant of another KMS action than the decrypt a read performs, and an explicit
//! `Deny` beside an allow — because the wording is the same whichever of them withheld the
//! bytes. The last leg is the other half of the rule over ONE key: its policy admits the
//! CloudFront principal for this distribution and denies the account the same action, so the
//! very same object is served through the edge and refused to the account's own direct read in
//! KMS's own sentence.
//!
//! `cloudfront_oac_kms_s3_origin_edge.rs` tables WHICH of those grants serves the fetch at all
//! (and asserts nothing about the wording); the S3 crate's own `sse_kms_decrypt_authorization`
//! suite tables the same key-policy decision for a caller presenting a credential of its own.
//!
//! The requests are driven through the assembled router with request authentication off, as
//! every suite in this test crate drives it: what each setup call carries is a credential
//! scope, which is what names the Region. The VIEWER requests carry no credential at all,
//! which is what makes the fetch's own identity the subject. Assembling the whole router costs
//! stack (see `boot_serve.rs`), so it is built — and the requests driven — on a sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for, and so the Region the origin host names and the
/// keys are resolved in.
const REGION: &str = "us-east-1";

/// CloudFront's control-plane host, and the API version every route is under.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";
const API_VERSION: &str = "2020-05-31";

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each answer is that fetch's own.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The service principal an origin access control makes CloudFront's origin request as, and so
/// the principal a bucket policy and a key policy grant to admit it.
const CLOUDFRONT_PRINCIPAL: &str = "cloudfront.amazonaws.com";

/// The code S3 refuses a read nothing granted with, and the TEXT Amazon S3 "Error responses"
/// publishes for it — the whole of what a refused service-principal call reads.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";

/// The status that code carries ("403 Forbidden", same published list).
const ACCESS_DENIED_STATUS: u16 = 403;

/// The IAM action a read that serves an SSE-KMS object's bytes is authorized under, and the
/// opening of the sentence AWS words such a refusal to an IAM identity in.
const DECRYPT_ACTION: &str = "kms:Decrypt";
const NOT_AUTHORIZED: &str = "is not authorized to perform:";

/// The account every resource below belongs to, named the way a resource policy names a whole
/// account: the principal a key's default policy delegates to.
const ACCOUNT_ROOT: &str = "arn:aws:iam::000000000000:root";

/// `Distribution$Status` once the clock has propagated a configuration to the edge locations.
const DEPLOYED: &str = "Deployed";

/// How long [`await_deployed`] keeps looking, and how long it waits between looks: a generous
/// deadline on a condition the emulator's clock owes, not a measurement of how fast it is.
const DEPLOY_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What one call answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    /// A response header's value, or the empty string.
    fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    /// The body as JSON (`Null` when it is empty or not JSON), for the awsJson services.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or(Value::Null)
        }
    }

    /// The body as text — an object's bytes, or an error document.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The text of `<name>…</name>` in the body, or the empty string.
    fn element(&self, name: &str) -> String {
        let body = self.text();
        body.split_once(&format!("<{name}>"))
            .and_then(|(_before, rest)| rest.split_once(&format!("</{name}>")))
            .map_or(String::new(), |(text, _after)| text.to_string())
    }
}

/// The assembled emulator every call below is driven through, and the runtime it is driven on:
/// held as one value, so no call carries both.
struct Emulator {
    router: MultiServiceRouter,
    runtime: tokio::runtime::Runtime,
}

/// Dispatch one request to `host`, carrying the credential scope `signing_name` names when it
/// names one and nothing at all when it does not — which is how a VIEWER reaches an edge.
fn dispatch(
    router: &Emulator,
    signing_name: Option<&str>,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: bytes::Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host);
    if let Some(signing_name) = signing_name {
        // The scope names the access key id, the service and the Region a handler builds its
        // ARNs in. The signature itself is not computed: this router runs with authentication
        // off.
        builder = builder.header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(body).expect("a well-formed request");
    let response = router.runtime.block_on(router.router.dispatch(request));
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// The PATH-STYLE S3 endpoint of this Region, which carries no bucket in its host: the address
/// a call naming its bucket in the PATH has to use.
fn regional_endpoint() -> String {
    format!("s3.{REGION}.amazonaws.com")
}

/// The host a bucket of this Region is addressed at VIRTUAL-HOSTED style, which is what an S3
/// origin's `DomainName` carries.
fn origin_domain(bucket: &str) -> String {
    format!("{bucket}.s3.{REGION}.amazonaws.com")
}

/// One S3 call, addressed path-style and signed with the account's own credential scope.
fn s3(router: &Emulator, method: &str, path: &str, headers: &[(&str, &str)], body: bytes::Bytes) -> Answer {
    dispatch(
        router,
        Some("s3"),
        &regional_endpoint(),
        method,
        path,
        headers,
        body,
    )
}

/// One CloudFront control-plane call.
fn cloudfront(router: &Emulator, method: &str, path: &str, body: bytes::Bytes) -> Answer {
    dispatch(
        router,
        Some("cloudfront"),
        CLOUDFRONT_HOST,
        method,
        path,
        &[("content-type", "application/xml")],
        body,
    )
}

/// One `kms` awsJson1_1 call.
fn kms(router: &Emulator, target: &str, body: &Value) -> Answer {
    dispatch(
        router,
        Some("kms"),
        &format!("kms.{REGION}.amazonaws.com"),
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target),
        ],
        bytes::Bytes::from(body.to_string()),
    )
}

/// `kms:CreateKey`, answering the new key's ARN.
fn create_key(router: &Emulator, case: &str) -> String {
    let created = kms(
        router,
        "TrentService.CreateKey",
        &json!({ "Description": "the CMK an origin object is stored under" }),
    );
    assert_eq!(
        created.status,
        200,
        "case {case:?}: CreateKey must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let arn = created.json()["KeyMetadata"]["Arn"]
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

/// `kms:PutKeyPolicy`, replacing the key's own document.
fn put_key_policy(router: &Emulator, case: &str, key_arn: &str, policy: &str) {
    let stored = kms(
        router,
        "TrentService.PutKeyPolicy",
        &json!({ "KeyId": key_arn, "PolicyName": "default", "Policy": policy }),
    );
    assert_eq!(
        stored.status,
        200,
        "case {case:?}: PutKeyPolicy must succeed, got {} with {:?}",
        stored.status,
        stored.text()
    );
}

/// `s3:CreateBucket`.
fn create_bucket(router: &Emulator, case: &str, bucket: &str) {
    let created = s3(router, "PUT", &format!("/{bucket}"), &[], bytes::Bytes::new());
    assert_eq!(
        created.status,
        200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
}

/// `s3:PutBucketEncryption`, making SSE-KMS under `key_arn` the bucket's default — the
/// configuration an origin bucket holding encrypted objects carries.
fn put_bucket_encryption(router: &Emulator, case: &str, bucket: &str, key_arn: &str) {
    let configuration = format!(
        "<ServerSideEncryptionConfiguration><Rule>\
         <ApplyServerSideEncryptionByDefault>\
         <SSEAlgorithm>aws:kms</SSEAlgorithm><KMSMasterKeyID>{key_arn}</KMSMasterKeyID>\
         </ApplyServerSideEncryptionByDefault></Rule>\
         </ServerSideEncryptionConfiguration>"
    );
    let stored = s3(
        router,
        "PUT",
        &format!("/{bucket}?encryption"),
        &[("content-type", "application/xml")],
        bytes::Bytes::from(configuration),
    );
    assert!(
        (200..300).contains(&stored.status),
        "case {case:?}: PutBucketEncryption must succeed, got {} with {:?}",
        stored.status,
        stored.text()
    );
}

/// `s3:PutObject` under the bucket's own default encryption, asserting the write recorded the
/// SSE-KMS envelope whose key policy the rows are about.
fn put_object(router: &Emulator, case: &str, bucket: &str, key: &str, body: &str) {
    let written = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        &[],
        bytes::Bytes::from(body.to_string()),
    );
    assert_eq!(
        written.status,
        200,
        "case {case:?}: PutObject({key:?}) must succeed, got {} with {:?}",
        written.status,
        written.text()
    );
    assert_eq!(
        written.header("x-amz-server-side-encryption"),
        "aws:kms",
        "case {case:?}: expected the object stored under SSE-KMS, got {:?}",
        written.header("x-amz-server-side-encryption")
    );
}

/// `s3:PutBucketPolicy` granting the CloudFront principal the read of every object of `bucket`
/// for the distribution `distribution_arn` names — the document that makes the origin fetch
/// admitted by the BUCKET, so what remains to refuse it is the key policy alone.
fn grant_the_bucket(router: &Emulator, case: &str, bucket: &str, distribution_arn: &str) {
    let document = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\
         \"Sid\":\"AllowCloudFrontServicePrincipal\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{CLOUDFRONT_PRINCIPAL}\"}},\
         \"Action\":\"s3:GetObject\",\"Resource\":\"arn:aws:s3:::{bucket}/*\",\
         \"Condition\":{{\"StringEquals\":{{\"AWS:SourceArn\":\"{distribution_arn}\"}}}}}}]}}"
    );
    let stored = s3(
        router,
        "PUT",
        &format!("/{bucket}?policy"),
        &[("content-type", "application/json")],
        bytes::Bytes::from(document),
    );
    assert!(
        (200..300).contains(&stored.status),
        "case {case:?}: PutBucketPolicy must succeed, got {} with {:?}",
        stored.status,
        stored.text()
    );
}

/// `cloudfront:CreateOriginAccessControl` for an `s3` origin signed on every request, answering
/// the control's id.
fn create_origin_access_control(router: &Emulator, case: &str, name: &str) -> String {
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/origin-access-control"),
        bytes::Bytes::from(format!(
            "<OriginAccessControlConfig \
             xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>{name}</Name><SigningProtocol>sigv4</SigningProtocol>\
             <SigningBehavior>always</SigningBehavior>\
             <OriginAccessControlOriginType>s3</OriginAccessControlOriginType>\
             </OriginAccessControlConfig>"
        )),
    );
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateOriginAccessControl must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    created.element("Id")
}

/// `cloudfront:CreateDistribution` over the S3 origin `bucket` with `control` attached,
/// answering the distribution's `(id, DomainName, ARN)`.
fn create_distribution(
    router: &Emulator,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    control: &str,
) -> (String, String, String) {
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        bytes::Bytes::from(format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{caller_reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items><Origin>\
             <Id>origin-bucket</Id><DomainName>{}</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
             <OriginAccessControlId>{control}</OriginAccessControlId>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
             </DefaultCacheBehavior>\
             <Comment>oac over an encrypted s3 origin</Comment>\
             <Enabled>true</Enabled></DistributionConfig>",
            origin_domain(bucket)
        )),
    );
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain, arn) = (
        created.element("Id"),
        created.element("DomainName"),
        created.element("ARN"),
    );
    for (member, value) in [("Id", &id), ("DomainName", &domain), ("ARN", &arn)] {
        assert!(
            !value.is_empty(),
            "case {case:?}: the created distribution reports no {member}: {:?}",
            created.text()
        );
    }
    (id, domain, arn)
}

/// Look at `GetDistribution` until the clock has deployed `id`, or until the deadline passes:
/// the configuration has to reach the edge locations before a viewer request is served, and
/// what this waits for is that CONDITION.
fn await_deployed(router: &Emulator, case: &str, id: &str) {
    let started = Instant::now();
    loop {
        let read = cloudfront(
            router,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            bytes::Bytes::new(),
        );
        let status = read.element("Status");
        if status == DEPLOYED {
            return;
        }
        assert!(
            started.elapsed() < DEPLOY_DEADLINE,
            "case {case:?}: the distribution never reached {DEPLOYED}, last reported \
             {status:?} with {:?}",
            read.text()
        );
        router
            .runtime
            .block_on(async { tokio::time::sleep(POLL_INTERVAL).await });
    }
}

/// A key policy's statement allowing the ACCOUNT's own principals every KMS action — the
/// `Enable IAM User Permissions` statement a key's DEFAULT policy carries, which every document
/// below keeps so the key stays manageable and the account's own writes go through.
fn root_statement() -> String {
    format!(
        "{{\"Sid\":\"Enable IAM User Permissions\",\"Effect\":\"Allow\",\
         \"Principal\":{{\"AWS\":\"{ACCOUNT_ROOT}\"}},\"Action\":\"kms:*\",\"Resource\":\"*\"}}"
    )
}

/// A key policy of [`root_statement`] plus the statements a row adds.
fn key_policy(statements: &[String]) -> String {
    let mut all = vec![root_statement()];
    all.extend(statements.iter().cloned());
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{}]}}",
        all.join(",")
    )
}

/// One key-policy statement about a SERVICE principal and `actions`, with the condition clause
/// the row names.
fn service_statement(
    effect: &str,
    principal: &str,
    actions: &str,
    condition: Option<&str>,
) -> String {
    let condition = condition
        .map(|clause| format!(",\"Condition\":{clause}"))
        .unwrap_or_default();
    format!(
        "{{\"Effect\":\"{effect}\",\"Principal\":{{\"Service\":\"{principal}\"}},\
         \"Action\":{actions},\"Resource\":\"*\"{condition}}}"
    )
}

/// One key-policy statement about the ACCOUNT and `actions`.
fn account_statement(effect: &str, actions: &str) -> String {
    format!(
        "{{\"Effect\":\"{effect}\",\"Principal\":{{\"AWS\":\"{ACCOUNT_ROOT}\"}},\
         \"Action\":{actions},\"Resource\":\"*\"}}"
    )
}

/// The clause scoping a grant to one distribution's ARN, which is what `AWS:SourceArn` carries
/// for an origin fetch that distribution's origin access control signed.
fn scoped_to(arn: &str) -> String {
    format!("{{\"StringEquals\":{{\"AWS:SourceArn\":\"{arn}\"}}}}")
}

/// One row: a bucket, an SSE-KMS object under its own CMK, a distribution over it under an
/// origin access control, and the key-policy statements that leave the fetch REFUSED.
struct Row {
    case: &'static str,
    /// This row's own bucket, so one row's grants cannot admit another's read.
    bucket: &'static str,
    key: &'static str,
    body: &'static str,
    /// The statements beside [`root_statement`], built from the distribution's own ARN once it
    /// is known; an empty list leaves the key the policy it was created with.
    statements: fn(&str) -> Vec<String>,
}

/// The table: one row per reason the object's key withholds the decrypt from the fetch.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a key policy naming no CloudFront principal at all",
            bucket: "wording-kms-default-policy",
            key: "reports/default-policy.csv",
            body: "the sse-kms object",
            statements: |_arn| Vec::new(),
        },
        Row {
            case: "a grant scoped to ANOTHER distribution's ARN",
            bucket: "wording-kms-other-distribution",
            key: "other-distribution/plain.txt",
            body: "not for this distribution",
            statements: |_arn| {
                vec![service_statement(
                    "Allow",
                    CLOUDFRONT_PRINCIPAL,
                    "\"kms:Decrypt\"",
                    Some(&scoped_to(
                        "arn:aws:cloudfront::000000000000:distribution/E2NOTTHISONE",
                    )),
                )]
            },
        },
        Row {
            case: "a grant naming another service principal",
            bucket: "wording-kms-other-principal",
            key: "other-principal/plain.txt",
            body: "granted to someone else",
            statements: |_arn| {
                vec![service_statement(
                    "Allow",
                    "lambda.amazonaws.com",
                    "\"kms:Decrypt\"",
                    None,
                )]
            },
        },
        Row {
            case: "a grant of another KMS action than the decrypt a read performs",
            bucket: "wording-kms-other-action",
            key: "other-action/plain.txt",
            body: "encrypt but not decrypt",
            statements: |arn| {
                vec![service_statement(
                    "Allow",
                    CLOUDFRONT_PRINCIPAL,
                    "[\"kms:Encrypt\",\"kms:GenerateDataKey*\"]",
                    Some(&scoped_to(arn)),
                )]
            },
        },
        Row {
            case: "an explicit Deny of the CloudFront principal beside the allow",
            bucket: "wording-kms-explicit-deny",
            key: "explicit-deny/plain.txt",
            body: "denied outright",
            statements: |arn| {
                vec![
                    service_statement(
                        "Allow",
                        CLOUDFRONT_PRINCIPAL,
                        "\"kms:Decrypt\"",
                        Some(&scoped_to(arn)),
                    ),
                    service_statement("Deny", CLOUDFRONT_PRINCIPAL, "\"kms:Decrypt\"", None),
                ]
            },
        },
    ]
}

/// What one row left behind for the viewer phase.
struct Prepared {
    case: &'static str,
    key: &'static str,
    body: &'static str,
    key_arn: String,
    domain: String,
    id: String,
}

/// Everything one bucket+object+distribution setup answers back.
struct Origin {
    key_arn: String,
    id: String,
    domain: String,
    arn: String,
}

/// Create the bucket, its CMK, its SSE-KMS object, an origin access control and a distribution
/// over the bucket, and grant the bucket's read to the CloudFront principal for that
/// distribution: everything a row needs before its KEY policy decides the fetch.
fn prepare_origin(
    router: &Emulator,
    case: &str,
    index: usize,
    bucket: &str,
    key: &str,
    body: &str,
) -> Origin {
    create_bucket(router, case, bucket);
    let key_arn = create_key(router, case);
    put_bucket_encryption(router, case, bucket, &key_arn);
    // Written while the key still carries the policy it was created with, which delegates to
    // this account: the write's own `kms:GenerateDataKey` is the account's, not the fetch's.
    put_object(router, case, bucket, key, body);
    let control = create_origin_access_control(router, case, &format!("wording-oac-{index}"));
    let (id, domain, arn) =
        create_distribution(router, case, &format!("wording-reference-{index}"), bucket, &control);
    // Both documents are written AFTER the distribution exists, because each is scoped to its
    // own ARN — the order a client has to follow.
    grant_the_bucket(router, case, bucket, &arn);
    Origin {
        key_arn,
        id,
        domain,
        arn,
    }
}

/// Assert that `answered` is S3's own refusal, worded as S3 words one for a call that presents
/// no IAM identity of a caller's: the published code and text, and none of the principal, the
/// key or the action the decision was about.
fn assert_s3s_own_refusal(case: &str, answered: &Answer, key_arn: &str, body: &str) {
    assert_eq!(
        answered.status,
        ACCESS_DENIED_STATUS,
        "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} through the edge, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.element("Code"),
        ACCESS_DENIED,
        "case {case:?}: expected S3's own {ACCESS_DENIED} code, got {:?}",
        answered.text()
    );
    assert_eq!(
        answered.element("Message"),
        ACCESS_DENIED_MESSAGE,
        "case {case:?}: expected the refusal worded {ACCESS_DENIED_MESSAGE:?}, got {:?}",
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
        "case {case:?}: a refused read must not carry the object's bytes, got {:?}",
        answered.text()
    );
}

#[test]
fn s3_words_a_service_principals_refusal_in_its_own_published_text() {
    std::thread::Builder::new()
        .name("s3-service-principal-refusal-wording".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut assembled = build_router(engine);
            assembled.set_request_authentication(false);
            let router = Emulator {
                router: assembled,
                runtime: tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    // The emulator opens a core-owned listener for every distribution it
                    // creates — a distribution's edge is served at the address it publishes —
                    // so the runtime a create runs on needs the IO driver that socket is bound
                    // on.
                    .enable_io()
                    .build()
                    .expect("a current-thread runtime"),
            };

            let mut prepared: Vec<Prepared> = Vec::new();
            for (index, row) in rows().into_iter().enumerate() {
                let origin =
                    prepare_origin(&router, row.case, index, row.bucket, row.key, row.body);
                let statements = (row.statements)(&origin.arn);
                if !statements.is_empty() {
                    put_key_policy(&router, row.case, &origin.key_arn, &key_policy(&statements));
                }
                prepared.push(Prepared {
                    case: row.case,
                    key: row.key,
                    body: row.body,
                    key_arn: origin.key_arn,
                    domain: origin.domain,
                    id: origin.id,
                });
            }

            for row in &prepared {
                await_deployed(&router, row.case, &row.id);
                let viewed = dispatch(
                    &router,
                    None,
                    &row.domain,
                    "GET",
                    &format!("/{}", row.key),
                    &[],
                    bytes::Bytes::new(),
                );
                assert_s3s_own_refusal(row.case, &viewed, &row.key_arn, row.body);
            }

            // The other half of the rule, over ONE key: its policy admits the CloudFront
            // principal for this distribution and DENIES the account the same action, so the
            // same object is served through the edge while the account's own direct read is
            // refused in the sentence KMS words about that identity — which names the
            // principal, the key and the action, exactly as the service principal's refusal
            // must not.
            let case = "the key admits the fetch and denies the account";
            let origin = prepare_origin(
                &router,
                case,
                prepared.len(),
                "wording-kms-account-denied",
                "account-denied/plain.txt",
                "served through the edge alone",
            );
            put_key_policy(
                &router,
                case,
                &origin.key_arn,
                &key_policy(&[
                    service_statement(
                        "Allow",
                        CLOUDFRONT_PRINCIPAL,
                        "\"kms:Decrypt\"",
                        Some(&scoped_to(&origin.arn)),
                    ),
                    account_statement("Deny", "\"kms:Decrypt\""),
                ]),
            );
            await_deployed(&router, case, &origin.id);

            let viewed = dispatch(
                &router,
                None,
                &origin.domain,
                "GET",
                "/account-denied/plain.txt",
                &[],
                bytes::Bytes::new(),
            );
            assert_eq!(
                viewed.status,
                200,
                "case {case:?}: expected the origin's object through the edge, got {} with {:?}",
                viewed.status,
                viewed.text()
            );
            assert_eq!(
                viewed.text(),
                "served through the edge alone",
                "case {case:?}: expected the ORIGIN's own bytes, got {:?}",
                viewed.text()
            );

            let direct = s3(
                &router,
                "GET",
                "/wording-kms-account-denied/account-denied/plain.txt",
                &[],
                bytes::Bytes::new(),
            );
            assert_eq!(
                direct.status,
                ACCESS_DENIED_STATUS,
                "case {case:?}: the account's own read must be refused, got {} with {:?}",
                direct.status,
                direct.text()
            );
            assert_eq!(
                direct.element("Code"),
                ACCESS_DENIED,
                "case {case:?}: expected S3's own {ACCESS_DENIED} code, got {:?}",
                direct.text()
            );
            let message = direct.element("Message");
            for named in [NOT_AUTHORIZED, DECRYPT_ACTION, origin.key_arn.as_str()] {
                assert!(
                    message.contains(named),
                    "case {case:?}: a caller refused on its OWN identity reads the sentence \
                     naming {named:?}, got {message:?}"
                );
            }
        })
        .expect("a sized thread")
        .join()
        .expect("the suite thread");
}

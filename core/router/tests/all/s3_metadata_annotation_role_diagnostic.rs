// SPDX-License-Identifier: CC0-1.0

//! The diagnostic `GetBucketMetadataConfiguration` reports for an ANNOTATION table that could
//! not be filled through the IAM role it names.
//!
//! The rule under test: **the `ErrorMessage` of a table-provisioning failure is the service's
//! own fixed diagnostic for that error class — the same text whatever the request named and
//! whatever the emulator holds, since it interpolates nothing from either — so it names the
//! permissions the role has to grant, the service principal its trust policy has to admit, the
//! KMS permission and key state a bucket encrypted with a customer key needs, and what it takes
//! to replace the table.** A paraphrase is a divergence: an operator, and any automation
//! matching on the message, reads what to grant out of that string.
//!
//! The rows span what the rule generalises over rather than one recorded call: the FAULT in the
//! role (a role this account's IAM holds no record of, and a role whose trust policy admits
//! another service principal), the CALL that provisioned the table (the create that names a
//! whole configuration and the update that enables the table on one), and the bucket the
//! configuration belongs to — none of which may change one character of the text. A role S3
//! Metadata really can assume is the row that reports no error at all, so the text is not
//! emitted unconditionally.
//!
//! Two crates have to agree for that to hold — S3 provisions the table and IAM holds the role
//! the provisioning asks about — so the suite lives here rather than in either of them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, elements, form_encode, opt_element};

/// The Region every request here is signed for, and so the endpoint it addresses.
const REGION: &str = "us-east-1";

/// IAM's own awsQuery API version, the `Version` the setup call carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// The XML namespace S3's model declares.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The role name every row's annotation table configuration names.
const ROLE: &str = "metadata-annotations";

/// The service principal a role has to admit for S3 Metadata to fill a table through it, and
/// another service's principal — the one a wrong trust policy admits instead.
const METADATA_PRINCIPAL: &str = "metadata.s3.amazonaws.com";
const ANOTHER_PRINCIPAL: &str = "s3.amazonaws.com";

/// The `TableStatus` of a table that could not be created, and of one being created.
const FAILED: &str = "FAILED";
const CREATING: &str = "CREATING";

/// The containers a configuration's reader reports its tables in.
const JOURNAL: &str = "JournalTableConfigurationResult";
const ANNOTATION: &str = "AnnotationTableConfigurationResult";

/// The `TableStatus` a table reports once it has come into service.
const ACTIVE: &str = "ACTIVE";

/// How long a row waits for the journal table to come into service before it gives up:
/// generous, since what it waits for is a span the store records and a loaded machine may take
/// much longer to get there.
const SETTLE_BUDGET: Duration = Duration::from_secs(60);

/// How often it looks again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The diagnostic S3 reports for an annotation role that cannot be used, spelled out here so
/// the suite pins the wire text rather than reading it out of the code under test.
const ANNOTATION_ROLE_MESSAGE: &str = "The annotation role does not have sufficient \
     permissions. Ensure the role grants s3:GetObjectAnnotation, \
     s3:GetObjectVersionAnnotation, s3:ListBucket, and s3:ListBucketVersions on the bucket, \
     and that the role's trust policy allows metadata.s3.amazonaws.com to assume it. If the \
     bucket or its objects/annotations are encrypted with a KMS key, ensure the role also has \
     kms:Decrypt permission. Additionally, ensure that the KMS key used to encrypt the bucket \
     still exists, is active and has a resource policy granting access to the S3 service \
     principals. To create a new S3 Metadata table, you must delete the S3 Metadata \
     configuration for this bucket, and then create a new S3 Metadata configuration.";

/// S3 beside the IAM the role resolution asks, on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here is signed: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region and the caller's access key id.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One answer: the status and the body as text.
struct Answer {
    status: u16,
    body: String,
}

/// One S3 request, addressed path-style to the Regional endpoint.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3"))
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// One awsQuery call to IAM.
async fn iam(router: &MultiServiceRouter, body: String) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// Hold the role [`ROLE`] under a trust document admitting `principal`.
async fn create_role(router: &MultiServiceRouter, case: &str, principal: &str) {
    let trust = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    );
    let created = iam(
        router,
        format!(
            "Action=CreateRole&Version={IAM_API_VERSION}&RoleName={ROLE}\
             &AssumeRolePolicyDocument={}",
            form_encode(&trust)
        ),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateRole({ROLE:?}) must succeed, got {} with {}",
        created.status, created.body
    );
}

/// The ARN of that role in the account this emulator serves.
fn role_arn() -> String {
    format!("arn:aws:iam::{}:role/{ROLE}", ::engine::EMULATOR_ACCOUNT_ID)
}

/// `PUT /<bucket>` — the general purpose bucket a configuration is created for.
async fn create_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let created = s3(router, "PUT", &format!("/{bucket}"), b"").await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the bucket must be created, got {} with {}",
        created.status, created.body
    );
}

/// `POST /<bucket>?metadataConfiguration` — `CreateBucketMetadataConfiguration` with a journal
/// table and an annotation table in `state`, naming `role` when it names one.
async fn create_configuration(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    state: &str,
    role: Option<&str>,
) {
    let named = role.map_or_else(String::new, |role| format!("<Role>{role}</Role>"));
    let body = format!(
        "<MetadataConfiguration xmlns=\"{XMLNS}\"><JournalTableConfiguration>\
         <RecordExpiration><Expiration>ENABLED</Expiration><Days>7</Days></RecordExpiration>\
         </JournalTableConfiguration><AnnotationTableConfiguration>\
         <ConfigurationState>{state}</ConfigurationState>{named}\
         </AnnotationTableConfiguration></MetadataConfiguration>"
    );
    let created = s3(
        router,
        "POST",
        &format!("/{bucket}?metadataConfiguration"),
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the create must answer the modelled 200, got {} with {}",
        created.status, created.body
    );
}

/// `PUT /<bucket>?metadataAnnotationTable` —
/// `UpdateBucketMetadataAnnotationTableConfiguration` enabling the annotation table under
/// `role`.
async fn enable_annotation_table(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    role: &str,
) {
    let body = format!(
        "<AnnotationTableConfiguration xmlns=\"{XMLNS}\">\
         <ConfigurationState>ENABLED</ConfigurationState><Role>{role}</Role>\
         </AnnotationTableConfiguration>"
    );
    let updated = s3(
        router,
        "PUT",
        &format!("/{bucket}?metadataAnnotationTable"),
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: the update must answer the modelled 200, got {} with {}",
        updated.status, updated.body
    );
}

/// `GET /<bucket>?metadataConfiguration` — `GetBucketMetadataConfiguration`.
async fn read_configuration(router: &MultiServiceRouter, case: &str, bucket: &str) -> String {
    let read = s3(
        router,
        "GET",
        &format!("/{bucket}?metadataConfiguration"),
        b"",
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the read of the configuration must answer 200, got {} with {}",
        read.status, read.body
    );
    read.body
}

/// The `TableStatus` the container `container` of `body` reports, when it reports one.
fn status_of(body: &str, container: &str) -> Option<String> {
    opt_element(block_of(body, container)?, "TableStatus")
}

/// The `ErrorMessage` the annotation table result of `body` carries, when it carries one.
fn annotation_error_message(body: &str) -> Option<String> {
    let fragment = block_of(body, ANNOTATION)?;
    elements(fragment, "ErrorMessage").into_iter().next()
}

/// Wait until the JOURNAL table of `bucket` is in service, which is what an update of another
/// table of the same configuration waits for. The CONDITION is polled, never a fixed pause.
async fn wait_for_the_journal_table(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        let body = read_configuration(router, case, bucket).await;
        if status_of(&body, JOURNAL).as_deref() == Some(ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the journal table never came into service: {body}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Which call provisions the annotation table, which is the other half of what the rule
/// generalises over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provisioned {
    /// The create that names a whole configuration with the table enabled.
    ByTheCreate,
    /// A create leaving it disabled, and the update that enables it.
    ByTheUpdate,
}

/// One row: what IAM holds for the role the table names, and whether the table is provisioned.
struct Row {
    case: &'static str,
    /// The service principal IAM holds the role's trust policy for, or `None` for a row whose
    /// role IAM holds no record of at all.
    trusts: Option<&'static str>,
    /// Whether the table is provisioned: `true` is a table that reports no error, `false` one
    /// recorded `FAILED` carrying this class's diagnostic.
    provisioned: bool,
}

/// The rows: the two faults that leave the table unprovisioned, and the role that does not.
fn rows() -> [Row; 3] {
    [
        Row {
            case: "a role this account's IAM holds no record of",
            trusts: None,
            provisioned: false,
        },
        Row {
            case: "a role whose trust policy admits another service principal",
            trusts: Some(ANOTHER_PRINCIPAL),
            provisioned: false,
        },
        Row {
            case: "a role whose trust policy admits the metadata service principal",
            trusts: Some(METADATA_PRINCIPAL),
            provisioned: true,
        },
    ]
}

#[tokio::test]
async fn an_unusable_annotation_role_is_reported_with_the_services_own_diagnostic() {
    for provisioned_by in [Provisioned::ByTheCreate, Provisioned::ByTheUpdate] {
        for (index, row) in rows().into_iter().enumerate() {
            let case = format!("{} provisioned {provisioned_by:?}", row.case);
            let router = build_router();
            // The bucket's name differs per row, and the text may not: it interpolates
            // nothing from the request or from what the emulator holds.
            let by = match provisioned_by {
                Provisioned::ByTheCreate => "create",
                Provisioned::ByTheUpdate => "update",
            };
            let bucket = format!("annotation-role-{by}-{index}");
            create_bucket(&router, &case, &bucket).await;
            if let Some(principal) = row.trusts {
                create_role(&router, &case, principal).await;
            }

            match provisioned_by {
                Provisioned::ByTheCreate => {
                    create_configuration(&router, &case, &bucket, "ENABLED", Some(&role_arn()))
                        .await;
                }
                Provisioned::ByTheUpdate => {
                    create_configuration(&router, &case, &bucket, "DISABLED", None).await;
                    wait_for_the_journal_table(&router, &case, &bucket).await;
                    enable_annotation_table(&router, &case, &bucket, &role_arn()).await;
                }
            }

            let body = read_configuration(&router, &case, &bucket).await;
            let status = status_of(&body, ANNOTATION);
            // A table that CAN be provisioned reports a status of a table being made or in
            // service — which of the two depends only on how much of the span the store
            // recorded has passed, so neither is the behaviour under test; one that cannot
            // reports `FAILED` from the provisioning call onwards.
            let acceptable: &[&str] = match row.provisioned {
                true => &[CREATING, ACTIVE],
                false => &[FAILED],
            };
            assert!(
                status
                    .as_deref()
                    .is_some_and(|status| acceptable.contains(&status)),
                "case {case:?}: expected the annotation TableStatus to be one of \
                 {acceptable:?}, got {status:?} in {body}"
            );
            let message = annotation_error_message(&body);
            let expected = (!row.provisioned).then_some(ANNOTATION_ROLE_MESSAGE);
            assert_eq!(
                message.as_deref(),
                expected,
                "case {case:?}: expected the ErrorMessage {expected:?}, got {message:?}"
            );
        }
    }
}

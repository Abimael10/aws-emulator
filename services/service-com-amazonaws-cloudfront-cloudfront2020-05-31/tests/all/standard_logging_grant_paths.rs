// SPDX-License-Identifier: CC0-1.0

//! THE LOG-DELIVERY GRANT, over every call that stores a logging configuration — end to end
//! over HTTP, with Amazon S3 mounted beside CloudFront.
//!
//! The rule under test: standard logging is delivered into the bucket a distribution's
//! logging configuration names, as the log-delivery account rather than as the caller, so
//! whenever a configuration that ENABLES logging to an Amazon S3 bucket becomes a
//! distribution's stored configuration, that bucket's access control list ends up carrying a
//! grant of `FULL_CONTROL` to the log-delivery account's canonical user — beside the grants
//! the bucket already had, the owner's own among them. A client reading the bucket's ACL
//! therefore finds exactly two grants: the owner's and the log-delivery account's.
//!
//! The rule belongs to the CONFIGURATION, not to one operation, so the rows below span the
//! calls that store one: the create, the create that carries tags, the update, the copy that
//! takes the primary's configuration, the promotion that moves a staging configuration onto
//! its primary, and the create of a streaming distribution, whose own `Logging` names its
//! bucket the same way. Each row also varies what the rule generalises over: the FORM of the
//! bucket's endpoint host (the global one and a regional one), whether the bucket's name
//! carries dots, whether a prefix is named, and whether cookies are logged.
//!
//! Every row RESETS the bucket's ACL to the owner's own grant immediately before the call
//! under test, so what the assertion sees is the grant THAT call left — not one an earlier
//! call in the arrangement had already written.
//!
//! [`crate::standard_logging_bucket`] owns the harness (CloudFront and the object store over
//! one engine, and the bucket reads) and the two answers a submitted configuration earns;
//! this suite reuses both rather than standing up a second emulator.

use aws_sdk_s3::types::{BucketCannedAcl, Permission};

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::LOG_DELIVERY_CANONICAL_USER_ID;

use crate::distribution_lifecycle::{call, call_with_headers, Answer, API_VERSION};
use crate::standard_logging_bucket::{acl_access_not_enabled, Emulator, Ownership, OTHER_REGION};

// ---------------------------------------------------------------------------
// The bucket under test
// ---------------------------------------------------------------------------

/// The log bucket one row logs to, and how its endpoint host is spelled.
#[derive(Debug, Clone)]
struct Target {
    bucket: &'static str,
    /// The Region the bucket's endpoint host addresses, or `None` for the GLOBAL host form
    /// (`<bucket>.s3.amazonaws.com`), which addresses none.
    host_region: Option<&'static str>,
    /// `LoggingConfig$Prefix` — "an optional string that you want CloudFront to prefix to
    /// the access log filenames".
    prefix: &'static str,
    /// `LoggingConfig$IncludeCookies`, which a streaming distribution's logging has no member
    /// for.
    include_cookies: bool,
}

impl Target {
    /// `LoggingConfig$Bucket`: "The Amazon S3 bucket to store the access logs in, for
    /// example, `amzn-s3-demo-bucket.s3.amazonaws.com`" — the bucket's endpoint host.
    fn host(&self) -> String {
        match self.host_region {
            Some(region) => format!("{}.s3.{region}.amazonaws.com", self.bucket),
            None => format!("{}.s3.amazonaws.com", self.bucket),
        }
    }
}

/// The permission the log-delivery grant carries, as Amazon S3 spells it on the wire.
fn full_control() -> String {
    Permission::FullControl.as_str().to_string()
}

/// The canonical user id Amazon S3 reports as the bucket's OWNER, which is the account that
/// created it — the id the owner's own grant names.
async fn bucket_owner(emulator: &Emulator, bucket: &str) -> String {
    emulator
        .s3()
        .get_bucket_acl()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| panic!("reading the ACL of {bucket} must succeed, got {error:?}"))
        .owner()
        .and_then(|owner| owner.id())
        .unwrap_or_default()
        .to_string()
}

/// Put `bucket`'s ACL back to the owner's own `FULL_CONTROL` and nothing else, through S3's
/// own API.
///
/// The canned `private` ACL is exactly that — "Owner gets `FULL_CONTROL`. No one else has
/// access rights" — so a bucket reset this way is one a client has taken the log-delivery
/// grant off again.
async fn reset_acl(emulator: &Emulator, bucket: &str) {
    emulator
        .s3()
        .put_bucket_acl()
        .bucket(bucket)
        .acl(BucketCannedAcl::Private)
        .send()
        .await
        .unwrap_or_else(|error| panic!("resetting the ACL of {bucket} must succeed, got {error:?}"));
    let grants = emulator.acl_grants(bucket).await;
    let owner = bucket_owner(emulator, bucket).await;
    assert_eq!(
        grants,
        vec![(owner, full_control())],
        "resetting the ACL of {bucket} must leave the owner's grant alone, got {grants:?}"
    );
}

// ---------------------------------------------------------------------------
// The configurations, and the calls that store them
// ---------------------------------------------------------------------------

/// A `DistributionConfig` document carrying one origin, one default cache behaviour and —
/// where `logging` says so — a `Logging` element, in the order `DistributionConfig` declares
/// its members. `staging` appends the `Staging` member, the last one it declares.
fn distribution_config(
    reference: &str,
    logging: Option<(bool, &Target)>,
    staging: bool,
) -> String {
    let logging = match logging {
        Some((enabled, target)) => format!(
            "<Logging><Enabled>{enabled}</Enabled>\
             <IncludeCookies>{}</IncludeCookies>\
             <Bucket>{}</Bucket><Prefix>{}</Prefix></Logging>",
            target.include_cookies,
            target.host(),
            target.prefix
        ),
        None => String::new(),
    };
    let staging = if staging {
        "<Staging>true</Staging>"
    } else {
        ""
    };
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin><Id>primary</Id>\
         <DomainName>origin.example.com</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <Comment>log-delivery grant</Comment>{logging}\
         <PriceClass>PriceClass_All</PriceClass><Enabled>true</Enabled>{staging}\
         </DistributionConfig>"
    )
}

/// A `StreamingDistributionConfig` document, in the order the model declares its members.
///
/// `StreamingLoggingConfig`'s three members are all required, so a configuration that logs
/// nowhere states `Enabled` `false` rather than leaving the element's members out.
fn streaming_config(reference: &str, logging: Option<(bool, &Target)>) -> String {
    let logging = match logging {
        Some((enabled, target)) => format!(
            "<Logging><Enabled>{enabled}</Enabled><Bucket>{}</Bucket>\
             <Prefix>{}</Prefix></Logging>",
            target.host(),
            target.prefix
        ),
        None => String::new(),
    };
    format!(
        "<StreamingDistributionConfig \
          xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <S3Origin><DomainName>media.s3.amazonaws.com</DomainName>\
         <OriginAccessIdentity></OriginAccessIdentity></S3Origin>\
         <Comment>log-delivery grant</Comment>{logging}\
         <TrustedSigners><Enabled>false</Enabled><Quantity>0</Quantity></TrustedSigners>\
         <Enabled>true</Enabled></StreamingDistributionConfig>"
    )
}

/// `CreateDistribution`.
async fn create(emulator: &Emulator, config: &str) -> Answer {
    call(
        &emulator.router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(config),
        None,
    )
    .await
}

/// `CreateDistributionWithTags`, carrying the configuration and one tag.
async fn create_with_tags(emulator: &Emulator, config: &str) -> Answer {
    let body = format!(
        "<DistributionConfigWithTags \
          xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">{}<Tags><Items>\
         <Tag><Key>suite</Key><Value>log-delivery-grant</Value></Tag></Items></Tags>\
         </DistributionConfigWithTags>",
        config.replacen(
            &format!(" xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\""),
            "",
            1
        )
    );
    call(
        &emulator.router,
        "POST",
        &format!("/{API_VERSION}/distribution?WithTags"),
        Some(&body),
        None,
    )
    .await
}

/// `UpdateDistribution` on `id`, under the version `etag`.
async fn update(emulator: &Emulator, id: &str, config: &str, etag: &str) -> Answer {
    call(
        &emulator.router,
        "PUT",
        &format!("/{API_VERSION}/distribution/{id}/config"),
        Some(config),
        Some(etag),
    )
    .await
}

/// `CopyDistribution` on `primary_id`, which creates a staging distribution from the
/// primary's stored configuration.
async fn copy(emulator: &Emulator, primary_id: &str, reference: &str) -> Answer {
    call_with_headers(
        &emulator.router,
        "POST",
        &format!("/{API_VERSION}/distribution/{primary_id}/copy"),
        Some(&format!(
            "<CopyDistributionRequest \
              xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{reference}</CallerReference></CopyDistributionRequest>"
        )),
        &[("staging", "true")],
    )
    .await
}

/// `UpdateDistributionWithStagingConfig`: the staging distribution's configuration onto its
/// primary. The `If-Match` states both current versions, "`<primary ETag>, <staging ETag>`".
async fn promote(
    emulator: &Emulator,
    primary_id: &str,
    staging_id: &str,
    primary_etag: &str,
    staging_etag: &str,
) -> Answer {
    call(
        &emulator.router,
        "PUT",
        &format!(
            "/{API_VERSION}/distribution/{primary_id}/promote-staging-config\
             ?StagingDistributionId={staging_id}"
        ),
        None,
        Some(&format!("{primary_etag}, {staging_etag}")),
    )
    .await
}

/// `CreateStreamingDistribution`.
async fn create_streaming(emulator: &Emulator, config: &str) -> Answer {
    call(
        &emulator.router,
        "POST",
        &format!("/{API_VERSION}/streaming-distribution"),
        Some(config),
        None,
    )
    .await
}

/// `ListStreamingDistributions`.
async fn list_streaming(emulator: &Emulator) -> Answer {
    call(
        &emulator.router,
        "GET",
        &format!("/{API_VERSION}/streaming-distribution"),
        None,
        None,
    )
    .await
}

// ---------------------------------------------------------------------------
// The calls that store a logging configuration
// ---------------------------------------------------------------------------

/// One call that leaves a configuration enabling standard logging stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    Create,
    CreateWithTags,
    Update,
    Copy,
    Promote,
    StreamingCreate,
}

impl Path {
    /// The status the call answers when it is accepted: the creates are 201, the update and
    /// the promotion 200 (`SUCCESS_STATUS` of each operation).
    fn accepted_status(self) -> u16 {
        match self {
            Self::Update | Self::Promote => 200,
            _ => 201,
        }
    }
}

/// Arrange whatever this path needs, reset the log bucket's ACL to the owner's own grant, and
/// then make the ONE call that stores a configuration logging to `target`.
///
/// `run` distinguishes the resources one row creates from another's, so a row can be driven
/// twice over the same bucket.
async fn store_logging_config(
    emulator: &Emulator,
    path: Path,
    target: &Target,
    run: usize,
    reset: bool,
) -> Answer {
    let case = format!("{path:?}-{}-{run}", target.bucket);
    let enabled = Some((true, target));
    match path {
        Path::Create => {
            if reset {
                reset_acl(emulator, target.bucket).await;
            }
            create(emulator, &distribution_config(&case, enabled, false)).await
        }
        Path::CreateWithTags => {
            if reset {
                reset_acl(emulator, target.bucket).await;
            }
            create_with_tags(emulator, &distribution_config(&case, enabled, false)).await
        }
        Path::Update => {
            // The distribution starts out logging NOWHERE, so the grant can only come from
            // the update.
            let created = create(emulator, &distribution_config(&case, None, false)).await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected the create to be accepted, got {} and {}",
                created.status, created.raw
            );
            let id = distribution_id(&created, &case);
            let etag = created.header("etag").to_string();
            if reset {
                reset_acl(emulator, target.bucket).await;
            }
            update(
                emulator,
                &id,
                &distribution_config(&case, enabled, false),
                &etag,
            )
            .await
        }
        Path::Copy => {
            // The primary already logs to the bucket, so the copy runs under a configuration
            // that enables logging without naming one itself.
            let created = create(emulator, &distribution_config(&case, enabled, false)).await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected the primary's create to be accepted, got {} and {}",
                created.status, created.raw
            );
            let id = distribution_id(&created, &case);
            if reset {
                reset_acl(emulator, target.bucket).await;
            }
            copy(emulator, &id, &format!("{case}-copy")).await
        }
        Path::Promote => {
            // The primary logs nowhere; the STAGING distribution is the one that logs to the
            // bucket, so the promotion is what makes the primary log there.
            let created = create(emulator, &distribution_config(&case, None, false)).await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected the primary's create to be accepted, got {} and {}",
                created.status, created.raw
            );
            let primary_id = distribution_id(&created, &case);
            let primary_etag = created.header("etag").to_string();

            let staging_reference = format!("{case}-staging");
            let copied = copy(emulator, &primary_id, &staging_reference).await;
            assert_eq!(
                copied.status, 201,
                "case {case}: expected the copy to be accepted, got {} and {}",
                copied.status, copied.raw
            );
            let staging_id = distribution_id(&copied, &case);
            let staged = update(
                emulator,
                &staging_id,
                &distribution_config(&staging_reference, enabled, true),
                copied.header("etag"),
            )
            .await;
            assert_eq!(
                staged.status, 200,
                "case {case}: expected staging the configuration to be accepted, got {} and {}",
                staged.status, staged.raw
            );
            let staging_etag = staged.header("etag").to_string();

            if reset {
                reset_acl(emulator, target.bucket).await;
            }
            promote(
                emulator,
                &primary_id,
                &staging_id,
                &primary_etag,
                &staging_etag,
            )
            .await
        }
        Path::StreamingCreate => {
            if reset {
                reset_acl(emulator, target.bucket).await;
            }
            create_streaming(emulator, &streaming_config(&case, enabled)).await
        }
    }
}

/// The `Id` an accepted create or copy reported.
fn distribution_id(answered: &Answer, case: &str) -> String {
    answered
        .root()
        .find_text("Id")
        .unwrap_or_else(|| panic!("case {case}: expected an Id in {}", answered.raw))
        .to_string()
}

/// The rows: one per call that stores a logging configuration, each against a log bucket of
/// its own and each spelling that bucket's endpoint host its own way.
fn rows() -> Vec<(Path, Target)> {
    vec![
        (
            Path::Create,
            Target {
                bucket: "grant-create-logs",
                host_region: None,
                prefix: "cf-std/",
                include_cookies: false,
            },
        ),
        (
            Path::CreateWithTags,
            Target {
                bucket: "grant.tagged.logs",
                host_region: Some(OTHER_REGION),
                prefix: "",
                include_cookies: true,
            },
        ),
        (
            Path::Update,
            Target {
                bucket: "grant-update-logs",
                host_region: Some(OTHER_REGION),
                prefix: "updated/",
                include_cookies: false,
            },
        ),
        (
            Path::Copy,
            Target {
                bucket: "grant-copy-logs",
                host_region: None,
                prefix: "copied/",
                include_cookies: true,
            },
        ),
        (
            Path::Promote,
            Target {
                bucket: "grant.promoted.logs",
                host_region: None,
                prefix: "",
                include_cookies: false,
            },
        ),
        (
            Path::StreamingCreate,
            Target {
                bucket: "grant-streaming-logs",
                host_region: Some(OTHER_REGION),
                prefix: "rtmp/",
                include_cookies: false,
            },
        ),
    ]
}

/// The ACL `bucket` carries, as a SET of `(canonical user id, permission)` pairs: a grant is
/// a grant whatever order the list reports them in.
async fn grant_set(emulator: &Emulator, bucket: &str) -> Vec<(String, String)> {
    let mut grants = emulator.acl_grants(bucket).await;
    grants.sort();
    grants
}

/// The two grants a client finds on a log bucket once logging is enabled: the bucket owner's
/// own `FULL_CONTROL`, and the log-delivery account's.
async fn expected_grants(emulator: &Emulator, bucket: &str) -> Vec<(String, String)> {
    let mut expected = vec![
        (bucket_owner(emulator, bucket).await, full_control()),
        (LOG_DELIVERY_CANONICAL_USER_ID.to_string(), full_control()),
    ];
    expected.sort();
    expected
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// THE RULE: whichever call stores a configuration enabling standard logging to a bucket, the
/// log-delivery account ends up holding `FULL_CONTROL` on that bucket — so the bucket's ACL
/// reports exactly two grants, the owner's and the log-delivery account's.
#[tokio::test]
async fn every_call_that_stores_a_logging_configuration_grants_the_log_delivery_account() {
    let emulator = Emulator::new();
    for (path, target) in rows() {
        let case = format!("{path:?} logging to {}", target.host());
        emulator
            .bucket(target.bucket, Ownership::BucketOwnerPreferred)
            .await;

        let answered = store_logging_config(&emulator, path, &target, 1, true).await;

        assert_eq!(
            answered.status,
            path.accepted_status(),
            "case {case:?}: expected the call to be accepted {}, got {} and {}",
            path.accepted_status(),
            answered.status,
            answered.raw
        );
        let actual = grant_set(&emulator, target.bucket).await;
        assert_eq!(
            actual,
            expected_grants(&emulator, target.bucket).await,
            "case {case:?}: expected the owner's and the log-delivery account's FULL_CONTROL, \
             got {actual:?}"
        );
    }
}

/// The grant is ADDED to the ACL the bucket already carries, so a second call storing the
/// same logging configuration leaves the log-delivery account holding `FULL_CONTROL` ONCE —
/// the bucket does not collect a grant per distribution.
#[tokio::test]
async fn a_bucket_that_already_carries_the_grant_collects_no_second_one() {
    let emulator = Emulator::new();
    for (path, target) in rows() {
        let case = format!("{path:?} logging to {}", target.host());
        emulator
            .bucket(target.bucket, Ownership::BucketOwnerPreferred)
            .await;

        let first = store_logging_config(&emulator, path, &target, 1, true).await;
        assert_eq!(
            first.status,
            path.accepted_status(),
            "case {case:?}: expected the first call to be accepted, got {} and {}",
            first.status,
            first.raw
        );
        // No reset this time: the grant the first call left is still on the bucket.
        let second = store_logging_config(&emulator, path, &target, 2, false).await;
        assert_eq!(
            second.status,
            path.accepted_status(),
            "case {case:?}: expected the second call to be accepted, got {} and {}",
            second.status,
            second.raw
        );

        let actual = grant_set(&emulator, target.bucket).await;
        assert_eq!(
            actual,
            expected_grants(&emulator, target.bucket).await,
            "case {case:?}: expected the one log-delivery grant beside the owner's, got \
             {actual:?}"
        );
        let granted = emulator.log_delivery_grants(target.bucket).await;
        assert_eq!(
            granted, 1,
            "case {case:?}: expected exactly one log-delivery grant, got {granted}"
        );
    }
}

/// A configuration that enables NO logging names no bucket to grant on, so every one of these
/// calls leaves the bucket's ACL exactly as it was — even against a bucket its `Bucket` member
/// names.
#[tokio::test]
async fn a_configuration_that_enables_no_logging_leaves_the_bucket_alone() {
    let emulator = Emulator::new();
    let target = Target {
        bucket: "no-grant-logs",
        host_region: None,
        prefix: "off/",
        include_cookies: false,
    };
    emulator
        .bucket(target.bucket, Ownership::BucketOwnerPreferred)
        .await;
    let untouched = grant_set(&emulator, target.bucket).await;

    // The create and the copy that follows it: the primary logs nowhere, so neither does the
    // copy that runs under its configuration.
    let created = create(
        &emulator,
        &distribution_config("logging-off-primary", Some((false, &target)), false),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case \"the create\": expected 201, got {} and {}",
        created.status, created.raw
    );
    let primary_id = distribution_id(&created, "the create");
    let primary_etag = created.header("etag").to_string();

    let copied = copy(&emulator, &primary_id, "logging-off-staging").await;
    assert_eq!(
        copied.status, 201,
        "case \"the copy\": expected 201, got {} and {}",
        copied.status, copied.raw
    );
    let staging_id = distribution_id(&copied, "the copy");

    // The promotion of a staging configuration that logs nowhere.
    let staged = update(
        &emulator,
        &staging_id,
        &distribution_config("logging-off-staging", None, true),
        copied.header("etag"),
    )
    .await;
    assert_eq!(
        staged.status, 200,
        "case \"the staged update\": expected 200, got {} and {}",
        staged.status, staged.raw
    );
    let promoted = promote(
        &emulator,
        &primary_id,
        &staging_id,
        &primary_etag,
        staged.header("etag"),
    )
    .await;
    assert_eq!(
        promoted.status, 200,
        "case \"the promotion\": expected 200, got {} and {}",
        promoted.status, promoted.raw
    );

    // The streaming create whose logging is switched off, with the bucket still named.
    let streaming = create_streaming(
        &emulator,
        &streaming_config("logging-off-streaming", Some((false, &target))),
    )
    .await;
    assert_eq!(
        streaming.status, 201,
        "case \"the streaming create\": expected 201, got {} and {}",
        streaming.status, streaming.raw
    );

    let actual = grant_set(&emulator, target.bucket).await;
    assert_eq!(
        actual, untouched,
        "case \"logging switched off\": expected the bucket's ACL to be untouched, got \
         {actual:?}"
    );
}

/// A streaming distribution's logging is judged the same way a distribution's is: a bucket
/// whose ACLs are DISABLED can carry no log-delivery grant, so a configuration enabling
/// logging to one is refused where it is submitted and nothing is created — while the very
/// same configuration against a bucket whose ACLs are enabled is accepted and granted.
#[tokio::test]
async fn a_streaming_configuration_logging_to_a_bucket_with_acls_disabled_is_refused() {
    let emulator = Emulator::new();
    let refused = Target {
        bucket: "rtmp-logs-enforced",
        host_region: None,
        prefix: "rtmp/",
        include_cookies: false,
    };
    let accepted = Target {
        bucket: "rtmp.logs.enabled",
        host_region: Some(OTHER_REGION),
        prefix: "",
        include_cookies: false,
    };
    emulator
        .bucket(refused.bucket, Ownership::BucketOwnerEnforced)
        .await;
    emulator
        .bucket(accepted.bucket, Ownership::ObjectWriter)
        .await;
    let before = grant_set(&emulator, refused.bucket).await;

    let answered = create_streaming(
        &emulator,
        &streaming_config("rtmp-acls-disabled", Some((true, &refused))),
    )
    .await;
    assert_eq!(
        answered.status, 400,
        "case \"ACLs disabled\": expected the create to be refused 400, got {} and {}",
        answered.status, answered.raw
    );
    assert_eq!(
        answered.error_code(),
        "InvalidArgument",
        "case \"ACLs disabled\": expected InvalidArgument, got {:?} in {}",
        answered.error_code(),
        answered.raw
    );
    let message = answered.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message,
        acl_access_not_enabled(&refused.host()),
        "case \"ACLs disabled\": expected the refusal to name the bucket, got {message:?}"
    );
    let after = grant_set(&emulator, refused.bucket).await;
    assert_eq!(
        after, before,
        "case \"ACLs disabled\": expected a refused create to leave the ACL alone, got \
         {after:?}"
    );
    let listed = list_streaming(&emulator).await;
    let quantity = listed.root().find_text("Quantity").unwrap_or_default();
    assert_eq!(
        quantity, "0",
        "case \"ACLs disabled\": expected no streaming distribution to have been created, got \
         {} in {}",
        quantity, listed.raw
    );

    let answered = create_streaming(
        &emulator,
        &streaming_config("rtmp-acls-enabled", Some((true, &accepted))),
    )
    .await;
    assert_eq!(
        answered.status, 201,
        "case \"ACLs enabled\": expected the create to be accepted 201, got {} and {}",
        answered.status, answered.raw
    );
    let actual = grant_set(&emulator, accepted.bucket).await;
    assert_eq!(
        actual,
        expected_grants(&emulator, accepted.bucket).await,
        "case \"ACLs enabled\": expected the owner's and the log-delivery account's \
         FULL_CONTROL, got {actual:?}"
    );
}

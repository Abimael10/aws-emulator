// SPDX-License-Identifier: CC0-1.0

//! The references a cache behaviour makes to resources CloudFront holds APART from the
//! distribution, end to end over HTTP.
//!
//! RULE: a distribution configuration is stored only while every resource of another kind its
//! cache behaviours attach by identifier exists in this account's records — `CachePolicyId` is
//! "the unique identifier of the cache policy that is attached to this cache behavior", and
//! `OriginRequestPolicyId`, `ResponseHeadersPolicyId`, `RealtimeLogConfigArn` and
//! `FieldLevelEncryptionId` name one of their own kind the same way — so a configuration
//! naming one that is not there is refused with the `NoSuch…` error the model declares for
//! THAT kind, at that error's own status and with that kind's own sentence, and nothing is
//! stored; a configuration naming resources that exist is stored and reports every reference
//! back.
//!
//! What the rows span: all five reference members a behaviour carries, each with its own
//! modelled error, status and sentence; identifiers of several shapes nothing holds (a
//! well-formed uuid, a uuid of another namespace, an ARN whose name no configuration answers
//! to); the DEFAULT cache behaviour and an ORDERED one, which are the two places a behaviour
//! can carry a reference; and every path that writes a whole configuration
//! (`CreateDistribution`, `CreateDistributionWithTags` and `UpdateDistribution`). The
//! accepting side creates each resource through its OWN control plane and then reads the
//! stored configuration back, so the reference is resolved against records the service really
//! holds; the empty member — which is what a behaviour that attaches nothing carries once the
//! service has resolved the configuration — attaches nothing and is accepted.
//!
//! Each `NoSuch…` sentence is a LITERAL below rather than the crate's own constant, so a
//! rewording fails this suite. The wire harness, the per-resource creates and the two readers
//! are the sibling suites', reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::connection_mode_immutable::get_distribution_config;
use crate::distribution_lifecycle::{
    create_distribution, emulator, list_distributions, update_distribution, Answer, ACCOUNT_ID,
    API_VERSION,
};
use crate::edge_realtime_logs::{realtime_log_config, EndPoint};
use crate::edge_request_shaping::cache_policy;
use crate::field_level_encryption_config_lifecycle::{create_config, simple};
use crate::field_level_encryption_profile_lifecycle::store_profile;
use crate::lambda_edge_association::create_distribution_with_tags;

/// One reference member a cache behaviour attaches a resource of another kind through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference {
    CachePolicy,
    OriginRequestPolicy,
    ResponseHeadersPolicy,
    RealtimeLogConfig,
    FieldLevelEncryption,
}

impl Reference {
    /// Every reference member a behaviour carries, in the order the two behaviour shapes
    /// declare them (`smithy_query(query="shape",shape="CacheBehavior")`).
    fn all() -> [Self; 5] {
        [
            Self::FieldLevelEncryption,
            Self::RealtimeLogConfig,
            Self::CachePolicy,
            Self::OriginRequestPolicy,
            Self::ResponseHeadersPolicy,
        ]
    }

    /// The element the reference travels in.
    fn member(self) -> &'static str {
        match self {
            Self::CachePolicy => "CachePolicyId",
            Self::OriginRequestPolicy => "OriginRequestPolicyId",
            Self::ResponseHeadersPolicy => "ResponseHeadersPolicyId",
            Self::RealtimeLogConfig => "RealtimeLogConfigArn",
            Self::FieldLevelEncryption => "FieldLevelEncryptionId",
        }
    }

    /// The modelled error code an identifier nothing answers to earns, and its HTTP status —
    /// the error the model declares for this reference on every operation that writes a whole
    /// `DistributionConfig`
    /// (`smithy_query(query="operation",operation="CreateDistribution")`).
    fn refusal(self) -> (&'static str, u16) {
        match self {
            Self::CachePolicy => ("NoSuchCachePolicy", 404),
            Self::OriginRequestPolicy => ("NoSuchOriginRequestPolicy", 404),
            Self::ResponseHeadersPolicy => ("NoSuchResponseHeadersPolicy", 404),
            Self::RealtimeLogConfig => ("NoSuchRealtimeLogConfig", 404),
            Self::FieldLevelEncryption => ("NoSuchFieldLevelEncryptionConfig", 404),
        }
    }

    /// The sentence CloudFront answers that code with, written out as the service emits it.
    fn message(self) -> &'static str {
        match self {
            Self::CachePolicy => "The specified cache policy does not exist.",
            Self::OriginRequestPolicy => "The specified origin request policy does not exist.",
            Self::ResponseHeadersPolicy => "The specified response headers policy does not exist.",
            Self::RealtimeLogConfig => "The specified real-time log configuration does not exist.",
            Self::FieldLevelEncryption => {
                "The specified field level encryption config does not exist."
            }
        }
    }

    /// An identifier of this reference's own kind that no record answers to. Each row carries
    /// a different SHAPE of absent value, so the refusal is pinned to be about the lookup
    /// rather than about how the value reads.
    fn absent(self) -> String {
        match self {
            // A uuid in the shape a minted cache policy id takes, which nothing holds.
            Self::CachePolicy => "0c5e53f3-9d2a-4f27-8a5b-1f4e7f6a2b11".to_string(),
            // The nil uuid, which is well formed and is no policy's.
            Self::OriginRequestPolicy => "00000000-0000-0000-0000-000000000000".to_string(),
            // A uuid differing from the accepted one only in its last group.
            Self::ResponseHeadersPolicy => "7b1c4f90-3e52-4c8a-9d6f-2a0b5c7d8e9f".to_string(),
            // An ARN in the shape this service mints for a real-time log configuration,
            // naming a configuration that was never created.
            Self::RealtimeLogConfig => format!(
                "arn:aws:cloudfront::{ACCOUNT_ID}:realtime-log-config/absent-log-configuration"
            ),
            // A field-level encryption configuration id of this service's own namespace.
            Self::FieldLevelEncryption => "A1B2C3D4E5F6G7".to_string(),
        }
    }
}

/// Create the resource `reference` attaches through its own control plane, and answer the
/// identifier the service minted for it.
///
/// Each one goes through the operation that OWNS that resource, so the identifier the
/// distribution then attaches is one this account really holds.
async fn stored_resource(router: &MultiServiceRouter, reference: Reference) -> String {
    match reference {
        Reference::CachePolicy => {
            cache_policy(router, "attached-cache-policy", "none", &[], 60, false, false).await
        }
        Reference::OriginRequestPolicy => {
            let body = format!(
                "<OriginRequestPolicyConfig \
                 xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                 <Name>attached-origin-request-policy</Name>\
                 <HeadersConfig><HeaderBehavior>none</HeaderBehavior></HeadersConfig>\
                 <CookiesConfig><CookieBehavior>none</CookieBehavior></CookiesConfig>\
                 <QueryStringsConfig><QueryStringBehavior>none</QueryStringBehavior>\
                 </QueryStringsConfig></OriginRequestPolicyConfig>"
            );
            minted_id(
                "OriginRequestPolicy",
                crate::origin_request_policy_lifecycle::create_policy(router, &body).await,
            )
        }
        Reference::ResponseHeadersPolicy => {
            let body = format!(
                "<ResponseHeadersPolicyConfig \
                 xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                 <Name>attached-response-headers-policy</Name></ResponseHeadersPolicyConfig>"
            );
            minted_id(
                "ResponseHeadersPolicy",
                crate::response_headers_policy_lifecycle::create_policy(router, &body).await,
            )
        }
        Reference::RealtimeLogConfig => {
            let (arn, _fields) = realtime_log_config(
                router,
                "attached-log-configuration",
                100,
                &["timestamp"],
                &[EndPoint {
                    role_arn: format!("arn:aws:iam::{ACCOUNT_ID}:role/cf-realtime-log"),
                    stream_arn: format!("arn:aws:kinesis:us-east-1:{ACCOUNT_ID}:stream/cf-logs"),
                }],
            )
            .await;
            arn
        }
        Reference::FieldLevelEncryption => {
            let profile = store_profile(router, "attached-fle-profile").await;
            let config = simple("attached", "attached-fle-config", &profile);
            minted_id(
                "FieldLevelEncryptionConfig",
                create_config(router, &config.xml()).await,
            )
        }
    }
}

/// The identifier a create answered with, or a failure naming the operation that did not
/// answer one.
fn minted_id(operation: &str, created: Answer) -> String {
    assert_eq!(
        created.status, 201,
        "case {operation}: expected HTTP 201 creating the attached resource, got {} with {}",
        created.status, created.raw
    );
    created
        .root()
        .find_text("Id")
        .unwrap_or_else(|| panic!("case {operation}: the create reports no Id: {}", created.raw))
        .to_string()
}

/// Where a configuration carries the reference under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// On `DefaultCacheBehavior`.
    Default,
    /// On one ordered `CacheBehavior`.
    Ordered,
}

/// A complete `DistributionConfig` document under `caller_reference` whose behaviour at
/// `placement` attaches `value` through `reference`'s member.
///
/// `value` is written as the element's text; an EMPTY value is written as the empty element,
/// which is what a behaviour attaching nothing carries.
fn config_xml(
    caller_reference: &str,
    reference: Reference,
    value: &str,
    placement: Placement,
) -> String {
    let attachment = format!("<{0}>{value}</{0}>", reference.member());
    let on = |wanted: Placement| {
        if placement == wanted {
            attachment.as_str()
        } else {
            ""
        }
    };
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin><Id>primary</Id>\
         <DomainName>origin.example.com</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{}\
         </DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>1</Quantity><Items><CacheBehavior>\
         <PathPattern>/images/*</PathPattern><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>https-only</ViewerProtocolPolicy>{}\
         </CacheBehavior></Items></CacheBehaviors>\
         <Comment>attaches {}</Comment><Enabled>true</Enabled></DistributionConfig>",
        on(Placement::Default),
        on(Placement::Ordered),
        reference.member(),
    )
}

/// Assert `answer` is the refusal this reference's kind earns: its modelled code, that code's
/// status, that kind's sentence, and no `Location` for a resource nothing created.
fn assert_absent_reference_refusal(
    case: &str,
    path: &str,
    reference: Reference,
    answer: &Answer,
) {
    let (code, status) = reference.refusal();
    assert_eq!(
        answer.status, status,
        "case {case} ({path}): expected HTTP {status}, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case} ({path}): expected {code}, got {:?} with {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message,
        reference.message(),
        "case {case} ({path}): expected the kind's sentence, got {message:?}"
    );
    assert!(
        answer.header("location").is_empty(),
        "case {case} ({path}): a refusal announces no location, got {:?}",
        answer.header("location")
    );
}

/// How many distributions the account holds.
async fn stored_count(router: &MultiServiceRouter) -> usize {
    list_distributions(router, "")
        .await
        .root()
        .find_text("Quantity")
        .unwrap_or_default()
        .parse()
        .unwrap_or_default()
}

/// Every row of the refused table: each reference member, on each of the two places a
/// behaviour can carry it.
fn refused_rows() -> Vec<(String, Reference, Placement)> {
    let mut rows = Vec::new();
    for reference in Reference::all() {
        for (label, placement) in [
            ("the-default-cache-behaviour", Placement::Default),
            ("an-ordered-cache-behaviour", Placement::Ordered),
        ] {
            rows.push((
                format!("{}-on-{label}", reference.member()),
                reference,
                placement,
            ));
        }
    }
    rows
}

#[tokio::test]
async fn an_attached_resource_that_does_not_exist_is_refused_on_every_create_path() {
    let router = emulator();

    for (index, (case, reference, placement)) in refused_rows().into_iter().enumerate() {
        let absent = reference.absent();
        for (path, answer) in [
            (
                "CreateDistribution",
                create_distribution(
                    &router,
                    &config_xml(
                        &format!("attached-{index}-plain"),
                        reference,
                        &absent,
                        placement,
                    ),
                )
                .await,
            ),
            (
                "CreateDistributionWithTags",
                create_distribution_with_tags(
                    &router,
                    &config_xml(
                        &format!("attached-{index}-tagged"),
                        reference,
                        &absent,
                        placement,
                    ),
                )
                .await,
            ),
        ] {
            assert_absent_reference_refusal(&case, path, reference, &answer);
        }
        // A refused create claims nothing: no distribution, and so no id or caller reference
        // spent on it.
        let held = stored_count(&router).await;
        assert_eq!(
            held, 0,
            "case {case}: expected the account to hold no distribution, got {held}"
        );
    }
}

#[tokio::test]
async fn an_update_cannot_attach_a_resource_that_does_not_exist() {
    let router = emulator();
    // One stored distribution the update rows land on, attaching nothing at all.
    let created = create_distribution(
        &router,
        &config_xml(
            "attached-subject",
            Reference::CachePolicy,
            "",
            Placement::Default,
        ),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case the subject: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    let etag = created.header("etag").to_string();

    for (case, reference, placement) in refused_rows() {
        // An update submits the configuration whole under the distribution's OWN caller
        // reference, which no update may rename.
        let refused = update_distribution(
            &router,
            &id,
            &config_xml(
                "attached-subject",
                reference,
                &reference.absent(),
                placement,
            ),
            Some(&etag),
        )
        .await;
        assert_absent_reference_refusal(&case, "UpdateDistribution", reference, &refused);

        // The refused update left the stored configuration, and its version, exactly as they
        // were: the next row still lands on the version this one read.
        let read = get_distribution_config(&router, &id).await;
        assert_eq!(
            read.header("etag"),
            etag,
            "case {case} (read-back): a refused update mints no new version, got {:?}",
            read.header("etag")
        );
        let attached = read
            .root()
            .text_at(&format!("DefaultCacheBehavior/{}", reference.member()))
            .unwrap_or_default()
            .to_string();
        assert!(
            attached.is_empty(),
            "case {case} (read-back): expected the stored behaviour to attach nothing, got \
             {attached:?}"
        );
    }
}

#[tokio::test]
async fn a_configuration_attaching_resources_that_exist_is_stored_with_every_reference() {
    for reference in Reference::all() {
        for (label, placement) in [
            ("the-default-cache-behaviour", Placement::Default),
            ("an-ordered-cache-behaviour", Placement::Ordered),
        ] {
            let case = format!("{}-on-{label}", reference.member());
            // One emulator per row: each row creates the resource under its own name, and a
            // name is unique per account for every one of these kinds.
            let router = emulator();
            let identifier = stored_resource(&router, reference).await;

            let created = create_distribution(
                &router,
                &config_xml(&format!("attached-{case}"), reference, &identifier, placement),
            )
            .await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected HTTP 201, got {} with {}",
                created.status, created.raw
            );
            let id = created.root().text_at("Id").unwrap_or_default().to_string();

            // The reference the create stored is the one a later read answers with, on the
            // behaviour that carried it.
            let read = get_distribution_config(&router, &id).await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected HTTP 200 from GetDistributionConfig, got {} with {}",
                read.status, read.raw
            );
            let path = match placement {
                Placement::Default => format!("DefaultCacheBehavior/{}", reference.member()),
                Placement::Ordered => format!(
                    "CacheBehaviors/Items/CacheBehavior/{}",
                    reference.member()
                ),
            };
            assert_eq!(
                read.root().text_at(&path),
                Some(identifier.as_str()),
                "case {case}: expected the stored behaviour to attach {identifier:?}, got {:?}",
                read.root().text_at(&path)
            );
        }
    }
}

#[tokio::test]
async fn a_reference_member_that_names_nothing_attaches_nothing() {
    let router = emulator();

    for (index, reference) in Reference::all().into_iter().enumerate() {
        for (label, placement) in [
            ("the-default-cache-behaviour", Placement::Default),
            ("an-ordered-cache-behaviour", Placement::Ordered),
        ] {
            let case = format!("{}-empty-on-{label}", reference.member());
            let created = create_distribution(
                &router,
                &config_xml(
                    &format!("attached-empty-{index}-{label}"),
                    reference,
                    "",
                    placement,
                ),
            )
            .await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected HTTP 201 for a member that names nothing, got {} with {}",
                created.status, created.raw
            );
        }
    }
}

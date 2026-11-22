// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a distribution configuration stating more origins than the per-distribution
//! quota is refused with, and the boundary that refusal holds at, end to end over HTTP.
//!
//! RULE: a configuration whose `Origins` list is longer than the origins published per
//! distribution is refused HTTP 400 `TooManyOrigins` with CloudFront's own sentence for that
//! condition — it names the MEMBER the origins travel in and the scope the quota is published
//! for, and carries nothing of the request, so neither the number of origins submitted, the
//! quota itself nor any origin id can change a character of it (the shape's modelled
//! documentation sentence, which describes the condition instead, is not what the service
//! sends); a configuration AT the quota is stored and reports every origin it submitted back
//! from `GetDistributionConfig`.
//!
//! What the rows span: the counts one, two, nine and half a quota past the bound, so the
//! sentence is pinned to be independent of how far over the request is; both origin forms the
//! model declares for an origin's endpoint (`CustomOriginConfig` and `S3OriginConfig`) and a
//! list mixing them, so the sentence is independent of what the origins ARE; and the counts
//! one, one below the bound and exactly the bound on the accepting side. Each refused row is
//! submitted on all three paths that write a whole configuration: `CreateDistribution`,
//! `CreateDistributionWithTags` and `UpdateDistribution`.
//!
//! The sentence is a LITERAL below rather than the crate's own constant, so a rewording fails
//! this suite. The wire harness is the lifecycle suite's, reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::connection_mode_immutable::get_distribution_config;
use crate::distribution_lifecycle::{
    create_distribution, emulator, list_distributions, update_distribution, Answer, API_VERSION,
};
// `CreateDistributionWithTags` takes the same configuration wrapped in a
// `DistributionConfigWithTags`; the raw-XML call for it is already `pub(crate)` beside the
// Lambda@Edge association suite, so the second create path is driven through that one helper.
use crate::lambda_edge_association::create_distribution_with_tags;

/// The origins ONE distribution's configuration may state: the bound the recorded service
/// admits a configuration up to and refuses one past (`MAX_ORIGINS_PER_DISTRIBUTION`).
const ORIGIN_QUOTA: usize = 100;

/// The sentence CloudFront refuses a configuration past that quota with, written out as the
/// service emits it: fixed text with no variable parts.
const EXPECTED_MESSAGE: &str = "Your request contains more Origins than are allowed per \
     distribution.";

/// How an origin addresses its endpoint. `Origin` declares exactly these two forms —
/// `CustomOriginConfig` and `S3OriginConfig`
/// (`smithy_query(query="shape",shape="com.amazonaws.cloudfront#Origin")`) — and `Mixed`
/// alternates them down the list.
#[derive(Debug, Clone, Copy)]
enum Form {
    Custom,
    S3,
    Mixed,
}

impl Form {
    /// The endpoint element the origin at `index` carries under this form.
    fn endpoint(self, index: usize) -> &'static str {
        const CUSTOM: &str = "<CustomOriginConfig><HTTPPort>80</HTTPPort>\
                              <HTTPSPort>443</HTTPSPort>\
                              <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
                              </CustomOriginConfig>";
        const S3: &str = "<S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity>\
                          </S3OriginConfig>";
        match self {
            Self::Custom => CUSTOM,
            Self::S3 => S3,
            Self::Mixed if index.is_multiple_of(2) => CUSTOM,
            Self::Mixed => S3,
        }
    }
}

/// The id of the origin at `index`, which is also what its domain name is built from.
fn origin_id(index: usize) -> String {
    format!("origin-{index}")
}

/// A `DistributionConfig` document under `reference` stating `count` origins of `form`, with
/// the default cache behaviour targeting the first of them.
///
/// Every member a complete configuration needs is stated, so the same document serves a
/// create and an update alike.
fn config_xml(reference: &str, count: usize, form: Form) -> String {
    let mut out = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>{count}</Quantity><Items>"
    );
    for index in 0..count {
        let id = origin_id(index);
        out.push_str(&format!(
            "<Origin><Id>{id}</Id><DomainName>{id}.example.com</DomainName>{}</Origin>",
            form.endpoint(index)
        ));
    }
    out.push_str("</Items></Origins>");
    out.push_str(&format!(
        "<DefaultCacheBehavior><TargetOriginId>{}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <Comment>for {reference}</Comment>\
         <PriceClass>PriceClass_All</PriceClass><Enabled>true</Enabled>\
         </DistributionConfig>",
        origin_id(0)
    ));
    out
}

/// The `<Message>` of the restXml error envelope, or `""`.
fn message(answer: &Answer) -> &str {
    answer.root().find_text("Message").unwrap_or_default()
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

/// Assert `answer` is the refusal CloudFront states for this class, word for word, and that it
/// repeats nothing the request carried.
fn assert_class_refusal(case: &str, path: &str, answer: &Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case} ({path}): expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "TooManyOrigins",
        "case {case} ({path}): expected TooManyOrigins, got {:?} with {}",
        answer.error_code(),
        answer.raw
    );
    assert_eq!(
        message(answer),
        EXPECTED_MESSAGE,
        "case {case} ({path}): expected the class sentence, got {:?}",
        message(answer)
    );
    assert!(
        answer.header("location").is_empty(),
        "case {case} ({path}): a refusal announces no location, got {:?}",
        answer.header("location")
    );
}

/// Every refused row: how far past the quota the list is, and the form its origins take.
fn refused_rows() -> Vec<(String, usize, Form)> {
    [
        ("one-past-the-quota", ORIGIN_QUOTA + 1, Form::Custom),
        ("two-past-the-quota", ORIGIN_QUOTA + 2, Form::S3),
        ("nine-past-the-quota", ORIGIN_QUOTA + 9, Form::Mixed),
        ("half-a-quota-past-the-quota", ORIGIN_QUOTA * 3 / 2, Form::Custom),
    ]
    .into_iter()
    .map(|(case, count, form)| (case.to_string(), count, form))
    .collect()
}

#[tokio::test]
async fn a_configuration_past_the_origins_quota_states_the_class_sentence_on_every_create_path() {
    let router = emulator();

    for (index, (case, count, form)) in refused_rows().into_iter().enumerate() {
        for (path, answer) in [
            (
                "CreateDistribution",
                create_distribution(
                    &router,
                    &config_xml(&format!("origins-{index}-plain"), count, form),
                )
                .await,
            ),
            (
                "CreateDistributionWithTags",
                create_distribution_with_tags(
                    &router,
                    &config_xml(&format!("origins-{index}-tagged"), count, form),
                )
                .await,
            ),
        ] {
            assert_class_refusal(&case, path, &answer);
        }
        // A refused create claims nothing.
        let held = stored_count(&router).await;
        assert_eq!(
            held, 0,
            "case {case}: expected the account to hold no distribution, got {held}"
        );
    }
}

#[tokio::test]
async fn a_refused_update_states_the_same_sentence_and_changes_nothing() {
    let router = emulator();
    // One stored distribution the update rows land on, carrying a single origin.
    let created =
        create_distribution(&router, &config_xml("origins-subject", 1, Form::Custom)).await;
    assert_eq!(
        created.status, 201,
        "case the subject: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    let etag = created.header("etag").to_string();

    for (case, count, form) in refused_rows() {
        // An update submits the configuration whole under the distribution's OWN caller
        // reference, which no update may rename.
        let refused = update_distribution(
            &router,
            &id,
            &config_xml("origins-subject", count, form),
            Some(&etag),
        )
        .await;
        assert_class_refusal(&case, "UpdateDistribution", &refused);

        // The refused update left the stored configuration, and its version, exactly as they
        // were: the next row still lands on the version this one read.
        let read = get_distribution_config(&router, &id).await;
        assert_eq!(
            read.root().text_at("Origins/Quantity"),
            Some("1"),
            "case {case} (read-back): expected the stored configuration to keep its one \
             origin, got {}",
            read.raw
        );
        assert_eq!(
            read.header("etag"),
            etag,
            "case {case} (read-back): a refused update mints no new version, got {:?}",
            read.header("etag")
        );
    }
}

#[tokio::test]
async fn a_configuration_at_the_origins_quota_is_stored_with_every_origin_it_submitted() {
    let router = emulator();

    for (case, count) in [
        ("one-origin", 1),
        ("one-below-the-quota", ORIGIN_QUOTA - 1),
        ("exactly-the-quota", ORIGIN_QUOTA),
    ] {
        let reference = format!("origins-at-{count}");
        let created =
            create_distribution(&router, &config_xml(&reference, count, Form::Mixed)).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201, got {} with {}",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();

        // The configuration the create stored is the one a later read answers with, origin for
        // origin and in the order the document stated them.
        let read = get_distribution_config(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 from GetDistributionConfig, got {} with {}",
            read.status, read.raw
        );
        assert_eq!(
            read.root().text_at("Origins/Quantity"),
            Some(count.to_string().as_str()),
            "case {case}: expected Origins.Quantity {count}, got {:?}",
            read.root().text_at("Origins/Quantity")
        );
        let stored: Vec<String> = read
            .root()
            .at("Origins/Items")
            .map(|items| {
                items
                    .children_named("Origin")
                    .into_iter()
                    .map(|origin| origin.text_at("Id").unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        let submitted: Vec<String> = (0..count).map(origin_id).collect();
        assert_eq!(
            stored, submitted,
            "case {case}: expected the stored origins {submitted:?}, got {stored:?}"
        );
    }
}

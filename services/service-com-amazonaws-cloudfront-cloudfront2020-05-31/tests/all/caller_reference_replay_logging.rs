// SPDX-License-Identifier: CC0-1.0

//! What a repeated `CallerReference` is on `CreateDistribution`: the same create again, or a
//! second distribution under a reference that is already taken.
//!
//! The rule: a caller reference is "a unique value … that ensures that the request can't be
//! replayed", and a repeat under one the account already used is the SAME create when it asks
//! for the same distribution — then the call answers with the distribution that reference
//! already names, at its current version, and mints nothing — and a second distribution under
//! a taken reference otherwise, which is refused `DistributionAlreadyExists`. The
//! ACCESS-LOGGING configuration is not part of what the create asks that reference to name:
//! logging is a delivery setting of a distribution that already exists, turned on and
//! retargeted through `UpdateDistribution`, so a repeat that differs only in `Logging`
//! answers with the original distribution — carrying the logging configuration the original
//! was created with — while a repeat that differs in any other member is refused.
//!
//! The rows differ along what the rule generalises over: each member of `LoggingConfig`
//! (`Enabled`, `IncludeCookies`, `Bucket`, `Prefix`), the element being added and dropped
//! whole, and — as the controls that keep the rule from being "anything replays" — a member
//! outside `Logging` changed at the configuration level, inside a nested structure and inside
//! a list.
//!
//! The harness is [`crate::distribution_lifecycle`]'s, reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    create_distribution, emulator, list_distributions, Answer, API_VERSION, HOST,
};

/// The logging configuration a row's first create submits, as an XML fragment, or `""` for a
/// configuration that states no `Logging` element at all.
///
/// `LoggingConfig$Bucket` is "the Amazon S3 bucket to store the access logs in, for example,
/// `amzn-s3-demo-bucket.s3.amazonaws.com`", so the value is a bucket's endpoint host.
fn logging_xml(enabled: bool, include_cookies: bool, bucket: &str, prefix: &str) -> String {
    format!(
        "<Logging><Enabled>{enabled}</Enabled>\
         <IncludeCookies>{include_cookies}</IncludeCookies>\
         <Bucket>{bucket}</Bucket><Prefix>{prefix}</Prefix></Logging>"
    )
}

/// One `DistributionConfig`: one origin, one default cache behaviour, and whatever the row
/// states for the comment, the origin host, the enabled flag and the logging element.
struct Config {
    reference: String,
    comment: String,
    origin_host: String,
    enabled: bool,
    logging: String,
}

impl Config {
    /// The configuration both legs of a row start from.
    fn base(reference: &str, logging: String) -> Self {
        Self {
            reference: reference.to_string(),
            comment: "a caller-reference replay".to_string(),
            origin_host: HOST.to_string(),
            enabled: true,
            logging,
        }
    }

    fn xml(&self) -> String {
        let Self {
            reference,
            comment,
            origin_host,
            enabled,
            logging,
        } = self;
        format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items>\
             <Origin><Id>primary</Id><DomainName>{origin_host}</DomainName></Origin>\
             </Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
             </DefaultCacheBehavior>\
             <Comment>{comment}</Comment>\
             <Enabled>{enabled}</Enabled>{logging}\
             </DistributionConfig>"
        )
    }
}

/// What a row expects of the repeat under the same caller reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The same create again: the distribution that reference already names, unchanged.
    TheOriginal,
    /// A second distribution under a taken reference: refused, and nothing created.
    Refused,
}

/// One row: the configuration the first create submits, the change the repeat makes to it,
/// and what the repeat must then be answered with.
struct Row {
    case: &'static str,
    /// The logging element the FIRST create states.
    first_logging: String,
    /// The change the repeat makes — applied to the configuration the first create submitted.
    change: fn(&mut Config),
    expected: Expected,
}

/// The four members of `LoggingConfig`, the element added and dropped whole, the identical
/// repeat, and the controls that differ outside `Logging`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the logging prefix changed",
            first_logging: logging_xml(true, false, "logs.s3.amazonaws.com", "a/"),
            change: |config| {
                config.logging = logging_xml(true, false, "logs.s3.amazonaws.com", "b/");
            },
            expected: Expected::TheOriginal,
        },
        Row {
            case: "the logging bucket changed",
            first_logging: logging_xml(true, false, "logs-one.s3.amazonaws.com", "a/"),
            change: |config| {
                config.logging = logging_xml(true, false, "logs-two.s3.amazonaws.com", "a/");
            },
            expected: Expected::TheOriginal,
        },
        Row {
            case: "logging switched off",
            first_logging: logging_xml(true, false, "logs.s3.amazonaws.com", "a/"),
            change: |config| {
                config.logging = logging_xml(false, false, "", "");
            },
            expected: Expected::TheOriginal,
        },
        Row {
            case: "logging switched on",
            first_logging: logging_xml(false, false, "", ""),
            change: |config| {
                config.logging = logging_xml(true, false, "logs.s3.amazonaws.com", "a/");
            },
            expected: Expected::TheOriginal,
        },
        Row {
            case: "cookies included in the logs",
            first_logging: logging_xml(true, false, "logs.s3.amazonaws.com", "a/"),
            change: |config| {
                config.logging = logging_xml(true, true, "logs.s3.amazonaws.com", "a/");
            },
            expected: Expected::TheOriginal,
        },
        Row {
            case: "the logging element dropped whole",
            first_logging: logging_xml(true, false, "logs.s3.amazonaws.com", "a/"),
            change: |config| config.logging = String::new(),
            expected: Expected::TheOriginal,
        },
        Row {
            case: "the logging element added where there was none",
            first_logging: String::new(),
            change: |config| {
                config.logging = logging_xml(true, false, "logs.s3.amazonaws.com", "a/");
            },
            expected: Expected::TheOriginal,
        },
        Row {
            case: "nothing changed at all",
            first_logging: logging_xml(true, false, "logs.s3.amazonaws.com", "a/"),
            change: |_config| {},
            expected: Expected::TheOriginal,
        },
        Row {
            case: "the comment changed",
            first_logging: logging_xml(true, false, "logs.s3.amazonaws.com", "a/"),
            change: |config| config.comment = "a different comment".to_string(),
            expected: Expected::Refused,
        },
        Row {
            case: "the distribution disabled",
            first_logging: logging_xml(true, false, "logs.s3.amazonaws.com", "a/"),
            change: |config| config.enabled = false,
            expected: Expected::Refused,
        },
        Row {
            case: "the origin's domain name changed",
            first_logging: logging_xml(true, false, "logs.s3.amazonaws.com", "a/"),
            change: |config| config.origin_host = "origin.example.com".to_string(),
            expected: Expected::Refused,
        },
    ]
}

/// The `Logging` element of a created distribution, as four values read off the answer.
fn logging_of(answer: &Answer, case: &str) -> (String, String, String, String) {
    let logging = answer
        .root()
        .descendant("Logging")
        .unwrap_or_else(|| panic!("case {case}: the answer carries a Logging element: {}", answer.raw));
    let member = |name: &str| logging.text_at(name).unwrap_or_default().to_string();
    (
        member("Enabled"),
        member("IncludeCookies"),
        member("Bucket"),
        member("Prefix"),
    )
}

/// How many distributions this account holds, which is what says whether a repeat minted one.
async fn distribution_count(router: &MultiServiceRouter, case: &str) -> String {
    let listed = list_distributions(router, "").await;
    assert_eq!(
        listed.status, 200,
        "case {case}: expected the listing to be served, got {} with {}",
        listed.status, listed.raw
    );
    listed
        .root()
        .find_text("Quantity")
        .unwrap_or_default()
        .to_string()
}

/// Every row's first create lands, and the repeat under its caller reference is answered the
/// way the rule says — with the original distribution, logging and all, or with the modelled
/// conflict.
#[tokio::test]
async fn a_repeat_under_a_used_caller_reference_is_the_same_create_unless_it_asks_for_another_distribution()
{
    let router = emulator();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let first = Config::base(&format!("replay-{index}"), row.first_logging.clone());
        let created = create_distribution(&router, &first.xml()).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected the first create to be accepted, got {} with {}",
            created.status, created.raw
        );
        let id = created
            .root()
            .find_text("Id")
            .unwrap_or_default()
            .to_string();
        let etag = created.header("etag").to_string();
        let logging = logging_of(&created, case);
        let held = distribution_count(&router, case).await;

        let mut repeat = Config::base(&format!("replay-{index}"), row.first_logging.clone());
        (row.change)(&mut repeat);
        let repeated = create_distribution(&router, &repeat.xml()).await;

        match row.expected {
            Expected::TheOriginal => {
                assert_eq!(
                    repeated.status, 201,
                    "case {case}: expected the repeat to be answered with the original, got {} \
                     with {}",
                    repeated.status, repeated.raw
                );
                assert_eq!(
                    repeated.root().find_text("Id").unwrap_or_default(),
                    id,
                    "case {case}: expected the distribution {id:?} the reference already names, \
                     got {}",
                    repeated.raw
                );
                assert_eq!(
                    repeated.header("etag"),
                    etag,
                    "case {case}: expected the original's own version {etag:?}, got {:?}",
                    repeated.header("etag")
                );
                assert!(
                    repeated.header("location").ends_with(&format!("/{id}")),
                    "case {case}: expected the Location to address {id:?}, got {:?}",
                    repeated.header("location")
                );
                let repeated_logging = logging_of(&repeated, case);
                assert_eq!(
                    repeated_logging, logging,
                    "case {case}: expected the logging configuration the distribution was \
                     created with, got {repeated_logging:?}"
                );
            }
            Expected::Refused => {
                assert_eq!(
                    repeated.status, 409,
                    "case {case}: expected the repeat to be refused, got {} with {}",
                    repeated.status, repeated.raw
                );
                assert_eq!(
                    repeated.error_code(),
                    "DistributionAlreadyExists",
                    "case {case}: expected the modelled conflict, got {}",
                    repeated.raw
                );
                assert!(
                    repeated.raw.contains(&id),
                    "case {case}: expected the refusal to name the distribution {id:?} the \
                     reference is associated with, got {}",
                    repeated.raw
                );
            }
        }

        let after = distribution_count(&router, case).await;
        assert_eq!(
            after, held,
            "case {case}: expected the repeat to mint nothing, so {held:?} distributions, got \
             {after:?}"
        );
    }
}

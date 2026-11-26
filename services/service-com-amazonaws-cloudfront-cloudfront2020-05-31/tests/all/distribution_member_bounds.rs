// SPDX-License-Identifier: CC0-1.0

//! Three members of a submitted `DistributionConfig` whose value CloudFront bounds, end to
//! end over HTTP.
//!
//! RULE: a distribution is written only where every member the configuration states carries a
//! value inside the domain CloudFront documents for that member; one outside it is refused
//! `InvalidArgument` (HTTP 400) with the sentence that names the parameter, with no `Location`
//! header and nothing stored. The members this suite covers, each with what documents its
//! domain:
//!
//! * `DistributionConfig$Comment` — "a comment to describe the distribution. The comment
//!   cannot be longer than 128 characters"
//!   (`smithy_query(query="doc",shape="DistributionConfig",member="Comment")`), the same bound
//!   this service states for every other comment it stores;
//! * `CacheTagConfig$HeaderName` — "the name of the HTTP header that your origin includes in
//!   responses. CloudFront uses this header to extract cache tags"
//!   (`smithy_query(query="shape",shape="CacheTagConfig")`), so the value is a header FIELD
//!   NAME: an HTTP token, which is RFC 9110's `tchar` run — the ASCII letters and digits plus
//!   ``!#$%&'*+-.^_`|~``;
//! * `OriginCustomHeader$HeaderName` — "the name of a header that you want CloudFront to send
//!   to your origin", whose documentation points at the Developer Guide page
//!   (`smithy_query(query="doc",shape="OriginCustomHeader",member="HeaderName")`) that lists
//!   the headers CloudFront can't add to an origin request ([`RESERVED_HEADERS`]).
//!
//! The rows span what each rule generalises over: the comment AT its bound and one character
//! past it (and far past it), a header name drawn from each class of character a token admits
//! beside values carrying a character from outside it (a space, a colon, a comma, a
//! non-ASCII letter) and the empty value, and a reserved header name the issue that filed this
//! did not cite, in the case AWS writes it and in another case — a field name is
//! case-insensitive — beside legal header names on the same origin. Every refusal is asserted
//! to have left nothing behind, and the rules are asserted on the CREATE and the UPDATE alike,
//! because each is about the configuration rather than about the call carrying one.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, reused rather than copied.

use crate::distribution_lifecycle::{
    create_distribution, emulator, get_distribution, list_distributions, update_distribution,
    Answer, API_VERSION,
};

/// The code every refusal here travels under, and the status the model gives it
/// (`smithy_query(query="operation",operation="CreateDistribution")` -> `InvalidArgument`,
/// httpError 400).
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The longest comment a distribution's configuration carries, from the member's own
/// documentation.
const MAX_COMMENT: usize = 128;

/// Headers the Amazon CloudFront Developer Guide's "Adding custom headers to origin requests"
/// lists as ones CloudFront can't add for a distribution — the page
/// `OriginCustomHeader$HeaderName` points at. The rows below name ones the report that filed
/// this rule did not cite, so the catalog is tested beyond its example.
const RESERVED_HEADERS: &[&str] = &["Cookie", "Via", "Transfer-Encoding", "X-Real-IP"];

/// One configuration a row submits, assembled in the member order the model declares.
#[derive(Debug, Clone, Default)]
struct Document {
    /// `Origins$Items$Origin$CustomHeaders$Items`, as `(HeaderName, HeaderValue)` pairs.
    custom_headers: Vec<(String, String)>,
    /// `DistributionConfig$Comment`, or `None` for the comment every other row carries.
    comment: Option<String>,
    /// `DistributionConfig$CacheTagConfig$HeaderName`, where the row states the member.
    cache_tag_header: Option<String>,
}

/// XML-escape a value a document carries as element text.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl Document {
    /// The `DistributionConfig` XML a client sends for this document under `reference`.
    fn xml(&self, reference: &str) -> String {
        let mut out = format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{reference}</CallerReference>\
             <Origins><Quantity>1</Quantity><Items>\
             <Origin><Id>primary</Id><DomainName>primary.example.com</DomainName>"
        );
        if !self.custom_headers.is_empty() {
            out.push_str(&format!(
                "<CustomHeaders><Quantity>{}</Quantity><Items>",
                self.custom_headers.len()
            ));
            for (name, value) in &self.custom_headers {
                out.push_str(&format!(
                    "<OriginCustomHeader><HeaderName>{}</HeaderName>\
                     <HeaderValue>{}</HeaderValue></OriginCustomHeader>",
                    escape(name),
                    escape(value)
                ));
            }
            out.push_str("</Items></CustomHeaders>");
        }
        out.push_str(
            "<CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
             <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
             </CustomOriginConfig></Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>",
        );
        out.push_str(&format!(
            "<Comment>{}</Comment>",
            escape(self.comment.as_deref().unwrap_or("member bounds"))
        ));
        out.push_str("<PriceClass>PriceClass_All</PriceClass><Enabled>true</Enabled>");
        if let Some(header_name) = &self.cache_tag_header {
            out.push_str(&format!(
                "<CacheTagConfig><HeaderName>{}</HeaderName></CacheTagConfig>",
                escape(header_name)
            ));
        }
        out.push_str("</DistributionConfig>");
        out
    }
}

/// What the emulator has to answer a row with.
#[derive(Debug, Clone)]
enum Expect {
    /// The configuration is stored.
    Accepted,
    /// The configuration is refused with this sentence.
    Refused(String),
}

/// One row: a document, and what the emulator has to answer it with.
struct Case {
    case: String,
    document: Document,
    expect: Expect,
}

/// Every row the three rules are asserted over.
fn cases() -> Vec<Case> {
    let mut cases = vec![
        Case {
            case: "a comment at the documented maximum".to_string(),
            document: Document {
                comment: Some("c".repeat(MAX_COMMENT)),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "a comment one character past the maximum".to_string(),
            document: Document {
                comment: Some("c".repeat(MAX_COMMENT + 1)),
                ..Document::default()
            },
            expect: Expect::Refused("The parameter Comment is too big.".to_string()),
        },
        Case {
            case: "a comment far past the maximum".to_string(),
            document: Document {
                comment: Some("c".repeat(MAX_COMMENT * 4)),
                ..Document::default()
            },
            expect: Expect::Refused("The parameter Comment is too big.".to_string()),
        },
        Case {
            case: "an empty comment".to_string(),
            document: Document {
                comment: Some(String::new()),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
    ];

    // The cache tag header name: one row per class of character an HTTP token admits, and one
    // per class of character outside it. The token row leaves out the two characters XML
    // escapes in element text, so what the document carries is what the member carries.
    for header_name in ["X-Cache-Tags", "x-tags", "Tags0", "a!#$%*+-.^_`|~"] {
        cases.push(Case {
            case: format!("a cache tag header name that is a token: {header_name:?}"),
            document: Document {
                cache_tag_header: Some(header_name.to_string()),
                ..Document::default()
            },
            expect: Expect::Accepted,
        });
    }
    for header_name in [
        "X-Cache Tags",
        "X-Cache:Tags",
        "X-Cache,Tags",
        "X-Cache/Tags",
        "X-Cachè-Tags",
        "",
    ] {
        cases.push(Case {
            case: format!("a cache tag header name outside the token set: {header_name:?}"),
            document: Document {
                cache_tag_header: Some(header_name.to_string()),
                ..Document::default()
            },
            expect: Expect::Refused(format!(
                "The parameter CacheTagConfig:headerName contains {header_name} that has \
                 illegal characters."
            )),
        });
    }

    // The origin custom headers: a legal header name, then each reserved one, in the case the
    // guide writes it and in another case, since a field name is case-insensitive.
    cases.push(Case {
        case: "origin custom headers CloudFront may add".to_string(),
        document: Document {
            custom_headers: vec![
                ("X-Tenant".to_string(), "acme".to_string()),
                ("X-Origin-Verify".to_string(), "secret".to_string()),
            ],
            ..Document::default()
        },
        expect: Expect::Accepted,
    });
    for reserved in RESERVED_HEADERS {
        for written in [reserved.to_string(), reserved.to_uppercase()] {
            cases.push(Case {
                case: format!("an origin custom header CloudFront reserves: {written:?}"),
                document: Document {
                    custom_headers: vec![
                        ("X-Tenant".to_string(), "acme".to_string()),
                        (written.clone(), "overridden".to_string()),
                    ],
                    ..Document::default()
                },
                expect: Expect::Refused(format!(
                    "The parameter HeaderName : {written} is not allowed."
                )),
            });
        }
    }

    cases
}

/// Assert that `answer` is what `expect` says, and that a refusal announced no resource.
fn assert_answer(case: &str, answer: &Answer, expect: &Expect, context: &str, success: u16) {
    match expect {
        Expect::Accepted => assert_eq!(
            answer.status, success,
            "case {case} ({context}): expected the configuration to be stored, got {} with {}",
            answer.status, answer.raw
        ),
        Expect::Refused(message) => {
            assert_eq!(
                answer.status, 400,
                "case {case} ({context}): expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                INVALID_ARGUMENT,
                "case {case} ({context}): expected {INVALID_ARGUMENT}, got {:?} in {}",
                answer.error_code(),
                answer.raw
            );
            let answered = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                answered, message,
                "case {case} ({context}): expected the message {message:?}, got {answered:?}"
            );
            assert_eq!(
                answer.header("location"),
                "",
                "case {case} ({context}): expected no Location header on a refusal, got {:?}",
                answer.header("location")
            );
        }
    }
}

/// How many distributions the account holds.
async fn stored_count(router: &smithy_server_runtime::MultiServiceRouter) -> usize {
    list_distributions(router, "")
        .await
        .root()
        .find_text("Quantity")
        .unwrap_or_default()
        .parse()
        .unwrap_or_default()
}

/// A create stores a configuration only where every one of the three members is inside its
/// documented domain, and an admitted document really is persisted.
#[tokio::test]
async fn a_create_admits_only_a_member_inside_its_documented_domain() {
    let router = emulator();
    let mut stored = 0usize;

    for (index, case) in cases().into_iter().enumerate() {
        let reference = format!("member-bound-{index}");
        let answer = create_distribution(&router, &case.document.xml(&reference)).await;
        assert_answer(&case.case, &answer, &case.expect, "CreateDistribution", 201);

        if matches!(case.expect, Expect::Accepted) {
            stored += 1;
            // The configuration the create stored is the one a later read answers with, so an
            // admitted value is persisted rather than merely accepted.
            let id = answer.root().text_at("Id").unwrap_or_default().to_string();
            let read = get_distribution(&router, &id).await;
            assert_eq!(
                read.status, 200,
                "case {}: expected the stored distribution to be readable, got {} with {}",
                case.case, read.status, read.raw
            );
            let config = read
                .root()
                .at("DistributionConfig")
                .unwrap_or_else(|| panic!("case {}: the read carries no configuration", case.case));
            assert_eq!(
                config.text_at("Comment").unwrap_or_default(),
                case.document.comment.as_deref().unwrap_or("member bounds"),
                "case {}: expected the submitted Comment, got {}",
                case.case,
                read.raw
            );
            if let Some(header_name) = &case.document.cache_tag_header {
                assert_eq!(
                    config.text_at("CacheTagConfig/HeaderName"),
                    Some(header_name.as_str()),
                    "case {}: expected the submitted CacheTagConfig header name, got {}",
                    case.case,
                    read.raw
                );
            }
            let reported: Vec<String> = config
                .at("Origins/Items/Origin/CustomHeaders/Items")
                .map(|items| {
                    items
                        .children_named("OriginCustomHeader")
                        .into_iter()
                        .map(|header| header.text_at("HeaderName").unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default();
            let submitted: Vec<String> = case
                .document
                .custom_headers
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            assert_eq!(
                reported, submitted,
                "case {}: expected the submitted custom header names, got {reported:?}",
                case.case
            );
        }

        // A refused create claims nothing: the account holds exactly the distributions its
        // admitted documents created.
        let count = stored_count(&router).await;
        assert_eq!(
            count, stored,
            "case {}: expected the account to hold {stored} distributions, got {count}",
            case.case
        );
    }
}

/// The same rules on the UPDATE path: a configuration a create would refuse is refused there
/// too, and the refusal leaves the stored configuration and its version exactly as they were.
#[tokio::test]
async fn an_update_admits_only_a_member_inside_its_documented_domain() {
    let router = emulator();
    let created = create_distribution(
        &router,
        &Document::default().xml("member-bound-update-subject"),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case the subject: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    let mut etag = created.header("etag").to_string();

    for case in cases() {
        // An update submits the configuration whole under the distribution's OWN caller
        // reference, which no update may rename.
        let document = case.document.xml("member-bound-update-subject");
        let answer = update_distribution(&router, &id, &document, Some(&etag)).await;
        assert_answer(&case.case, &answer, &case.expect, "UpdateDistribution", 200);
        if matches!(case.expect, Expect::Accepted) {
            etag = answer.header("etag").to_string();
            continue;
        }
        let read = get_distribution(&router, &id).await;
        assert_eq!(
            read.header("etag"),
            etag,
            "case {}: expected the refused update to leave the version at {etag:?}, got {:?}",
            case.case,
            read.header("etag")
        );
    }
}

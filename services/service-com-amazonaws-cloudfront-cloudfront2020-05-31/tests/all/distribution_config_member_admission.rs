// SPDX-License-Identifier: CC0-1.0

//! What a submitted `DistributionConfig`'s OWN MEMBERS are admitted by, end to end over HTTP.
//!
//! RULE: a distribution is written only where every member the configuration states carries a
//! value inside the domain CloudFront documents for that member and agrees with the members it
//! depends on; a configuration carrying one outside it is refused with the status and the code
//! the model declares for THAT member, with no `Location` header and nothing stored. The rule
//! is about the CONFIGURATION rather than about the call carrying one, so it holds on the
//! create and on the update alike.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The table below spans what the rule generalises over, one group of rows per
//! member family, each with the value AT the boundary (accepted) beside the value outside it
//! (refused):
//!
//! * the published per-list quotas: the origin custom headers and the alternate domain names
//!   of a distribution, its origins and origin groups, and the cookie names, headers and query
//!   string keys of one cache behaviour's legacy cache key;
//! * `DefaultRootObject`'s character set and length;
//! * a custom error response's `ErrorCode`, `ResponseCode` and `ResponsePagePath`;
//! * a geographic restriction's agreement between its restriction type and its locations, and
//!   each location being an `ISO 3166-1-alpha-2` country code;
//! * a legacy cache key that forwards no query string while keying on query strings;
//! * the order of a cache behaviour's three TTLs, over each pair of them;
//! * a cache behaviour's `TargetOriginId` resolving against the configuration's own origins
//!   and origin groups;
//! * a cache behaviour's `TrustedKeyGroups` resolving against the key groups the account holds.
//!
//! The refusal sentences are LITERALS below rather than the crate's own constants, so a
//! reworded refusal fails this suite. The harness — the router and the request helpers — is
//! the one the per-resource suites use, reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    create_distribution, emulator, get_distribution, list_distributions, update_distribution,
    Answer, API_VERSION, CUSTOM_VIEWER_CERTIFICATE,
};
use crate::key_group_lifecycle::{create_group, mint_public_keys};

/// The published quota on the origin custom headers one distribution may state.
const ORIGIN_CUSTOM_HEADERS_QUOTA: usize = 10;
/// The published quota on the alternate domain names one distribution may state.
const ALIAS_QUOTA: usize = 100;
/// The published quota on the origins one distribution may state.
const ORIGIN_QUOTA: usize = 25;
/// The published quota on the origin groups one distribution may state.
const ORIGIN_GROUP_QUOTA: usize = 10;
/// The published quota on the cookie names, headers and query string keys ONE cache behaviour
/// may state in its legacy cache key.
const PER_BEHAVIOR_QUOTA: usize = 10;
/// The longest a `DefaultRootObject` may be.
const DEFAULT_ROOT_OBJECT_LENGTH: usize = 255;

/// What the emulator has to answer a row with.
#[derive(Debug, Clone)]
enum Expect {
    /// The configuration is stored: HTTP 201 with a `Location` header.
    Accepted,
    /// The configuration is refused: this status, this wire code and this sentence, and no
    /// `Location` header.
    Refused {
        status: u16,
        code: &'static str,
        message: &'static str,
    },
}

/// A refusal expectation, written once per row.
fn refused(status: u16, code: &'static str, message: &'static str) -> Expect {
    Expect::Refused {
        status,
        code,
        message,
    }
}

/// XML-escape a value a document carries as element text, so a member whose value carries one
/// of the characters XML reserves travels as that value rather than as markup.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One `DistributionConfig` a row submits, assembled in the member order the model declares.
#[derive(Debug, Clone)]
struct Document {
    /// `Aliases.Items`, which take a custom viewer certificate when any are stated.
    aliases: Vec<String>,
    /// `DefaultRootObject`, where the row states one.
    default_root_object: Option<String>,
    /// `(origin id, how many custom headers that origin carries)`, in order.
    origins: Vec<(String, usize)>,
    /// `OriginGroups.Items` ids, each failing over between the first two origins.
    origin_groups: Vec<String>,
    /// `DefaultCacheBehavior$TargetOriginId`.
    default_target: String,
    /// The key groups the default cache behaviour trusts, where the row names any.
    trusted_key_groups: Vec<String>,
    /// The members of the default cache behaviour that follow `ViewerProtocolPolicy`.
    behavior_tail: String,
    /// `(path pattern, target origin id)` of each ordered cache behaviour.
    ordered: Vec<(String, String)>,
    /// `(ErrorCode, ResponsePagePath, ResponseCode)` of each custom error response.
    custom_errors: Vec<(i32, Option<String>, Option<String>)>,
    /// `(RestrictionType, the locations it names)`, where the row states a restriction.
    geo: Option<(String, Vec<String>)>,
}

impl Default for Document {
    /// One custom origin and a default cache behaviour targeting it: a configuration every
    /// rule below admits, which each row then varies in ONE member.
    fn default() -> Self {
        Self {
            aliases: Vec::new(),
            default_root_object: None,
            origins: vec![("primary".to_string(), 0)],
            origin_groups: Vec::new(),
            default_target: "primary".to_string(),
            trusted_key_groups: Vec::new(),
            behavior_tail: String::new(),
            ordered: Vec::new(),
            custom_errors: Vec::new(),
            geo: None,
        }
    }
}

impl Document {
    /// The `DistributionConfig` XML a client sends for this document under `reference`.
    ///
    /// `PriceClass` is always stated, so the same document is a complete configuration for an
    /// update as well as for a create.
    fn xml(&self, reference: &str) -> String {
        let mut out = format!(
            "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <CallerReference>{reference}</CallerReference>"
        );
        if !self.aliases.is_empty() {
            out.push_str(&format!(
                "<Aliases><Quantity>{}</Quantity><Items>",
                self.aliases.len()
            ));
            for alias in &self.aliases {
                out.push_str(&format!("<CNAME>{alias}</CNAME>"));
            }
            out.push_str("</Items></Aliases>");
        }
        if let Some(object) = &self.default_root_object {
            out.push_str(&format!(
                "<DefaultRootObject>{}</DefaultRootObject>",
                escape(object)
            ));
        }
        out.push_str(&format!(
            "<Origins><Quantity>{}</Quantity><Items>",
            self.origins.len()
        ));
        for (id, headers) in &self.origins {
            out.push_str(&format!(
                "<Origin><Id>{id}</Id><DomainName>{id}.example.com</DomainName>"
            ));
            if *headers > 0 {
                out.push_str(&format!("<CustomHeaders><Quantity>{headers}</Quantity><Items>"));
                for index in 0..*headers {
                    out.push_str(&format!(
                        "<OriginCustomHeader><HeaderName>x-{id}-{index}</HeaderName>\
                         <HeaderValue>{index}</HeaderValue></OriginCustomHeader>"
                    ));
                }
                out.push_str("</Items></CustomHeaders>");
            }
            out.push_str(
                "<CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
                 <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
                 </CustomOriginConfig></Origin>",
            );
        }
        out.push_str("</Items></Origins>");
        if !self.origin_groups.is_empty() {
            out.push_str(&format!(
                "<OriginGroups><Quantity>{}</Quantity><Items>",
                self.origin_groups.len()
            ));
            for group in &self.origin_groups {
                // `OriginGroupMemberList` carries exactly two members, so a group fails over
                // between the document's first two origins.
                let members: String = self
                    .origins
                    .iter()
                    .take(2)
                    .map(|(id, _)| format!("<OriginGroupMember><OriginId>{id}</OriginId></OriginGroupMember>"))
                    .collect();
                out.push_str(&format!(
                    "<OriginGroup><Id>{group}</Id><FailoverCriteria><StatusCodes>\
                     <Quantity>1</Quantity><Items><StatusCode>500</StatusCode></Items>\
                     </StatusCodes></FailoverCriteria><Members><Quantity>2</Quantity>\
                     <Items>{members}</Items></Members></OriginGroup>"
                ));
            }
            out.push_str("</Items></OriginGroups>");
        }
        out.push_str(&format!(
            "<DefaultCacheBehavior><TargetOriginId>{}</TargetOriginId>",
            self.default_target
        ));
        if !self.trusted_key_groups.is_empty() {
            let items: String = self
                .trusted_key_groups
                .iter()
                .map(|group| format!("<KeyGroup>{group}</KeyGroup>"))
                .collect();
            out.push_str(&format!(
                "<TrustedKeyGroups><Enabled>true</Enabled><Quantity>{}</Quantity>\
                 <Items>{items}</Items></TrustedKeyGroups>",
                self.trusted_key_groups.len()
            ));
        }
        out.push_str("<ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>");
        out.push_str(&self.behavior_tail);
        out.push_str("</DefaultCacheBehavior>");
        if !self.ordered.is_empty() {
            out.push_str(&format!(
                "<CacheBehaviors><Quantity>{}</Quantity><Items>",
                self.ordered.len()
            ));
            for (pattern, target) in &self.ordered {
                out.push_str(&format!(
                    "<CacheBehavior><PathPattern>{pattern}</PathPattern>\
                     <TargetOriginId>{target}</TargetOriginId>\
                     <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></CacheBehavior>"
                ));
            }
            out.push_str("</Items></CacheBehaviors>");
        }
        if !self.custom_errors.is_empty() {
            out.push_str(&format!(
                "<CustomErrorResponses><Quantity>{}</Quantity><Items>",
                self.custom_errors.len()
            ));
            for (error_code, page, response_code) in &self.custom_errors {
                out.push_str(&format!(
                    "<CustomErrorResponse><ErrorCode>{error_code}</ErrorCode>"
                ));
                if let Some(page) = page {
                    out.push_str(&format!("<ResponsePagePath>{page}</ResponsePagePath>"));
                }
                if let Some(response_code) = response_code {
                    out.push_str(&format!("<ResponseCode>{response_code}</ResponseCode>"));
                }
                out.push_str("</CustomErrorResponse>");
            }
            out.push_str("</Items></CustomErrorResponses>");
        }
        out.push_str(
            "<Comment>member admission</Comment>\
             <PriceClass>PriceClass_All</PriceClass><Enabled>true</Enabled>",
        );
        if !self.aliases.is_empty() {
            out.push_str(CUSTOM_VIEWER_CERTIFICATE);
        }
        if let Some((restriction_type, locations)) = &self.geo {
            let items: String = locations
                .iter()
                .map(|location| format!("<Location>{location}</Location>"))
                .collect();
            let items = if locations.is_empty() {
                String::new()
            } else {
                format!("<Items>{items}</Items>")
            };
            out.push_str(&format!(
                "<Restrictions><GeoRestriction><RestrictionType>{restriction_type}\
                 </RestrictionType><Quantity>{}</Quantity>{items}</GeoRestriction></Restrictions>",
                locations.len()
            ));
        }
        out.push_str("</DistributionConfig>");
        out
    }
}

/// One row: a document, and what the emulator has to answer it with.
struct Case {
    case: &'static str,
    document: Document,
    expect: Expect,
}

/// `count` origins, the first named `primary` so the default behaviour's target resolves.
fn origins(count: usize) -> Vec<(String, usize)> {
    (0..count)
        .map(|index| {
            if index == 0 {
                ("primary".to_string(), 0)
            } else {
                (format!("origin-{index}"), 0)
            }
        })
        .collect()
}

/// The `ForwardedValues` of a legacy cache key: `query_string` as stated, `count` whitelisted
/// cookie names, `headers` forwarded headers and `keys` query string cache keys.
fn forwarded_values(query_string: bool, cookies: usize, headers: usize, keys: usize) -> String {
    let mut out = format!("<ForwardedValues><QueryString>{query_string}</QueryString>");
    if cookies == 0 {
        out.push_str("<Cookies><Forward>none</Forward></Cookies>");
    } else {
        let items: String = (0..cookies)
            .map(|index| format!("<Name>cookie-{index}</Name>"))
            .collect();
        out.push_str(&format!(
            "<Cookies><Forward>whitelist</Forward><WhitelistedNames><Quantity>{cookies}\
             </Quantity><Items>{items}</Items></WhitelistedNames></Cookies>"
        ));
    }
    if headers > 0 {
        let items: String = (0..headers)
            .map(|index| format!("<Name>x-header-{index}</Name>"))
            .collect();
        out.push_str(&format!(
            "<Headers><Quantity>{headers}</Quantity><Items>{items}</Items></Headers>"
        ));
    }
    if keys > 0 {
        let items: String = (0..keys)
            .map(|index| format!("<Name>key-{index}</Name>"))
            .collect();
        out.push_str(&format!(
            "<QueryStringCacheKeys><Quantity>{keys}</Quantity><Items>{items}</Items>\
             </QueryStringCacheKeys>"
        ));
    }
    out.push_str("</ForwardedValues>");
    out
}

/// The three TTL members a behaviour states, each where it is `Some`.
fn ttls(min: Option<i64>, default: Option<i64>, max: Option<i64>) -> String {
    let mut out = String::new();
    if let Some(min) = min {
        out.push_str(&format!("<MinTTL>{min}</MinTTL>"));
    }
    if let Some(default) = default {
        out.push_str(&format!("<DefaultTTL>{default}</DefaultTTL>"));
    }
    if let Some(max) = max {
        out.push_str(&format!("<MaxTTL>{max}</MaxTTL>"));
    }
    out
}

/// Every row the rule has to hold for: each member family at the boundary of its domain and
/// outside it.
///
/// The published quotas are the Amazon CloudFront Developer Guide's "Quotas" (the general
/// quotas on distributions, the headers quotas and the cache-key quotas). The character set
/// and the status-code domains are the ones the service states in the sentences it refuses a
/// value outside them with.
fn cases() -> Vec<Case> {
    let mut cases = vec![
        Case {
            case: "origin-custom-headers-at-the-quota",
            document: Document {
                origins: vec![("primary".to_string(), ORIGIN_CUSTOM_HEADERS_QUOTA)],
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "origin-custom-headers-past-the-quota",
            document: Document {
                origins: vec![("primary".to_string(), ORIGIN_CUSTOM_HEADERS_QUOTA + 1)],
                ..Document::default()
            },
            expect: refused(
                400,
                "TooManyOriginCustomHeaders",
                "Your request contains more OriginCustomHeaders than allowed per distribution.",
            ),
        },
        Case {
            // The quota is per DISTRIBUTION, so two origins that are each within it are
            // refused together once their counts add past it.
            case: "origin-custom-headers-past-the-quota-across-two-origins",
            document: Document {
                origins: vec![
                    ("primary".to_string(), 6),
                    ("secondary".to_string(), 5),
                ],
                ..Document::default()
            },
            expect: refused(
                400,
                "TooManyOriginCustomHeaders",
                "Your request contains more OriginCustomHeaders than allowed per distribution.",
            ),
        },
        Case {
            case: "origins-at-the-quota",
            document: Document {
                origins: origins(ORIGIN_QUOTA),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "origins-past-the-quota",
            document: Document {
                origins: origins(ORIGIN_QUOTA + 1),
                ..Document::default()
            },
            expect: refused(
                400,
                "TooManyOrigins",
                "You cannot create more origins for the distribution.",
            ),
        },
        Case {
            case: "origin-groups-at-the-quota",
            document: Document {
                origins: origins(2),
                origin_groups: (0..ORIGIN_GROUP_QUOTA)
                    .map(|index| format!("group-{index}"))
                    .collect(),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "origin-groups-past-the-quota",
            document: Document {
                origins: origins(2),
                origin_groups: (0..ORIGIN_GROUP_QUOTA + 1)
                    .map(|index| format!("group-{index}"))
                    .collect(),
                ..Document::default()
            },
            expect: refused(
                400,
                "TooManyOriginGroupsPerDistribution",
                "Processing your request would cause you to exceed the maximum number of \
                 origin groups allowed.",
            ),
        },
        Case {
            case: "aliases-past-the-quota",
            document: Document {
                aliases: (0..ALIAS_QUOTA + 1)
                    .map(|index| format!("alias-{index}.example.com"))
                    .collect(),
                ..Document::default()
            },
            expect: refused(
                400,
                "TooManyDistributionCNAMEs",
                "Your request contains more CNAMEs than are allowed per distribution.",
            ),
        },
        Case {
            case: "legacy-cache-key-at-every-per-behaviour-quota",
            document: Document {
                behavior_tail: forwarded_values(
                    true,
                    PER_BEHAVIOR_QUOTA,
                    PER_BEHAVIOR_QUOTA,
                    PER_BEHAVIOR_QUOTA,
                ),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "whitelisted-cookie-names-past-the-quota",
            document: Document {
                behavior_tail: forwarded_values(true, PER_BEHAVIOR_QUOTA + 1, 0, 0),
                ..Document::default()
            },
            expect: refused(
                400,
                "TooManyCookieNamesInWhiteList",
                "Your request contains more cookie names in the whitelist than are allowed \
                 per cache behavior.",
            ),
        },
        Case {
            case: "forwarded-headers-past-the-quota",
            document: Document {
                behavior_tail: forwarded_values(true, 0, PER_BEHAVIOR_QUOTA + 1, 0),
                ..Document::default()
            },
            expect: refused(
                400,
                "TooManyHeadersInForwardedValues",
                "Your request contains too many headers in forwarded values.",
            ),
        },
        Case {
            case: "query-string-cache-keys-past-the-quota",
            document: Document {
                behavior_tail: forwarded_values(true, 0, 0, PER_BEHAVIOR_QUOTA + 1),
                ..Document::default()
            },
            expect: refused(
                400,
                "TooManyQueryStringParameters",
                "Your request contains too many query string parameters.",
            ),
        },
        Case {
            // The whole character set the service states, including the punctuation a path
            // and a quoted key name carry — an entry the reported case did not cite.
            case: "default-root-object-over-the-whole-character-set",
            document: Document {
                default_root_object: Some("folder~1/a-b_c.d*e$f&g\"h'i.html".to_string()),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "default-root-object-with-a-space",
            document: Document {
                default_root_object: Some("my index.html".to_string()),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidDefaultRootObject",
                "The parameter DefaultRootObject can only contain these characters: \
                 [a-zA-Z0-9_-.*$/~\"'&]",
            ),
        },
        Case {
            // A character that has to be percent-encoded in a URL path is outside the set
            // too, and `%` itself is not in it.
            case: "default-root-object-with-a-percent-sign",
            document: Document {
                default_root_object: Some("index%20.html".to_string()),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidDefaultRootObject",
                "The parameter DefaultRootObject can only contain these characters: \
                 [a-zA-Z0-9_-.*$/~\"'&]",
            ),
        },
        Case {
            case: "default-root-object-at-the-length-bound",
            document: Document {
                default_root_object: Some("a".repeat(DEFAULT_ROOT_OBJECT_LENGTH)),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "default-root-object-past-the-length-bound",
            document: Document {
                default_root_object: Some("a".repeat(DEFAULT_ROOT_OBJECT_LENGTH + 1)),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidDefaultRootObject",
                "The parameter DefaultRootObject cannot be longer than 255 characters.",
            ),
        },
        Case {
            case: "custom-error-response-over-the-documented-statuses",
            document: Document {
                custom_errors: vec![
                    (404, Some("/404.html".to_string()), Some("200".to_string())),
                    (503, Some("/5xx.html".to_string()), Some("410".to_string())),
                ],
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            // `CustomErrorResponse` replaces "HTTP status codes in the 4xx and 5xx range", so
            // a success status is no status a custom error response is stated for.
            case: "custom-error-response-for-a-success-status",
            document: Document {
                custom_errors: vec![(200, None, None)],
                ..Document::default()
            },
            expect: refused(400, "InvalidArgument", "The parameter ErrorCode is invalid."),
        },
        Case {
            case: "custom-error-response-for-a-status-past-the-last-class",
            document: Document {
                custom_errors: vec![(600, None, None)],
                ..Document::default()
            },
            expect: refused(400, "InvalidArgument", "The parameter ErrorCode is invalid."),
        },
        Case {
            case: "custom-error-response-returning-no-status-code",
            document: Document {
                custom_errors: vec![(404, Some("/404.html".to_string()), Some("999".to_string()))],
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidArgument",
                "The parameter ResponseCode is invalid.",
            ),
        },
        Case {
            case: "custom-error-response-returning-text-for-a-status-code",
            document: Document {
                custom_errors: vec![(
                    404,
                    Some("/404.html".to_string()),
                    Some("not-a-code".to_string()),
                )],
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidArgument",
                "The parameter ResponseCode is invalid.",
            ),
        },
        Case {
            case: "custom-error-response-page-path-without-a-leading-slash",
            document: Document {
                custom_errors: vec![(404, Some("404.html".to_string()), Some("404".to_string()))],
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidArgument",
                "The parameter ResponsePagePath is invalid.",
            ),
        },
        Case {
            case: "custom-error-response-page-path-that-is-not-url-encoded",
            document: Document {
                custom_errors: vec![(
                    404,
                    Some("/4 04.html".to_string()),
                    Some("404".to_string()),
                )],
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidArgument",
                "The parameter ResponsePagePath is invalid.",
            ),
        },
        Case {
            case: "geo-restriction-disabled-and-naming-no-country",
            document: Document {
                geo: Some(("none".to_string(), Vec::new())),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "geo-restriction-whitelisting-one-country",
            document: Document {
                geo: Some(("whitelist".to_string(), vec!["DE".to_string()])),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            // A country code the reported case did not cite, read against the published
            // `ISO 3166-1-alpha-2` table rather than a list of a few codes.
            case: "geo-restriction-blacklisting-two-countries",
            document: Document {
                geo: Some((
                    "blacklist".to_string(),
                    vec!["ZW".to_string(), "FJ".to_string()],
                )),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "geo-restriction-whitelisting-no-country",
            document: Document {
                geo: Some(("whitelist".to_string(), Vec::new())),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidGeoRestrictionParameter",
                "Your request contains invalid Restriction parameters.",
            ),
        },
        Case {
            case: "geo-restriction-disabled-but-naming-a-country",
            document: Document {
                geo: Some(("none".to_string(), vec!["DE".to_string()])),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidGeoRestrictionParameter",
                "Your request contains invalid Restriction parameters.",
            ),
        },
        Case {
            case: "geo-restriction-naming-no-country-code",
            document: Document {
                geo: Some(("whitelist".to_string(), vec!["XX".to_string()])),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidLocationCode",
                "Your request contains one or more invalid location codes.",
            ),
        },
        Case {
            // "A two-letter, UPPERCASE country code": the same letters in lower case name no
            // country.
            case: "geo-restriction-naming-a-lowercase-country-code",
            document: Document {
                geo: Some(("whitelist".to_string(), vec!["de".to_string()])),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidLocationCode",
                "Your request contains one or more invalid location codes.",
            ),
        },
        Case {
            case: "legacy-cache-key-forwarding-the-query-string-it-keys-on",
            document: Document {
                behavior_tail: forwarded_values(true, 0, 0, 2),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "legacy-cache-key-keying-on-a-query-string-it-forwards-none-of",
            document: Document {
                behavior_tail: forwarded_values(false, 0, 0, 2),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidArgument",
                "The parameter QueryString cannot be false if query strings are set in \
                 QueryStringCacheKeys.",
            ),
        },
        Case {
            case: "ttls-in-order",
            document: Document {
                behavior_tail: ttls(Some(0), Some(60), Some(120)),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "ttls-all-equal",
            document: Document {
                behavior_tail: ttls(Some(60), Some(60), Some(60)),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "min-ttl-above-max-ttl",
            document: Document {
                behavior_tail: ttls(Some(120), None, Some(60)),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidTTLOrder",
                "MinTTL should be less than or equal to MaxTTL",
            ),
        },
        Case {
            case: "min-ttl-above-default-ttl",
            document: Document {
                behavior_tail: ttls(Some(120), Some(60), None),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidTTLOrder",
                "MinTTL should be less than or equal to DefaultTTL",
            ),
        },
        Case {
            case: "default-ttl-above-max-ttl",
            document: Document {
                behavior_tail: ttls(None, Some(120), Some(60)),
                ..Document::default()
            },
            expect: refused(
                400,
                "InvalidTTLOrder",
                "DefaultTTL should be less than or equal to MaxTTL",
            ),
        },
        Case {
            case: "an-ordered-behaviour-targeting-a-second-origin",
            document: Document {
                origins: origins(2),
                ordered: vec![("/images/*".to_string(), "origin-1".to_string())],
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            // "The value of the `ID` field of the origin that you want CloudFront to route
            // requests to" — an origin GROUP's id is one such value.
            case: "a-behaviour-targeting-an-origin-group",
            document: Document {
                origins: origins(2),
                origin_groups: vec!["failover".to_string()],
                default_target: "failover".to_string(),
                ..Document::default()
            },
            expect: Expect::Accepted,
        },
        Case {
            case: "the-default-behaviour-targeting-an-origin-the-configuration-does-not-carry",
            document: Document {
                default_target: "absent".to_string(),
                ..Document::default()
            },
            expect: refused(
                404,
                "NoSuchOrigin",
                "One or more of your cache behavior target origins or origin groups do not \
                 exist.",
            ),
        },
        Case {
            case: "an-ordered-behaviour-targeting-an-origin-the-configuration-does-not-carry",
            document: Document {
                ordered: vec![("/images/*".to_string(), "absent".to_string())],
                ..Document::default()
            },
            expect: refused(
                404,
                "NoSuchOrigin",
                "One or more of your cache behavior target origins or origin groups do not \
                 exist.",
            ),
        },
    ];
    // The alias quota's accepted boundary is stated here rather than in the literal above, so
    // the row carries the alternate domain names and the custom certificate that covers them.
    cases.push(Case {
        case: "aliases-at-the-quota",
        document: Document {
            aliases: (0..ALIAS_QUOTA)
                .map(|index| format!("held-{index}.example.com"))
                .collect(),
            ..Document::default()
        },
        expect: Expect::Accepted,
    });
    cases
}

/// Assert that `answer` is what `expect` says, and that a refusal announced no resource.
fn assert_answer(case: &str, answer: &Answer, expect: &Expect, context: &str) {
    match expect {
        Expect::Accepted => {
            assert_eq!(
                answer.status,
                if context == "CreateDistribution" { 201 } else { 200 },
                "case {case} ({context}): expected the configuration to be stored, got {} with \
                 {}",
                answer.status,
                answer.raw
            );
        }
        Expect::Refused {
            status,
            code,
            message,
        } => {
            assert_eq!(
                answer.status, *status,
                "case {case} ({context}): expected HTTP {status}, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                *code,
                "case {case} ({context}): expected the error code {code:?}, got {:?} in {}",
                answer.error_code(),
                answer.raw
            );
            let answered = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                answered, *message,
                "case {case} ({context}): expected the message {message:?}, got {answered:?}"
            );
            assert_eq!(
                answer.root().find_text("Type").unwrap_or_default(),
                "Sender",
                "case {case} ({context}): expected a Sender fault, got {}",
                answer.raw
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
async fn stored_count(router: &MultiServiceRouter) -> usize {
    let listed = list_distributions(router, "").await;
    listed
        .root()
        .find_text("Quantity")
        .unwrap_or_default()
        .parse()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_configuration_is_created_only_where_every_member_is_inside_its_documented_domain() {
    let router = emulator();
    let mut stored = 0usize;
    for (index, case) in cases().into_iter().enumerate() {
        let reference = format!("member-admission-{index}");
        let answer = create_distribution(&router, &case.document.xml(&reference)).await;
        assert_answer(case.case, &answer, &case.expect, "CreateDistribution");

        if matches!(case.expect, Expect::Accepted) {
            stored += 1;
            // The configuration the create stored is the one a later read answers with, so an
            // admitted document really is persisted rather than merely accepted.
            let id = answer.root().text_at("Id").unwrap_or_default().to_string();
            let read = get_distribution(&router, &id).await;
            assert_eq!(
                read.status, 200,
                "case {}: expected the stored distribution to be readable, got {} with {}",
                case.case, read.status, read.raw
            );
            assert_eq!(
                read.root().text_at("DistributionConfig/CallerReference"),
                Some(reference.as_str()),
                "case {}: expected the read to answer the configuration created under \
                 {reference:?}, got {}",
                case.case,
                read.raw
            );
            // A member the document stated comes back as submitted, so an admitted value is
            // stored rather than normalised on the way in.
            if let Some(object) = &case.document.default_root_object {
                assert_eq!(
                    read.root().text_at("DistributionConfig/DefaultRootObject"),
                    Some(object.as_str()),
                    "case {}: expected DefaultRootObject {object:?}, got {:?}",
                    case.case,
                    read.root().text_at("DistributionConfig/DefaultRootObject")
                );
            }
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

#[tokio::test]
async fn a_configuration_a_create_would_refuse_is_refused_on_the_update_path_too() {
    let router = emulator();
    let created = create_distribution(&router, &Document::default().xml("update-subject")).await;
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
        let document = case.document.xml("update-subject");
        let answer = update_distribution(&router, &id, &document, Some(&etag)).await;
        assert_answer(case.case, &answer, &case.expect, "UpdateDistribution");
        if matches!(case.expect, Expect::Accepted) {
            // An accepted update mints a fresh version token, which the next row lands on.
            etag = answer.header("etag").to_string();
            continue;
        }
        // A refused update leaves the stored configuration, and its version, exactly as they
        // were: the next row still lands on the version this one read.
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

#[tokio::test]
async fn a_behaviour_may_only_trust_a_key_group_the_account_holds() {
    let router = emulator();
    // The group the account holds, created through the key group surface's own calls.
    let keys = mint_public_keys(&router, "member-admission-key", 1).await;
    let body = format!(
        "<KeyGroupConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>member-admission-group</Name><Items><PublicKey>{}</PublicKey></Items>\
         </KeyGroupConfig>",
        keys.first().expect("one public key was minted")
    );
    let group = create_group(&router, &body).await;
    assert_eq!(
        group.status, 201,
        "case the held group: expected HTTP 201 from CreateKeyGroup, got {} with {}",
        group.status, group.raw
    );
    let held = group.root().find_text("Id").unwrap_or_default().to_string();

    let rows = [
        ("a group the account holds", held.clone(), Expect::Accepted),
        (
            "an id no key group holds",
            "f9a6a0b4-8d39-4a1e-9f67-1b0f7f5d2c3e".to_string(),
            refused(
                400,
                "TrustedKeyGroupDoesNotExist",
                "One or more of your trusted keygroups do not exist.",
            ),
        ),
        (
            "a value that is no key group id at all",
            "not-a-key-group".to_string(),
            refused(
                400,
                "TrustedKeyGroupDoesNotExist",
                "One or more of your trusted keygroups do not exist.",
            ),
        ),
    ];

    // One stored distribution the update rows land on, carrying no trusted key group of its
    // own.
    let created = create_distribution(&router, &Document::default().xml("trusted-subject")).await;
    assert_eq!(
        created.status, 201,
        "case the subject: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let subject = created.root().text_at("Id").unwrap_or_default().to_string();
    let mut etag = created.header("etag").to_string();

    for (index, (case, group, expect)) in rows.into_iter().enumerate() {
        let document = Document {
            trusted_key_groups: vec![group.clone()],
            ..Document::default()
        };
        let answer = create_distribution(&router, &document.xml(&format!("trusted-{index}"))).await;
        assert_answer(case, &answer, &expect, "CreateDistribution");

        // The same rule on the update path, against the stored distribution, under the version
        // it stands at now.
        let updated = update_distribution(
            &router,
            &subject,
            &Document {
                trusted_key_groups: vec![group],
                ..Document::default()
            }
            .xml("trusted-subject"),
            Some(&etag),
        )
        .await;
        match &expect {
            Expect::Accepted => {
                assert_eq!(
                    updated.status, 200,
                    "case {case} (UpdateDistribution): expected HTTP 200, got {} with {}",
                    updated.status, updated.raw
                );
                etag = updated.header("etag").to_string();
            }
            Expect::Refused { .. } => assert_answer(case, &updated, &expect, "UpdateDistribution"),
        }
    }
}

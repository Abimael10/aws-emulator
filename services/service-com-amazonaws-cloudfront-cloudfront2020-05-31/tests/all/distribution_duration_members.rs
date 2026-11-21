// SPDX-License-Identifier: CC0-1.0

//! The members of a `DistributionConfig` that state an AMOUNT OF TIME, end to end over HTTP.
//!
//! RULE: a member documented as an amount of time in seconds states a span, and no span is
//! negative — so a configuration stating a negative one is refused with `InvalidArgument` (HTTP
//! 400) naming that parameter as outside the range it admits, with nothing stored. The rule is
//! about the CONFIGURATION rather than about the call carrying one, so it holds on the create and
//! on the update alike.
//!
//! The members, each with the sentence that makes it a duration (from this service's Smithy
//! model — `smithy_query(query="shape", shape="CustomErrorResponse,CacheBehavior,Origin")` and
//! the same query for `CustomOriginConfig`):
//!
//! * `CustomErrorResponse$ErrorCachingMinTTL` — "the minimum amount of time, in seconds, that you
//!   want CloudFront to cache the HTTP status code specified in `ErrorCode`";
//! * a cache behaviour's `MinTTL`, `DefaultTTL` and `MaxTTL` — "the {minimum,default,maximum}
//!   amount of time that you want objects to stay in CloudFront caches";
//! * `CustomOriginConfig$OriginReadTimeout` and `$OriginKeepaliveTimeout` — "specifies how long,
//!   in seconds, CloudFront waits for a response from the origin" and "how long, in seconds,
//!   CloudFront persists its connection to the origin";
//! * `Origin$ConnectionTimeout` — "the number of seconds that CloudFront waits when trying to
//!   establish a connection to the origin".
//!
//! The rows span what the rule generalises over: every one of those members, each with a value
//! INSIDE its domain (accepted and read back) beside one step outside it and the furthest value
//! outside it there is, which is what says the refusal is about the sign rather than about one
//! number. The refusal sentence is a LITERAL here rather than the crate's own constant, so a
//! reworded refusal fails this suite.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    create_distribution, emulator, get_distribution, update_distribution, Answer, API_VERSION,
};

/// The wire code and status every refusal below carries.
const INVALID_ARGUMENT: &str = "InvalidArgument";
const BAD_REQUEST: u16 = 400;

/// WHICH member of the configuration one row varies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Member {
    /// `CustomErrorResponse$ErrorCachingMinTTL`.
    ErrorCachingMinTtl,
    /// The cache behaviour's own `MinTTL`, `DefaultTTL` and `MaxTTL`.
    MinTtl,
    DefaultTtl,
    MaxTtl,
    /// `CustomOriginConfig$OriginReadTimeout` and `$OriginKeepaliveTimeout`.
    OriginReadTimeout,
    OriginKeepaliveTimeout,
    /// `Origin$ConnectionTimeout`.
    ConnectionTimeout,
}

impl Member {
    /// The name the WIRE reports this member under, which is the name its refusal carries.
    fn parameter(self) -> &'static str {
        match self {
            Member::ErrorCachingMinTtl => "ErrorCachingMinTTL",
            Member::MinTtl => "MinTTL",
            Member::DefaultTtl => "DefaultTTL",
            Member::MaxTtl => "MaxTTL",
            Member::OriginReadTimeout => "OriginReadTimeout",
            Member::OriginKeepaliveTimeout => "OriginKeepaliveTimeout",
            Member::ConnectionTimeout => "ConnectionTimeout",
        }
    }

    /// A value inside the member's own domain: a span CloudFront documents for it.
    fn inside(self) -> i64 {
        match self {
            // "the minimum amount of time … that you want CloudFront to cache the HTTP status
            // code": zero asks for no caching of the error at all, which is a span.
            Member::ErrorCachingMinTtl => 0,
            Member::MinTtl => 0,
            Member::DefaultTtl => 86_400,
            Member::MaxTtl => 31_536_000,
            Member::OriginReadTimeout => 30,
            Member::OriginKeepaliveTimeout => 5,
            Member::ConnectionTimeout => 10,
        }
    }

    /// The values OUTSIDE it one row submits: one step past zero, and the furthest value of the
    /// member's own width — `ErrorCachingMinTTL` is a `long`, the rest are `integer`.
    fn outside(self) -> Vec<(&'static str, i64)> {
        let furthest = match self {
            Member::ErrorCachingMinTtl | Member::MinTtl | Member::DefaultTtl | Member::MaxTtl => {
                i64::MIN
            }
            _ => i64::from(i32::MIN),
        };
        vec![
            ("one second short of no time at all", -1),
            ("the furthest value outside the range", furthest),
        ]
    }

    /// The sentence a configuration stating a negative value for this member is refused with.
    fn refusal(self) -> String {
        format!(
            "The parameter {} is not within allowed range.",
            self.parameter()
        )
    }
}

/// One `DistributionConfig`: a custom origin and a default cache behaviour stating the legacy
/// per-behaviour TTLs, with `member` carrying `seconds` and every other duration member carrying
/// a value inside its own domain.
fn config_xml(reference: &str, member: Member, seconds: i64) -> String {
    let stated = |of: Member| {
        if of == member {
            seconds
        } else {
            of.inside()
        }
    };
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>primary.example.com</DomainName>\
         <ConnectionAttempts>3</ConnectionAttempts>\
         <ConnectionTimeout>{connection}</ConnectionTimeout>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         <OriginSslProtocols><Quantity>1</Quantity>\
         <Items><SslProtocol>TLSv1.2</SslProtocol></Items></OriginSslProtocols>\
         <OriginReadTimeout>{read}</OriginReadTimeout>\
         <OriginKeepaliveTimeout>{keepalive}</OriginKeepaliveTimeout>\
         </CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <ForwardedValues><QueryString>false</QueryString>\
         <Cookies><Forward>none</Forward></Cookies>\
         <Headers><Quantity>0</Quantity></Headers>\
         <QueryStringCacheKeys><Quantity>0</Quantity></QueryStringCacheKeys>\
         </ForwardedValues>\
         <MinTTL>{min}</MinTTL><DefaultTTL>{default}</DefaultTTL><MaxTTL>{max}</MaxTTL>\
         </DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>0</Quantity></CacheBehaviors>\
         <CustomErrorResponses><Quantity>1</Quantity><Items><CustomErrorResponse>\
         <ErrorCode>502</ErrorCode>\
         <ErrorCachingMinTTL>{error_caching}</ErrorCachingMinTTL>\
         </CustomErrorResponse></Items></CustomErrorResponses>\
         <Comment>the durations a configuration states</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        connection = stated(Member::ConnectionTimeout),
        read = stated(Member::OriginReadTimeout),
        keepalive = stated(Member::OriginKeepaliveTimeout),
        min = stated(Member::MinTtl),
        default = stated(Member::DefaultTtl),
        max = stated(Member::MaxTtl),
        error_caching = stated(Member::ErrorCachingMinTtl),
    )
}

/// Every member the rule covers, in the order the document carries them.
fn members() -> [Member; 7] {
    [
        Member::ErrorCachingMinTtl,
        Member::MinTtl,
        Member::DefaultTtl,
        Member::MaxTtl,
        Member::OriginReadTimeout,
        Member::OriginKeepaliveTimeout,
        Member::ConnectionTimeout,
    ]
}

/// Hold `answer` to being the refusal `member` earns, carrying nothing of a created resource.
fn assert_refused(case: &str, answer: &Answer, member: Member, call: &str) {
    assert_eq!(
        answer.status, BAD_REQUEST,
        "case {case:?} ({call}): expected HTTP {BAD_REQUEST}, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        INVALID_ARGUMENT,
        "case {case:?} ({call}): expected {INVALID_ARGUMENT}, got {}",
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message,
        member.refusal(),
        "case {case:?} ({call}): expected {:?}, got {message:?}",
        member.refusal()
    );
    assert_eq!(
        answer.header("location"),
        "",
        "case {case:?} ({call}): a refused call creates nothing, so it announces no location, \
         got {:?}",
        answer.header("location")
    );
}

/// How many distributions the account holds, which is what says a refused create stored nothing.
async fn stored_count(router: &MultiServiceRouter) -> usize {
    let listed = crate::distribution_lifecycle::list_distributions(router, "").await;
    listed
        .root()
        .find_text("Quantity")
        .unwrap_or_default()
        .parse()
        .unwrap_or_default()
}

/// `CreateDistribution`: a value inside each member's domain is stored and read back, and a
/// negative one is refused against that member by name with nothing added to the account.
#[tokio::test]
async fn a_negative_duration_is_refused_on_the_create() {
    let router = emulator();
    let mut expected_stored = 0;

    for member in members() {
        let parameter = member.parameter();

        // The value INSIDE the domain: stored, and read back as the configuration states it.
        let reference = format!("duration-{parameter}-inside");
        let accepted =
            create_distribution(&router, &config_xml(&reference, member, member.inside())).await;
        assert_eq!(
            accepted.status, 201,
            "case {parameter:?} inside its domain: expected HTTP 201, got {} with {}",
            accepted.status, accepted.raw
        );
        expected_stored += 1;
        let id = accepted.root().text_at("Id").unwrap_or_default().to_string();
        let read = get_distribution(&router, &id).await;
        let stored = read.root().find_text(parameter).unwrap_or_default();
        assert_eq!(
            stored,
            member.inside().to_string(),
            "case {parameter:?} inside its domain: expected the stored configuration to carry \
             {}, got {stored:?}",
            member.inside()
        );

        // And every value outside it: refused, with nothing stored.
        for (why, seconds) in member.outside() {
            let case = format!("{parameter} at {why} ({seconds})");
            let reference = format!("duration-{parameter}-{seconds}");
            let answer =
                create_distribution(&router, &config_xml(&reference, member, seconds)).await;
            assert_refused(&case, &answer, member, "CreateDistribution");
            let count = stored_count(&router).await;
            assert_eq!(
                count, expected_stored,
                "case {case:?}: expected the refused create to store nothing, so the account \
                 holds {expected_stored} distributions, got {count}"
            );
        }
    }
}

/// `UpdateDistribution`: the same rule, against a stored distribution — a refused update leaves
/// the configuration and its version exactly as they were, and a value inside the domain is
/// written.
#[tokio::test]
async fn a_negative_duration_is_refused_on_the_update() {
    let router = emulator();
    let reference = "duration-update-subject";
    let created = create_distribution(
        &router,
        &config_xml(reference, Member::MinTtl, Member::MinTtl.inside()),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "the subject distribution must be created, got {} with {}",
        created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    let mut etag = created.header("etag").to_string();

    for member in members() {
        let parameter = member.parameter();
        for (why, seconds) in member.outside() {
            let case = format!("{parameter} at {why} ({seconds}) on the update");
            let answer = update_distribution(
                &router,
                &id,
                &config_xml(reference, member, seconds),
                Some(&etag),
            )
            .await;
            assert_refused(&case, &answer, member, "UpdateDistribution");
            let read = get_distribution(&router, &id).await;
            assert_eq!(
                read.header("etag"),
                etag,
                "case {case:?}: expected the refused update to leave the version at {etag:?}, \
                 got {:?}",
                read.header("etag")
            );
        }

        // A value inside the domain IS written, under the version the refusals left standing.
        let accepted = update_distribution(
            &router,
            &id,
            &config_xml(reference, member, member.inside()),
            Some(&etag),
        )
        .await;
        assert_eq!(
            accepted.status, 200,
            "case {parameter:?} inside its domain on the update: expected HTTP 200, got {} with \
             {}",
            accepted.status, accepted.raw
        );
        etag = accepted.header("etag").to_string();
    }
}

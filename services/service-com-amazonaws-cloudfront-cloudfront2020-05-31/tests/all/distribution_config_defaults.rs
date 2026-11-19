// SPDX-License-Identifier: CC0-1.0

//! A `Distribution` payload reports the FULLY RESOLVED configuration, over HTTP.
//!
//! The rule every case here asserts: a `DistributionConfig` member a request leaves out
//! is not unset on the distribution — it takes the value the service applies, and
//! `CreateDistribution`, `GetDistribution` and `UpdateDistribution` all report the
//! resolved configuration. Resolution covers exactly three kinds of member:
//!
//! * a LIST WRAPPER the request omits is the wrapper stating `Quantity` 0;
//! * a member with a documented default takes that default (the origin's connection
//!   attempts and timeouts, an S3 origin's read timeout, a custom origin's read and
//!   keepalive timeouts and SSL protocols, the legacy TTLs, `PriceClass`, `HttpVersion`,
//!   `Logging`'s four `@default` members, the geo restriction type, the allowed and cached
//!   methods);
//! * a member whose absence has exactly one meaning takes that meaning
//!   (`DefaultRootObject`, `OriginPath`, `OriginAccessControlId`, `WebACLId`,
//!   `FieldLevelEncryptionId` and `ContinuousDeploymentPolicyId` name nothing;
//!   `OriginShield`, `GrpcConfig`, `SmoothStreaming`, `Compress` and `Staging` are off;
//!   the viewer certificate is CloudFront's own).
//!
//! A member that is a CHOICE the configuration has not made (a cache policy, a real-time
//! log configuration, a tenant or connection-mode setting) stays absent, which the
//! `absent` column of each row pins.
//!
//! The last two sections carry the same rule over the two members the SERVICE itself
//! settles rather than echoes: the `ViewerCertificate` settings CloudFront's own
//! certificate decides — including the `SSLSupportMethod` it serves that certificate
//! over, which follows how viewers reach the distribution — and the
//! `TenantConfig$ParameterDefinitions` a configuration declares. Each states the rule it
//! asserts where it begins.
//!
//! The table spans what the rule generalises over: the kind of origin (custom, S3), how a
//! behaviour states its cache key (the legacy `ForwardedValues` or a cache policy), how
//! many ordered cache behaviours and custom error responses there are, whether trusted key
//! groups are configured, and whether the request names the members the service would
//! otherwise supply at all. Every row is asserted on the create answer, on the read, on
//! the update answer and on the read after it — so resolution is also shown to be
//! IDEMPOTENT: re-submitting a configuration does not resolve it twice.
//!
//! The certificate rule has a second half — what a distribution on a CUSTOM certificate
//! reports — which the [`settled_members`] submodule asserts.

use crate::connection_mode_immutable::get_distribution_config;
use crate::distribution_lifecycle::{
    create_distribution, emulator, get_distribution, update_distribution, Answer, Node,
    API_VERSION,
};
// The key group surface's own calls: a behaviour may only trust a group the account HOLDS, so
// a row that states `TrustedKeyGroups` names one created through them.
use crate::key_group_lifecycle::{create_group, mint_public_keys};
use smithy_server_runtime::MultiServiceRouter;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::ops::types::S_S_L_SUPPORT_METHOD_VALUES;
use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    CERTIFICATE_SOURCE_ACM, CERTIFICATE_SOURCE_IAM,
    DEFAULT_CONNECTION_ATTEMPTS, DEFAULT_CONNECTION_TIMEOUT, DEFAULT_DEFAULT_TTL,
    DEFAULT_ERROR_CACHING_MIN_TTL, DEFAULT_HTTP_VERSION, DEFAULT_MAX_TTL, DEFAULT_MIN_TTL,
    DEFAULT_ORIGIN_KEEPALIVE_TIMEOUT, DEFAULT_ORIGIN_READ_TIMEOUT, DEFAULT_PRICE_CLASS,
    MAX_CACHE_BEHAVIORS, MAX_TRUSTED_KEY_GROUPS, MAX_TRUSTED_SIGNERS,
    MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE, SERVICE_ENDPOINT,
    SSL_SUPPORT_METHOD_DEDICATED_IP, SSL_SUPPORT_METHOD_SNI_ONLY,
};
/// The other half of the certificate rule the table below asserts: what a distribution on
/// a CUSTOM certificate reports. A SUBMODULE so this file keeps one concern per section,
/// and it reads this one's helpers through `use super::*` rather than copying them.
///
/// This file is itself declared with a `#[path]` (every suite of the one `all` target is),
/// so a child module of it carries one too: without it the module would be looked for
/// beside this file rather than under it.
#[path = "distribution_config_defaults/settled_members.rs"]
mod settled_members;

/// A SigV4 credential scope naming this service's signing name, which routes a request
/// here.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cloudfront/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// One custom origin, naming only the members `CustomOriginConfig` requires.
const MINIMAL_CUSTOM_ORIGIN: &str = "<Origins><Quantity>1</Quantity><Items><Origin>\
     <Id>primary</Id><DomainName>origin.example.com</DomainName>\
     <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
     <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
     </Origin></Items></Origins>";

/// A default cache behaviour naming only the members `DefaultCacheBehavior` requires,
/// plus `extra`.
fn minimal_behavior(extra: &str) -> String {
    format!(
        "<DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{extra}</DefaultCacheBehavior>"
    )
}

/// One row of the table: a configuration, and what the resolved configuration reports.
struct Row {
    case: &'static str,
    /// The `DistributionConfig` body this row submits.
    config: String,
    /// `(element path, text)` the resolved `DistributionConfig` must report.
    expected: Vec<(String, String)>,
    /// `(container path, member element, values)` — a resolved list, in order.
    lists: Vec<(&'static str, &'static str, Vec<&'static str>)>,
    /// Element paths the resolved configuration must NOT carry: a choice the
    /// configuration never made is not invented.
    absent: Vec<&'static str>,
    /// `ActiveTrustedKeyGroups`: `(Enabled, Quantity, the key groups in order)`.
    active_key_groups: (&'static str, &'static str, Vec<&'static str>),
}

/// One `(path, text)` expectation.
fn at(path: &str, text: impl Into<String>) -> (String, String) {
    (path.to_string(), text.into())
}

/// One `(path, text)` expectation whose text is a number the wire carries as text.
fn at_num(path: &str, value: i64) -> (String, String) {
    (path.to_string(), value.to_string())
}

/// A `DistributionConfig` body: `members` inside the element, under `reference`.
fn config_xml(reference: &str, members: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>{members}</DistributionConfig>"
    )
}

/// The distribution-level members the service supplies for a configuration that names
/// none of them.
fn distribution_level_defaults() -> Vec<(String, String)> {
    vec![
        at("Aliases/Quantity", "0"),
        at("DefaultRootObject", ""),
        at("OriginGroups/Quantity", "0"),
        at("CacheBehaviors/Quantity", "0"),
        at("CustomErrorResponses/Quantity", "0"),
        at("Logging/Enabled", "false"),
        at("Logging/IncludeCookies", "false"),
        at("Logging/Bucket", ""),
        at("Logging/Prefix", ""),
        at("PriceClass", DEFAULT_PRICE_CLASS),
        at("ViewerCertificate/CloudFrontDefaultCertificate", "true"),
        // The method the service supplies for a distribution served on CloudFront's own
        // certificate under a CloudFront domain name of its own, where no viewer has to
        // send the name it asked for.
        at(
            "ViewerCertificate/SSLSupportMethod",
            SSL_SUPPORT_METHOD_DEDICATED_IP,
        ),
        at("ViewerCertificate/MinimumProtocolVersion", MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE),
        at("ViewerCertificate/CertificateSource", "cloudfront"),
        at("Restrictions/GeoRestriction/RestrictionType", "none"),
        at("Restrictions/GeoRestriction/Quantity", "0"),
        at("WebACLId", ""),
        at("HttpVersion", DEFAULT_HTTP_VERSION),
        at("IsIPV6Enabled", "true"),
        at("ContinuousDeploymentPolicyId", ""),
        at("Staging", "false"),
    ]
}

/// The members the service supplies on an origin, whatever kind of origin it is.
fn origin_defaults() -> Vec<(String, String)> {
    vec![
        at("Origins/Items/Origin/OriginPath", ""),
        at("Origins/Items/Origin/CustomHeaders/Quantity", "0"),
        at_num(
            "Origins/Items/Origin/ConnectionAttempts",
            i64::from(DEFAULT_CONNECTION_ATTEMPTS),
        ),
        at_num(
            "Origins/Items/Origin/ConnectionTimeout",
            i64::from(DEFAULT_CONNECTION_TIMEOUT),
        ),
        at("Origins/Items/Origin/OriginShield/Enabled", "false"),
        at("Origins/Items/Origin/OriginAccessControlId", ""),
    ]
}

/// The members the service supplies on the custom-origin configuration at `prefix`.
fn custom_origin_defaults(prefix: &str) -> Vec<(String, String)> {
    vec![
        at(&format!("{prefix}/OriginSslProtocols/Quantity"), "3"),
        at_num(
            &format!("{prefix}/OriginReadTimeout"),
            i64::from(DEFAULT_ORIGIN_READ_TIMEOUT),
        ),
        at_num(
            &format!("{prefix}/OriginKeepaliveTimeout"),
            i64::from(DEFAULT_ORIGIN_KEEPALIVE_TIMEOUT),
        ),
    ]
}

/// The members the service supplies on the cache behaviour at `prefix` when it names none
/// of them.
fn behavior_defaults(prefix: &str) -> Vec<(String, String)> {
    vec![
        at(&format!("{prefix}/TrustedSigners/Enabled"), "false"),
        at(&format!("{prefix}/TrustedSigners/Quantity"), "0"),
        at(&format!("{prefix}/AllowedMethods/Quantity"), "2"),
        at(
            &format!("{prefix}/AllowedMethods/CachedMethods/Quantity"),
            "2",
        ),
        at(&format!("{prefix}/SmoothStreaming"), "false"),
        at(&format!("{prefix}/LambdaFunctionAssociations/Quantity"), "0"),
        at(&format!("{prefix}/FunctionAssociations/Quantity"), "0"),
        at(&format!("{prefix}/FieldLevelEncryptionId"), ""),
        at(&format!("{prefix}/GrpcConfig/Enabled"), "false"),
    ]
}

/// The table: one row per shape of configuration the rule has to hold for.
///
/// `key_group_id` is a key group THIS ACCOUNT HOLDS, created through `CreateKeyGroup` before
/// the table is built: a behaviour may only trust a group that exists, so the row that states
/// `TrustedKeyGroups` names a real one.
fn rows(key_group_id: &'static str) -> Vec<Row> {
    let mut rows = Vec::new();

    // A custom origin and a default cache behaviour, and nothing else: every resolvable
    // member comes from the service.
    let mut expected = distribution_level_defaults();
    expected.extend(origin_defaults());
    expected.extend(custom_origin_defaults(
        "Origins/Items/Origin/CustomOriginConfig",
    ));
    expected.extend(behavior_defaults("DefaultCacheBehavior"));
    expected.extend([
        at("DefaultCacheBehavior/TrustedKeyGroups/Enabled", "false"),
        at("DefaultCacheBehavior/TrustedKeyGroups/Quantity", "0"),
        at("DefaultCacheBehavior/Compress", "false"),
    ]);
    rows.push(Row {
        case: "minimal-custom-origin",
        config: config_xml(
            "defaults-minimal",
            &format!(
                "{MINIMAL_CUSTOM_ORIGIN}{}<Comment>minimal</Comment><Enabled>true</Enabled>",
                minimal_behavior("")
            ),
        ),
        expected,
        lists: vec![
            (
                "Origins/Items/Origin/CustomOriginConfig/OriginSslProtocols/Items",
                "SslProtocol",
                vec!["TLSv1", "TLSv1.1", "TLSv1.2"],
            ),
            (
                "DefaultCacheBehavior/AllowedMethods/Items",
                "Method",
                vec!["HEAD", "GET"],
            ),
            (
                "DefaultCacheBehavior/AllowedMethods/CachedMethods/Items",
                "Method",
                vec!["HEAD", "GET"],
            ),
        ],
        // The cache key, the policies and the multi-tenant members are choices this
        // configuration never made.
        absent: vec![
            "DefaultCacheBehavior/ForwardedValues",
            "DefaultCacheBehavior/MinTTL",
            "DefaultCacheBehavior/DefaultTTL",
            "DefaultCacheBehavior/MaxTTL",
            "DefaultCacheBehavior/CachePolicyId",
            "DefaultCacheBehavior/RealtimeLogConfigArn",
            "ConnectionMode",
            "AnycastIpListId",
            "TenantConfig",
        ],
        active_key_groups: ("false", "0", Vec::new()),
    });

    // An S3 origin: the origin-level members resolve the same way, and the S3 origin's own
    // response timeout takes the value the model documents for it — "the default (if you
    // don't specify otherwise) is 30 seconds" (`S3OriginConfig$OriginReadTimeout`), the same
    // response timeout a custom origin carries. The custom-origin members a bucket origin
    // has none of (the SSL protocols, the keepalive timeout) are not invented here.
    let mut expected = distribution_level_defaults();
    expected.extend(origin_defaults());
    expected.extend(behavior_defaults("DefaultCacheBehavior"));
    expected.push(at(
        "Origins/Items/Origin/S3OriginConfig/OriginAccessIdentity",
        "",
    ));
    expected.push(at_num(
        "Origins/Items/Origin/S3OriginConfig/OriginReadTimeout",
        i64::from(DEFAULT_ORIGIN_READ_TIMEOUT),
    ));
    rows.push(Row {
        case: "s3-origin",
        config: config_xml(
            "defaults-s3",
            "<Origins><Quantity>1</Quantity><Items><Origin><Id>bucket</Id>\
             <DomainName>bucket.s3.amazonaws.com</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>bucket</TargetOriginId>\
             <ViewerProtocolPolicy>redirect-to-https</ViewerProtocolPolicy></DefaultCacheBehavior>\
             <Comment>s3</Comment><Enabled>false</Enabled>",
        ),
        expected,
        lists: vec![(
            "DefaultCacheBehavior/AllowedMethods/Items",
            "Method",
            vec!["HEAD", "GET"],
        )],
        absent: vec![
            "Origins/Items/Origin/CustomOriginConfig",
            "Origins/Items/Origin/S3OriginConfig/OriginKeepaliveTimeout",
            "DefaultCacheBehavior/ForwardedValues",
            "DefaultCacheBehavior/DefaultTTL",
        ],
        active_key_groups: ("false", "0", Vec::new()),
    });

    // The same S3 origin STATING a response timeout of its own: a member the request names
    // is the one the distribution runs under and reports back, so resolution supplies a
    // default and never overrides a stated value. The value is inside the published range
    // the member documents ("The minimum timeout is 1 second, the maximum is 120 seconds")
    // and is not the default, which is what makes the row tell the two apart.
    let mut expected = distribution_level_defaults();
    expected.extend(origin_defaults());
    expected.extend(behavior_defaults("DefaultCacheBehavior"));
    expected.push(at(
        "Origins/Items/Origin/S3OriginConfig/OriginAccessIdentity",
        "",
    ));
    expected.push(at_num(
        "Origins/Items/Origin/S3OriginConfig/OriginReadTimeout",
        120,
    ));
    rows.push(Row {
        case: "s3-origin-stating-its-read-timeout",
        config: config_xml(
            "defaults-s3-read-timeout",
            "<Origins><Quantity>1</Quantity><Items><Origin><Id>bucket</Id>\
             <DomainName>bucket.s3.amazonaws.com</DomainName>\
             <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity>\
             <OriginReadTimeout>120</OriginReadTimeout></S3OriginConfig>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>bucket</TargetOriginId>\
             <ViewerProtocolPolicy>redirect-to-https</ViewerProtocolPolicy></DefaultCacheBehavior>\
             <Comment>s3 read timeout</Comment><Enabled>false</Enabled>",
        ),
        expected,
        lists: vec![(
            "DefaultCacheBehavior/AllowedMethods/Items",
            "Method",
            vec!["HEAD", "GET"],
        )],
        absent: vec![
            "Origins/Items/Origin/CustomOriginConfig",
            "Origins/Items/Origin/S3OriginConfig/OriginKeepaliveTimeout",
            "DefaultCacheBehavior/ForwardedValues",
            "DefaultCacheBehavior/DefaultTTL",
        ],
        active_key_groups: ("false", "0", Vec::new()),
    });

    // The legacy cache key: a behaviour that states it with `ForwardedValues` also gets
    // the forwarded-header and query-string wrappers and the three TTLs resolved.
    let mut expected = distribution_level_defaults();
    expected.extend(origin_defaults());
    expected.extend(custom_origin_defaults(
        "Origins/Items/Origin/CustomOriginConfig",
    ));
    expected.extend(behavior_defaults("DefaultCacheBehavior"));
    expected.extend([
        at(
            "DefaultCacheBehavior/ForwardedValues/Headers/Quantity",
            "0",
        ),
        at(
            "DefaultCacheBehavior/ForwardedValues/QueryStringCacheKeys/Quantity",
            "0",
        ),
        at_num("DefaultCacheBehavior/MinTTL", DEFAULT_MIN_TTL),
        at_num("DefaultCacheBehavior/DefaultTTL", DEFAULT_DEFAULT_TTL),
        at_num("DefaultCacheBehavior/MaxTTL", DEFAULT_MAX_TTL),
    ]);
    rows.push(Row {
        case: "legacy-forwarded-values",
        config: config_xml(
            "defaults-forwarded",
            &format!(
                "{MINIMAL_CUSTOM_ORIGIN}{}<Comment>forwarded</Comment><Enabled>true</Enabled>",
                minimal_behavior(
                    "<ForwardedValues><QueryString>false</QueryString>\
                     <Cookies><Forward>none</Forward></Cookies></ForwardedValues>"
                )
            ),
        ),
        expected,
        lists: Vec::new(),
        absent: vec!["DefaultCacheBehavior/CachePolicyId"],
        active_key_groups: ("false", "0", Vec::new()),
    });

    // A cache policy states the cache key instead, so the legacy group stays absent
    // whole — the service resolves no TTL a configuration did not ask for.
    let mut expected = distribution_level_defaults();
    expected.extend(origin_defaults());
    expected.extend(behavior_defaults("DefaultCacheBehavior"));
    expected.push(at(
        "DefaultCacheBehavior/CachePolicyId",
        "658327ea-f89d-4fab-a63d-7e88639e58f6",
    ));
    rows.push(Row {
        case: "cache-policy",
        config: config_xml(
            "defaults-cache-policy",
            &format!(
                "{MINIMAL_CUSTOM_ORIGIN}{}<Comment>policy</Comment><Enabled>true</Enabled>",
                minimal_behavior(
                    "<CachePolicyId>658327ea-f89d-4fab-a63d-7e88639e58f6</CachePolicyId>"
                )
            ),
        ),
        expected,
        lists: Vec::new(),
        absent: vec![
            "DefaultCacheBehavior/ForwardedValues",
            "DefaultCacheBehavior/MinTTL",
            "DefaultCacheBehavior/DefaultTTL",
            "DefaultCacheBehavior/MaxTTL",
        ],
        active_key_groups: ("false", "0", Vec::new()),
    });

    // Two ordered cache behaviours, two custom error responses (one naming its caching
    // TTL, one leaving it to the service) and a trusted key group: the resolution reaches
    // every behaviour and every error response, and the active key groups are read from
    // the configuration the distribution runs under.
    let mut expected: Vec<(String, String)> = distribution_level_defaults()
        .into_iter()
        // This row states both wrappers itself, so their quantities are its own.
        .filter(|(path, _)| {
            path != "CacheBehaviors/Quantity" && path != "CustomErrorResponses/Quantity"
        })
        .collect();
    expected.extend(origin_defaults());
    expected.extend(behavior_defaults("DefaultCacheBehavior"));
    expected.extend(behavior_defaults("CacheBehaviors/Items/CacheBehavior"));
    expected.extend([
        at("CacheBehaviors/Quantity", "2"),
        at("CacheBehaviors/Items/CacheBehavior/PathPattern", "/images/*"),
        at("CacheBehaviors/Items/CacheBehavior/Compress", "false"),
        at("CustomErrorResponses/Quantity", "2"),
        at("CustomErrorResponses/Items/CustomErrorResponse/ErrorCode", "404"),
        at_num(
            "CustomErrorResponses/Items/CustomErrorResponse/ErrorCachingMinTTL",
            DEFAULT_ERROR_CACHING_MIN_TTL,
        ),
        at("DefaultCacheBehavior/TrustedKeyGroups/Enabled", "true"),
        at("DefaultCacheBehavior/TrustedKeyGroups/Quantity", "1"),
    ]);
    rows.push(Row {
        case: "behaviours-and-error-responses",
        config: config_xml(
            "defaults-rich",
            &format!(
                "{MINIMAL_CUSTOM_ORIGIN}\
                 <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
                 <TrustedKeyGroups><Enabled>true</Enabled><Quantity>1</Quantity>\
                 <Items><KeyGroup>{key_group_id}</KeyGroup></Items>\
                 </TrustedKeyGroups>\
                 <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
                 <CacheBehaviors><Quantity>2</Quantity><Items>\
                 <CacheBehavior><PathPattern>/images/*</PathPattern>\
                 <TargetOriginId>primary</TargetOriginId>\
                 <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></CacheBehavior>\
                 <CacheBehavior><PathPattern>/api/*</PathPattern>\
                 <TargetOriginId>primary</TargetOriginId>\
                 <ViewerProtocolPolicy>https-only</ViewerProtocolPolicy>\
                 <Compress>true</Compress></CacheBehavior>\
                 </Items></CacheBehaviors>\
                 <CustomErrorResponses><Quantity>2</Quantity><Items>\
                 <CustomErrorResponse><ErrorCode>404</ErrorCode>\
                 <ResponsePagePath>/404.html</ResponsePagePath>\
                 <ResponseCode>404</ResponseCode></CustomErrorResponse>\
                 <CustomErrorResponse><ErrorCode>503</ErrorCode>\
                 <ErrorCachingMinTTL>30</ErrorCachingMinTTL></CustomErrorResponse>\
                 </Items></CustomErrorResponses>\
                 <Comment>rich</Comment><Enabled>true</Enabled>"
            ),
        ),
        expected,
        lists: vec![(
            "DefaultCacheBehavior/TrustedKeyGroups/Items",
            "KeyGroup",
            vec![key_group_id],
        )],
        absent: vec!["DefaultCacheBehavior/ForwardedValues"],
        active_key_groups: ("true", "1", vec![key_group_id]),
    });

    // Every member the service would otherwise supply, named by the request: resolution
    // ADDS, it never overwrites. `AllowedMethods` is named without `CachedMethods`, so
    // only the cached set is resolved; the certificate is an ACM one, so the deprecated
    // `Certificate` and `CertificateSource` follow it rather than CloudFront's own.
    let expected = vec![
        at("DefaultRootObject", "index.html"),
        at("Aliases/Quantity", "1"),
        at("PriceClass", "PriceClass_200"),
        at("HttpVersion", "http1.1"),
        at("IsIPV6Enabled", "false"),
        at("Staging", "false"),
        at("WebACLId", "a1b2c3d4-5678-90ab-cdef-EXAMPLE11111"),
        at("Logging/Enabled", "true"),
        at("Logging/IncludeCookies", "true"),
        at("Logging/Bucket", "logs.s3.amazonaws.com"),
        at("Logging/Prefix", "cdn/"),
        at("Restrictions/GeoRestriction/RestrictionType", "whitelist"),
        at("Restrictions/GeoRestriction/Quantity", "2"),
        at(
            "ViewerCertificate/ACMCertificateArn",
            "arn:aws:acm:us-east-1:000000000000:certificate/abc",
        ),
        at("ViewerCertificate/SSLSupportMethod", "sni-only"),
        at("ViewerCertificate/MinimumProtocolVersion", "TLSv1.2_2021"),
        at(
            "ViewerCertificate/Certificate",
            "arn:aws:acm:us-east-1:000000000000:certificate/abc",
        ),
        at("ViewerCertificate/CertificateSource", "acm"),
        at("Origins/Items/Origin/OriginPath", "/content"),
        at("Origins/Items/Origin/CustomHeaders/Quantity", "1"),
        at("Origins/Items/Origin/ConnectionAttempts", "1"),
        at("Origins/Items/Origin/ConnectionTimeout", "3"),
        at(
            "Origins/Items/Origin/CustomOriginConfig/OriginReadTimeout",
            "60",
        ),
        at(
            "Origins/Items/Origin/CustomOriginConfig/OriginKeepaliveTimeout",
            "30",
        ),
        at(
            "Origins/Items/Origin/CustomOriginConfig/OriginSslProtocols/Quantity",
            "1",
        ),
        at("Origins/Items/Origin/OriginShield/Enabled", "true"),
        at(
            "Origins/Items/Origin/OriginShield/OriginShieldRegion",
            "us-east-1",
        ),
        at("Origins/Items/Origin/OriginAccessControlId", ""),
        at("DefaultCacheBehavior/SmoothStreaming", "true"),
        at("DefaultCacheBehavior/Compress", "true"),
        at("DefaultCacheBehavior/AllowedMethods/Quantity", "3"),
        at(
            "DefaultCacheBehavior/AllowedMethods/CachedMethods/Quantity",
            "2",
        ),
        at("DefaultCacheBehavior/TrustedSigners/Enabled", "false"),
        at("DefaultCacheBehavior/GrpcConfig/Enabled", "false"),
        at("ContinuousDeploymentPolicyId", ""),
    ];
    rows.push(Row {
        case: "every-member-named",
        config: config_xml(
            "defaults-explicit",
            "<Aliases><Quantity>1</Quantity><Items><CNAME>cdn.example.com</CNAME></Items></Aliases>\
             <DefaultRootObject>index.html</DefaultRootObject>\
             <Origins><Quantity>1</Quantity><Items><Origin><Id>primary</Id>\
             <DomainName>origin.example.com</DomainName><OriginPath>/content</OriginPath>\
             <CustomHeaders><Quantity>1</Quantity><Items><OriginCustomHeader>\
             <HeaderName>x-origin</HeaderName><HeaderValue>yes</HeaderValue>\
             </OriginCustomHeader></Items></CustomHeaders>\
             <CustomOriginConfig><HTTPPort>8080</HTTPPort><HTTPSPort>8443</HTTPSPort>\
             <OriginProtocolPolicy>match-viewer</OriginProtocolPolicy>\
             <OriginSslProtocols><Quantity>1</Quantity><Items><SslProtocol>TLSv1.2</SslProtocol>\
             </Items></OriginSslProtocols>\
             <OriginReadTimeout>60</OriginReadTimeout>\
             <OriginKeepaliveTimeout>30</OriginKeepaliveTimeout></CustomOriginConfig>\
             <ConnectionAttempts>1</ConnectionAttempts><ConnectionTimeout>3</ConnectionTimeout>\
             <OriginShield><Enabled>true</Enabled>\
             <OriginShieldRegion>us-east-1</OriginShieldRegion></OriginShield>\
             </Origin></Items></Origins>\
             <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
             <ViewerProtocolPolicy>https-only</ViewerProtocolPolicy>\
             <AllowedMethods><Quantity>3</Quantity><Items><Method>HEAD</Method>\
             <Method>GET</Method><Method>OPTIONS</Method></Items></AllowedMethods>\
             <SmoothStreaming>true</SmoothStreaming><Compress>true</Compress>\
             </DefaultCacheBehavior>\
             <Comment>explicit</Comment>\
             <Logging><Enabled>true</Enabled><IncludeCookies>true</IncludeCookies>\
             <Bucket>logs.s3.amazonaws.com</Bucket><Prefix>cdn/</Prefix></Logging>\
             <PriceClass>PriceClass_200</PriceClass><Enabled>true</Enabled>\
             <ViewerCertificate>\
             <ACMCertificateArn>arn:aws:acm:us-east-1:000000000000:certificate/abc\
             </ACMCertificateArn><SSLSupportMethod>sni-only</SSLSupportMethod>\
             <MinimumProtocolVersion>TLSv1.2_2021</MinimumProtocolVersion></ViewerCertificate>\
             <Restrictions><GeoRestriction><RestrictionType>whitelist</RestrictionType>\
             <Quantity>2</Quantity><Items><Location>DE</Location><Location>FR</Location>\
             </Items></GeoRestriction></Restrictions>\
             <WebACLId>a1b2c3d4-5678-90ab-cdef-EXAMPLE11111</WebACLId>\
             <HttpVersion>http1.1</HttpVersion><IsIPV6Enabled>false</IsIPV6Enabled>\
             <Staging>false</Staging>",
        ),
        expected,
        lists: vec![
            (
                "Origins/Items/Origin/CustomOriginConfig/OriginSslProtocols/Items",
                "SslProtocol",
                vec!["TLSv1.2"],
            ),
            (
                "DefaultCacheBehavior/AllowedMethods/Items",
                "Method",
                vec!["HEAD", "GET", "OPTIONS"],
            ),
            (
                "DefaultCacheBehavior/AllowedMethods/CachedMethods/Items",
                "Method",
                vec!["HEAD", "GET"],
            ),
            (
                "Restrictions/GeoRestriction/Items",
                "Location",
                vec!["DE", "FR"],
            ),
        ],
        // CloudFront's own certificate is not in use here, so nothing claims it is.
        absent: vec![
            "ViewerCertificate/CloudFrontDefaultCertificate",
            "ViewerCertificate/IAMCertificateId",
        ],
        active_key_groups: ("false", "0", Vec::new()),
    });

    rows
}

/// Assert that one `DistributionConfig` element is this row's resolved configuration.
fn assert_resolved(row: &Row, config: &Node, context: &str, raw: &str) {
    let case = row.case;
    for (path, expected) in &row.expected {
        let actual = config.text_at(path);
        assert_eq!(
            actual,
            Some(expected.as_str()),
            "case {case} ({context}): expected {path} {expected:?}, got {actual:?} in {raw}"
        );
    }
    for (container, member, values) in &row.lists {
        let actual: Vec<String> = config
            .at(container)
            .map(|items| {
                items
                    .children_named(member)
                    .into_iter()
                    .map(|item| item.text().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            actual, *values,
            "case {case} ({context}): expected {container}/{member} {values:?}, got {actual:?}"
        );
    }
    for path in &row.absent {
        assert!(
            config.at(path).is_none(),
            "case {case} ({context}): expected no {path}, got one in {raw}"
        );
    }
}

/// Assert the `Distribution` reports the active signer and key-group sets, read from the
/// configuration it runs under.
fn assert_active_sets(row: &Row, distribution: &Node, context: &str) {
    let case = row.case;
    for (path, expected) in [
        ("ActiveTrustedSigners/Enabled", "false"),
        ("ActiveTrustedSigners/Quantity", "0"),
        ("ActiveTrustedKeyGroups/Enabled", row.active_key_groups.0),
        ("ActiveTrustedKeyGroups/Quantity", row.active_key_groups.1),
    ] {
        let actual = distribution.text_at(path);
        assert_eq!(
            actual,
            Some(expected),
            "case {case} ({context}): expected {path} {expected:?}, got {actual:?}"
        );
    }
    let groups: Vec<String> = distribution
        .at("ActiveTrustedKeyGroups/Items")
        .map(|items| {
            items
                .children_named("KeyGroup")
                .into_iter()
                .filter_map(|group| group.text_at("KeyGroupId").map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        groups, row.active_key_groups.2,
        "case {case} ({context}): expected the active key groups {:?}, got {groups:?}",
        row.active_key_groups.2
    );
}

/// The `DistributionConfig` element of a `Distribution` answer.
fn config_of<'a>(answer: &'a Answer, case: &str, context: &str) -> &'a Node {
    answer.root().at("DistributionConfig").unwrap_or_else(|| {
        panic!(
            "case {case} ({context}): the answer carries no DistributionConfig: {}",
            answer.raw
        )
    })
}

/// Create one key group naming one public key `CreatePublicKey` minted, and report the id
/// CloudFront allocated it — as a `&'static str`, which is what the table's rows hold.
///
/// A cache behaviour may only trust a key group the account HOLDS, so a row that states
/// `TrustedKeyGroups` names a group created here through the key group surface's own calls
/// rather than an id written out by hand.
async fn held_key_group(router: &MultiServiceRouter, name: &str) -> &'static str {
    let keys = mint_public_keys(router, name, 1).await;
    let body = format!(
        "<KeyGroupConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name><Items><PublicKey>{}</PublicKey></Items></KeyGroupConfig>",
        keys.first().expect("one public key was minted")
    );
    let answer = create_group(router, &body).await;
    assert_eq!(
        answer.status, 201,
        "case {name}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
        answer.status, answer.raw
    );
    let id = answer.root().find_text("Id").unwrap_or_default().to_string();
    assert!(
        !id.is_empty(),
        "case {name}: expected a minted key group id, got {}",
        answer.raw
    );
    Box::leak(id.into_boxed_str())
}

#[tokio::test]
async fn every_payload_reports_the_resolved_configuration() {
    let router = emulator();
    let key_group_id = held_key_group(&router, "defaults-trusted-group").await;
    for row in rows(key_group_id) {
        let case = row.case;

        let created = create_distribution(&router, &row.config).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected the create to be accepted, got {} ({})",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();
        let etag = created.header("etag").to_string();
        assert_resolved(
            &row,
            config_of(&created, case, "CreateDistribution"),
            "CreateDistribution",
            &created.raw,
        );
        assert_active_sets(&row, created.root(), "CreateDistribution");

        // The read answers from the stored record, so it reports the same resolved
        // configuration the create did.
        let read = get_distribution(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the read to succeed, got {} ({})",
            read.status, read.raw
        );
        assert_resolved(
            &row,
            config_of(&read, case, "GetDistribution"),
            "GetDistribution",
            &read.raw,
        );
        assert_active_sets(&row, read.root(), "GetDistribution");

        // Re-submitting the SAME configuration resolves it to the same thing: the
        // resolution is idempotent, so a read-modify-write round trip neither loses the
        // service-applied members nor changes them. An update submits the configuration
        // WHOLE, so where the create's document left the price class to the service the
        // update states the class the create resolved — a document that leaves it out is no
        // complete configuration for an update.
        let submitted = if row.config.contains("<PriceClass>") {
            row.config.to_string()
        } else {
            row.config.replace(
                "<Enabled>",
                &format!("<PriceClass>{DEFAULT_PRICE_CLASS}</PriceClass><Enabled>"),
            )
        };
        let updated = update_distribution(&router, &id, &submitted, Some(&etag)).await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected the update to be accepted, got {} ({})",
            updated.status, updated.raw
        );
        assert_resolved(
            &row,
            config_of(&updated, case, "UpdateDistribution"),
            "UpdateDistribution",
            &updated.raw,
        );
        assert_active_sets(&row, updated.root(), "UpdateDistribution");

        let reread = get_distribution(&router, &id).await;
        assert_resolved(
            &row,
            config_of(&reread, case, "GetDistribution after the update"),
            "GetDistribution after the update",
            &reread.raw,
        );
        assert_active_sets(&row, reread.root(), "GetDistribution after the update");
    }
}

/// One row of the certificate-and-tenancy table: a configuration, the two
/// `ViewerCertificate` members the service decides for it, and the `TenantConfig` the
/// resolved configuration reports.
struct TenancyRow {
    case: &'static str,
    /// The members this row adds after `Enabled`.
    extra: String,
    /// The `SSLSupportMethod` the resolved configuration reports.
    ssl_support_method: String,
    /// The `MinimumProtocolVersion` the resolved configuration reports.
    minimum_protocol_version: String,
    /// Whether the resolved configuration carries a `TenantConfig` element at all.
    tenant_config: bool,
    /// The parameter names `TenantConfig$ParameterDefinitions` reports, or `None` when the
    /// resolved configuration carries no `ParameterDefinitions` element.
    parameter_definitions: Option<Vec<&'static str>>,
}

/// A `ViewerCertificate` served on CloudFront's own certificate, naming whichever of the
/// method and the security policy the row names.
fn default_certificate(method: Option<&str>, policy: Option<&str>) -> String {
    let method = method
        .map(|method| format!("<SSLSupportMethod>{method}</SSLSupportMethod>"))
        .unwrap_or_default();
    let policy = policy
        .map(|policy| format!("<MinimumProtocolVersion>{policy}</MinimumProtocolVersion>"))
        .unwrap_or_default();
    format!(
        "<ViewerCertificate><CloudFrontDefaultCertificate>true\
         </CloudFrontDefaultCertificate>{method}{policy}</ViewerCertificate>"
    )
}

/// A `TenantConfig` declaring one definition per name in `names` — a `ParameterDefinitions`
/// declaring NOTHING when `names` is empty. `ParameterDefinitions`' member carries no
/// `xmlName` (`smithy_query(query="shape",shape="ParameterDefinitions")`), so each entry
/// travels in the restXml default `<member>` element.
fn tenant_config(names: &[&str]) -> String {
    let mut out = String::from("<TenantConfig><ParameterDefinitions>");
    for name in names {
        out.push_str(&format!(
            "<member><Name>{name}</Name><Definition><StringSchema>\
             <Required>false</Required></StringSchema></Definition></member>"
        ));
    }
    out.push_str("</ParameterDefinitions></TenantConfig>");
    out
}

/// The security policy a declared one is overridden by, and the one a configuration that
/// declared none is served under.
const DECLARED_POLICY: &str = "TLSv1.2_2021";

/// `ConnectionMode` `tenant-only` — the member that selects a multi-tenant distribution.
const TENANT_ONLY: &str = "<ConnectionMode>tenant-only</ConnectionMode>";

/// The rows the certificate-and-tenancy rule is asserted over.
fn tenancy_rows() -> Vec<TenancyRow> {
    // The method column spans every value the model's own catalog admits
    // (`ops::types::S_S_L_SUPPORT_METHOD_VALUES`, the values
    // `smithy_query(query="shape",shape="SSLSupportMethod")` reports), each named
    // explicitly, beside the two ways of naming none.
    let standard = |case: &'static str, extra: String, method: &str, policy: &str| TenancyRow {
        case,
        extra,
        ssl_support_method: method.to_string(),
        minimum_protocol_version: policy.to_string(),
        tenant_config: false,
        parameter_definitions: None,
    };
    let mut rows = vec![
        standard(
            "no ViewerCertificate at all",
            String::new(),
            SSL_SUPPORT_METHOD_DEDICATED_IP,
            MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE,
        ),
        standard(
            "CloudFront's own certificate, neither member named",
            default_certificate(None, None),
            SSL_SUPPORT_METHOD_DEDICATED_IP,
            MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE,
        ),
        standard(
            "a standard distribution declaring a security policy, which is overridden",
            default_certificate(None, Some(DECLARED_POLICY)),
            SSL_SUPPORT_METHOD_DEDICATED_IP,
            MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE,
        ),
    ];
    for method in S_S_L_SUPPORT_METHOD_VALUES {
        rows.push(standard(
            "a method the request names",
            default_certificate(Some(method), None),
            method,
            MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE,
        ));
    }
    // The tenancy column: a multi-tenant configuration has no CloudFront domain name of its
    // own, so it is served over the SNI-only method rather than the dedicated-address one,
    // and a security policy it DECLARED is the one it keeps, while one it declared none of
    // is served under the same automatic policy a standard distribution is. Its
    // `TenantConfig` is reported as the request stated it, and a `ParameterDefinitions`
    // carrying no entry declares no parameter, so no payload reports that element.
    rows.push(TenancyRow {
        case: "a TenantConfig declaring two parameters",
        extra: format!("{TENANT_ONLY}{}", tenant_config(&["stage", "region-label"])),
        ssl_support_method: SSL_SUPPORT_METHOD_SNI_ONLY.to_string(),
        minimum_protocol_version: MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE.to_string(),
        tenant_config: true,
        parameter_definitions: Some(vec!["stage", "region-label"]),
    });
    rows.push(TenancyRow {
        case: "a multi-tenant distribution declaring a security policy, which it keeps",
        extra: format!(
            "{}{TENANT_ONLY}{}",
            default_certificate(None, Some(DECLARED_POLICY)),
            tenant_config(&["stage"])
        ),
        ssl_support_method: SSL_SUPPORT_METHOD_SNI_ONLY.to_string(),
        minimum_protocol_version: DECLARED_POLICY.to_string(),
        tenant_config: true,
        parameter_definitions: Some(vec!["stage"]),
    });
    rows.push(TenancyRow {
        case: "a TenantConfig with an empty ParameterDefinitions",
        extra: format!("{TENANT_ONLY}{}", tenant_config(&[])),
        ssl_support_method: SSL_SUPPORT_METHOD_SNI_ONLY.to_string(),
        minimum_protocol_version: MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE.to_string(),
        tenant_config: true,
        parameter_definitions: None,
    });
    rows.push(TenancyRow {
        case: "a TenantConfig carrying no ParameterDefinitions",
        extra: format!("{TENANT_ONLY}<TenantConfig></TenantConfig>"),
        ssl_support_method: SSL_SUPPORT_METHOD_SNI_ONLY.to_string(),
        minimum_protocol_version: MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE.to_string(),
        tenant_config: true,
        parameter_definitions: None,
    });
    rows.push(TenancyRow {
        case: "a multi-tenant configuration with no TenantConfig",
        extra: TENANT_ONLY.to_string(),
        ssl_support_method: SSL_SUPPORT_METHOD_SNI_ONLY.to_string(),
        minimum_protocol_version: MINIMUM_PROTOCOL_VERSION_DEFAULT_CERTIFICATE.to_string(),
        tenant_config: false,
        parameter_definitions: None,
    });
    rows
}

/// Assert one `DistributionConfig` reports this row's certificate members and its
/// `TenantConfig`.
fn assert_tenancy(row: &TenancyRow, config: &Node, context: &str, raw: &str) {
    let case = row.case;
    for (path, expected) in [
        (
            "ViewerCertificate/SSLSupportMethod",
            row.ssl_support_method.as_str(),
        ),
        (
            "ViewerCertificate/MinimumProtocolVersion",
            row.minimum_protocol_version.as_str(),
        ),
    ] {
        let actual = config.text_at(path);
        assert_eq!(
            actual,
            Some(expected),
            "case {case} ({context}): expected {path} {expected:?}, got {actual:?} in {raw}"
        );
    }
    assert_eq!(
        config.at("TenantConfig").is_some(),
        row.tenant_config,
        "case {case} ({context}): expected TenantConfig present={}, got the opposite in {raw}",
        row.tenant_config
    );
    match &row.parameter_definitions {
        None => assert!(
            config.at("TenantConfig/ParameterDefinitions").is_none(),
            "case {case} ({context}): expected no ParameterDefinitions element, got one in {raw}"
        ),
        Some(names) => {
            let declared: Vec<String> = config
                .at("TenantConfig/ParameterDefinitions")
                .map(|list| {
                    list.children
                        .iter()
                        .map(|entry| entry.text_at("Name").unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                declared, *names,
                "case {case} ({context}): expected the parameter definitions {names:?}, got \
                 {declared:?} in {raw}"
            );
        }
    }
}

/// The certificate a configuration is served over and the tenant configuration it declares
/// are the ones the REQUEST described: a configuration naming no `SSLSupportMethod` is
/// served over the method CloudFront serves its own certificate over for how that
/// distribution is reached — the dedicated-address one under a CloudFront domain name of
/// its own, the SNI-only one for a multi-tenant distribution reached through a connection
/// group — one naming a method gets exactly that method back, the security policy is the
/// automatic one for CloudFront's own certificate unless the distribution has no CloudFront
/// domain name of its own to override it, and a `ParameterDefinitions` declaring no
/// parameter is reported by no payload. Asserted on the create answer, on the read, and on
/// the answer to an update that submits the same configuration, so none of the three paths
/// invents a member of its own.
#[tokio::test]
async fn the_certificate_and_the_tenant_config_are_the_ones_the_request_described() {
    let router = emulator();
    for (index, row) in tenancy_rows().into_iter().enumerate() {
        let case = row.case;
        let config = config_xml(
            &format!("tenancy-{index}"),
            &format!(
                "{MINIMAL_CUSTOM_ORIGIN}{}<Comment>tenancy</Comment><Enabled>true</Enabled>{}",
                minimal_behavior(""),
                row.extra
            ),
        );

        let created = create_distribution(&router, &config).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected the create to be accepted, got {} ({})",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();
        let etag = created.header("etag").to_string();
        assert_tenancy(
            &row,
            config_of(&created, case, "CreateDistribution"),
            "CreateDistribution",
            &created.raw,
        );

        let read = get_distribution(&router, &id).await;
        assert_tenancy(
            &row,
            config_of(&read, case, "GetDistribution"),
            "GetDistribution",
            &read.raw,
        );

        // The configuration the read reports is one an update admits back unchanged, so
        // what the create resolved survives a read-modify-write round trip. An update
        // submits the configuration WHOLE, so a STANDARD distribution's update states the
        // price class the create resolved — a multi-tenant one has no price class at all.
        let submitted = if row.extra.contains("tenant-only") {
            config.clone()
        } else {
            config.replace(
                "<Enabled>",
                &format!("<PriceClass>{DEFAULT_PRICE_CLASS}</PriceClass><Enabled>"),
            )
        };
        let updated = update_distribution(&router, &id, &submitted, Some(&etag)).await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected the update to be accepted, got {} ({})",
            updated.status, updated.raw
        );
        assert_tenancy(
            &row,
            config_of(&updated, case, "UpdateDistribution"),
            "UpdateDistribution",
            &updated.raw,
        );
    }
}

#[tokio::test]
async fn the_counts_a_configuration_states_are_bounded_by_the_published_quotas() {
    // A stored configuration is walked on every payload that reports it, so the counts
    // driving that walk are bounded on the way in: AT each published quota a configuration
    // is accepted, one PAST it is refused with the error CloudFront models for that count.
    let router = emulator();
    // The key groups the two `TrustedKeyGroups` cases name: a behaviour may only trust a
    // group the account holds, so the ids come from `CreateKeyGroup` — one more than the
    // quota, which is what the case past it states.
    let mut groups: Vec<String> = Vec::new();
    for index in 0..=MAX_TRUSTED_KEY_GROUPS {
        groups.push(held_key_group(&router, &format!("quota-group-{index}")).await.to_string());
    }
    let cases: Vec<(String, String, Option<&str>)> = vec![
        (
            "cache-behaviors-at-the-quota".to_string(),
            cache_behaviors_xml(MAX_CACHE_BEHAVIORS),
            None,
        ),
        (
            "cache-behaviors-past-the-quota".to_string(),
            cache_behaviors_xml(MAX_CACHE_BEHAVIORS + 1),
            Some("TooManyCacheBehaviors"),
        ),
        (
            "trusted-signers-at-the-quota".to_string(),
            trusted_signers_xml(MAX_TRUSTED_SIGNERS),
            None,
        ),
        (
            "trusted-signers-past-the-quota".to_string(),
            trusted_signers_xml(MAX_TRUSTED_SIGNERS + 1),
            Some("TooManyTrustedSigners"),
        ),
        (
            "trusted-key-groups-at-the-quota".to_string(),
            trusted_key_groups_xml(&groups[..MAX_TRUSTED_KEY_GROUPS]),
            None,
        ),
        (
            "trusted-key-groups-past-the-quota".to_string(),
            trusted_key_groups_xml(&groups),
            Some("TooManyKeyGroupsAssociatedToDistribution"),
        ),
    ];
    for (index, (case, members, refusal)) in cases.into_iter().enumerate() {
        let config = config_xml(&format!("quota-{index}"), &members);
        let answer = create_distribution(&router, &config).await;
        match refusal {
            None => {
                assert_eq!(
                    answer.status, 201,
                    "case {case}: expected the create to be accepted, got {} ({})",
                    answer.status, answer.raw
                );
            }
            Some(code) => {
                assert_eq!(
                    answer.status, 400,
                    "case {case}: expected the create to be refused with 400, got {} ({})",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    code,
                    "case {case}: expected the error code {code:?}, got {:?}",
                    answer.error_code()
                );
            }
        }
    }
}

/// A configuration whose `CacheBehaviors` carries `count` behaviours, each on its own path
/// pattern.
fn cache_behaviors_xml(count: usize) -> String {
    let mut items = String::new();
    for index in 0..count {
        items.push_str(&format!(
            "<CacheBehavior><PathPattern>/path-{index}/*</PathPattern>\
             <TargetOriginId>primary</TargetOriginId>\
             <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></CacheBehavior>"
        ));
    }
    format!(
        "{MINIMAL_CUSTOM_ORIGIN}{}<CacheBehaviors><Quantity>{count}</Quantity><Items>{items}\
         </Items></CacheBehaviors><Comment>quota</Comment><Enabled>true</Enabled>",
        minimal_behavior("")
    )
}

/// A configuration whose default cache behaviour trusts `count` signer accounts.
fn trusted_signers_xml(count: usize) -> String {
    let mut items = String::new();
    for index in 0..count {
        items.push_str(&format!(
            "<AwsAccountNumber>{:012}</AwsAccountNumber>",
            index + 1
        ));
    }
    format!(
        "{MINIMAL_CUSTOM_ORIGIN}{}<Comment>quota</Comment><Enabled>true</Enabled>",
        minimal_behavior(&format!(
            "<TrustedSigners><Enabled>true</Enabled><Quantity>{count}</Quantity>\
             <Items>{items}</Items></TrustedSigners>"
        ))
    )
}

/// A configuration whose default cache behaviour trusts the key groups `ids` names.
fn trusted_key_groups_xml(ids: &[String]) -> String {
    let mut items = String::new();
    for id in ids {
        items.push_str(&format!("<KeyGroup>{id}</KeyGroup>"));
    }
    format!(
        "{MINIMAL_CUSTOM_ORIGIN}{}<Comment>quota</Comment><Enabled>true</Enabled>",
        minimal_behavior(&format!(
            "<TrustedKeyGroups><Enabled>true</Enabled><Quantity>{}</Quantity>\
             <Items>{items}</Items></TrustedKeyGroups>",
            ids.len()
        ))
    )
}

#[tokio::test]
async fn the_location_is_rooted_at_the_endpoint_the_request_reached() {
    // The `Location` is an address the CALLER then follows, so it is rooted at the endpoint
    // this emulator answered the create on: a request that reached the emulator's own
    // endpoint — which is the only address a client on this machine can dial — is answered
    // that same address, port included, and a caller that resolves AWS's own hostnames (a
    // request addressed to one of this service's published hosts, on an emulator served at
    // no address of its own) is answered the service's published endpoint.
    //
    // The rows span what the rule generalises over: the service's global host, its regional
    // spelling, and the emulator's own endpoint as a loopback NAME and as a loopback LITERAL,
    // each with a port of its own.
    let cases = [
        ("cloudfront.amazonaws.com", SERVICE_ENDPOINT.to_string()),
        ("cloudfront.us-east-1.amazonaws.com", SERVICE_ENDPOINT.to_string()),
        ("localhost:9297", "localhost:9297".to_string()),
        ("127.0.0.1:443", "127.0.0.1:443".to_string()),
    ];
    let router = emulator();
    for (index, (host, authority)) in cases.iter().enumerate() {
        let config = config_xml(
            &format!("location-{index}"),
            &format!(
                "{MINIMAL_CUSTOM_ORIGIN}{}<Comment>location</Comment><Enabled>true</Enabled>",
                minimal_behavior("")
            ),
        );
        let request = ::http::Request::builder()
            .method("POST")
            .uri(format!("/{API_VERSION}/distribution"))
            .header("host", *host)
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/xml")
            .body(::bytes::Bytes::from(config))
            .expect("the test request is well formed");
        let response = router.dispatch(request).await;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 201,
            "case host={host}: expected the create to be accepted, got {status} ({body})"
        );
        let id = body
            .split_once("<Id>")
            .and_then(|(_, rest)| rest.split_once("</Id>"))
            .map(|(id, _)| id.to_string())
            .unwrap_or_default();
        let expected = format!("https://{authority}/{API_VERSION}/distribution/{id}");
        assert_eq!(
            location, expected,
            "case host={host}: expected Location {expected:?}, got {location:?}"
        );
    }
}

/// One row of the certificate-method table: the certificate and tenancy members a request
/// carries, and the `SSLSupportMethod` every payload must then report.
struct MethodRow {
    case: &'static str,
    /// The `ViewerCertificate` element the request carries; empty for a request carrying
    /// none at all.
    certificate: String,
    /// The tenancy members the request carries; empty for a standard distribution.
    tenancy: String,
    /// Whether the configuration serves an alternate domain name of the caller's, which is
    /// what makes a certificate of the caller's the one that validates it.
    aliased: bool,
    /// The `SSLSupportMethod` the resolved configuration must report.
    expected: String,
}

/// An ACM certificate ARN, in the one Region CloudFront supports ACM certificates from
/// ("CloudFront only supports ACM certificates in the US East (N. Virginia) Region"). Read
/// by the [`settled_members`] submodule too, through its `use super::*`.
const ACM_ARN: &str = "arn:aws:acm:us-east-1:000000000000:certificate/\
     11111111-2222-3333-4444-555555555555";

/// An IAM server certificate id, within the 32-character maximum
/// `ViewerCertificate$IAMCertificateId` carries.
const IAM_ID: &str = "ASCAEXAMPLE1234567890";

/// A `ViewerCertificate` on a certificate of the caller's own, which states both members
/// the caller decides there: a distribution using `Aliases` names one of
/// `ACMCertificateArn` or `IAMCertificateId` together with the method viewers reach it
/// over, and the security policy it is served under. Read by the [`settled_members`]
/// submodule too, so the element is built in one place.
pub(crate) fn own_certificate(member: &str, value: &str, method: &str, policy: &str) -> String {
    format!(
        "<ViewerCertificate><{member}>{value}</{member}>\
         <SSLSupportMethod>{method}</SSLSupportMethod>\
         <MinimumProtocolVersion>{policy}</MinimumProtocolVersion></ViewerCertificate>"
    )
}

/// The `Aliases` element for one alternate domain name.
pub(crate) fn aliases(name: &str) -> String {
    format!("<Aliases><Quantity>1</Quantity><Items><CNAME>{name}</CNAME></Items></Aliases>")
}

/// The rows the certificate-method rule is asserted over.
fn method_rows() -> Vec<MethodRow> {
    let standard = |case: &'static str, certificate: String, expected: &str| MethodRow {
        case,
        certificate,
        tenancy: String::new(),
        aliased: false,
        expected: expected.to_string(),
    };
    let tenant = |case: &'static str, certificate: String, expected: &str| MethodRow {
        case,
        certificate,
        tenancy: TENANT_ONLY.to_string(),
        aliased: false,
        expected: expected.to_string(),
    };
    // A distribution on a certificate of the caller's own serves an alternate domain name,
    // which is what such a certificate validates.
    let own = |case: &'static str, certificate: String, expected: &str| MethodRow {
        case,
        certificate,
        tenancy: String::new(),
        aliased: true,
        expected: expected.to_string(),
    };
    let mut rows = vec![
        standard(
            "a standard distribution carrying no ViewerCertificate at all",
            String::new(),
            SSL_SUPPORT_METHOD_DEDICATED_IP,
        ),
        standard(
            "a standard distribution on CloudFront's own certificate",
            default_certificate(None, None),
            SSL_SUPPORT_METHOD_DEDICATED_IP,
        ),
        tenant(
            "a multi-tenant distribution carrying no ViewerCertificate at all",
            String::new(),
            SSL_SUPPORT_METHOD_SNI_ONLY,
        ),
        tenant(
            "a multi-tenant distribution on CloudFront's own certificate",
            default_certificate(None, None),
            SSL_SUPPORT_METHOD_SNI_ONLY,
        ),
        // A configuration whose tenancy is declared by its `TenantConfig` alone is
        // multi-tenant too, so it is served over the same method as one naming the mode.
        MethodRow {
            case: "a distribution whose TenantConfig alone declares its tenancy",
            certificate: default_certificate(None, None),
            tenancy: format!("{TENANT_ONLY}{}", tenant_config(&["stage"])),
            aliased: false,
            expected: SSL_SUPPORT_METHOD_SNI_ONLY.to_string(),
        },
    ];
    // Every method the model's own catalog admits
    // (`ops::types::S_S_L_SUPPORT_METHOD_VALUES`, the values
    // `smithy_query(query="shape",shape="SSLSupportMethod")` reports), named by the request
    // on each kind of certificate: a named method is the one reported back, whichever
    // certificate the configuration is served on and however viewers reach it.
    for method in S_S_L_SUPPORT_METHOD_VALUES {
        rows.push(standard(
            "a standard distribution naming the method itself",
            default_certificate(Some(method), None),
            method,
        ));
        rows.push(tenant(
            "a multi-tenant distribution naming the method itself",
            default_certificate(Some(method), None),
            method,
        ));
        rows.push(own(
            "an ACM certificate of the caller's own",
            own_certificate("ACMCertificateArn", ACM_ARN, method, DECLARED_POLICY),
            method,
        ));
        rows.push(own(
            "an IAM certificate of the caller's own",
            own_certificate("IAMCertificateId", IAM_ID, method, DECLARED_POLICY),
            method,
        ));
    }
    rows
}

/// RULE: `SSLSupportMethod` is not a member a configuration on CloudFront's own certificate
/// states — "If the distribution uses the CloudFront domain name such as
/// `d111111abcdef8.cloudfront.net`, don't set a value for this field" — so what the service
/// reports for such a configuration is the method it serves its own certificate over for HOW
/// viewers reach that distribution: a distribution answering under a CloudFront domain name
/// of its own is reached at CloudFront's own addresses, where no viewer has to send the name
/// it asked for, while a multi-tenant distribution has none of its own and is reached
/// through a connection group that answers for the tenant whose name the viewer sent. A
/// configuration that NAMES a method keeps exactly the one it named, and one served on a
/// certificate of the caller's own states the method itself.
///
/// The rows span what the rule generalises over: the tenancy (standard, multi-tenant by
/// `ConnectionMode`, multi-tenant by `TenantConfig`), how the default certificate is
/// declared (not at all, the member stated), every method the model's catalog admits when a
/// request names one, and both places a certificate of the caller's own is kept. Each row is
/// asserted on the create answer and on BOTH read paths, so no path invents a method of its
/// own.
#[tokio::test]
async fn the_default_certificate_is_served_over_the_method_the_distribution_is_reached_by() {
    let router = emulator();
    for (index, row) in method_rows().into_iter().enumerate() {
        let case = row.case;
        let multi_tenant = !row.tenancy.is_empty();
        // `PriceClass` "only supports standard distributions", so only a standard
        // configuration states it. A row on a certificate of the caller's own serves an
        // alternate domain name of its own, unique to the row so no two rows claim one
        // domain.
        let document = config_xml(
            &format!("certificate-method-{index}"),
            &format!(
                "{}{MINIMAL_CUSTOM_ORIGIN}{}<Comment>certificate method</Comment>{}\
                 <Enabled>true</Enabled>{}{}",
                if row.aliased {
                    aliases(&format!("method-{index}.example.com"))
                } else {
                    String::new()
                },
                minimal_behavior(""),
                if multi_tenant {
                    String::new()
                } else {
                    format!("<PriceClass>{DEFAULT_PRICE_CLASS}</PriceClass>")
                },
                row.certificate,
                row.tenancy,
            ),
        );

        let created = create_distribution(&router, &document).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected the create to be accepted, got {} ({})",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();
        let actual = config_of(&created, case, "CreateDistribution")
            .text_at("ViewerCertificate/SSLSupportMethod");
        assert_eq!(
            actual,
            Some(row.expected.as_str()),
            "case {case} (CreateDistribution): expected SSLSupportMethod {:?}, got {actual:?} \
             in {}",
            row.expected,
            created.raw
        );

        // Both read paths answer from the stored record, so both report the method the
        // create resolved.
        let read = get_distribution(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the read to succeed, got {} ({})",
            read.status, read.raw
        );
        let actual = config_of(&read, case, "GetDistribution")
            .text_at("ViewerCertificate/SSLSupportMethod");
        assert_eq!(
            actual,
            Some(row.expected.as_str()),
            "case {case} (GetDistribution): expected SSLSupportMethod {:?}, got {actual:?} in {}",
            row.expected,
            read.raw
        );

        let stored = get_distribution_config(&router, &id).await;
        assert_eq!(
            stored.status, 200,
            "case {case}: expected the configuration read to succeed, got {} ({})",
            stored.status, stored.raw
        );
        let actual = stored.root().text_at("ViewerCertificate/SSLSupportMethod");
        assert_eq!(
            actual,
            Some(row.expected.as_str()),
            "case {case} (GetDistributionConfig): expected SSLSupportMethod {:?}, got \
             {actual:?} in {}",
            row.expected,
            stored.raw
        );
    }
}

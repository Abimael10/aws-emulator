// SPDX-License-Identifier: CC0-1.0

//! The READ-MODIFY-WRITE cycle over a distribution's own configuration, end to end over
//! HTTP: `GetDistributionConfig`, change ONE member of the document it served, then
//! `UpdateDistribution` under the version that read reported.
//!
//! The rule under test: the document `GetDistributionConfig` serves is a document
//! `UpdateDistribution` admits, so a client that changes one member of it and resubmits it
//! under the reported version is answered 2xx and the change is what the next read reports —
//! and where the member it changed is one an update may not move, the refusal is the one
//! THAT reason names. Two halves make it hold, and the rows below exercise both: the service
//! materialises no default for a member the kind of distribution does not have (a cache
//! behaviour's `SmoothStreaming` and the configuration's `PriceClass` carry "This field only
//! supports standard distributions. You can't specify this field for multi-tenant
//! distributions" —
//! `smithy_query(query="shape",shape="DistributionConfig,DefaultCacheBehavior")`), so a
//! multi-tenant document states none of them; and the immutability rules are read before the
//! member-admission one, so flipping `ConnectionMode` is refused `IllegalUpdate` rather than
//! for some member the flip made inadmissible.
//!
//! The rows differ along what the rule generalises over: which KIND of distribution the
//! document describes (multi-tenant by `ConnectionMode`, multi-tenant by `TenantConfig`
//! alone, standard), which member of the served document is changed (a comment, a flag the
//! service itself supplied, an enum, a nested value inside an origin, a nested value inside
//! the tenant configuration) and, for the refusals, which immutable member is moved.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s and the tenancy document builder
//! [`crate::connection_mode_immutable`]'s, reused rather than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    DEFAULT_COMPRESS, DEFAULT_HTTP_VERSION, ILLEGAL_UPDATE_CONNECTION_MODE_MESSAGE,
    ILLEGAL_UPDATE_DISTRIBUTION_CALLER_REFERENCE_MESSAGE,
};

use crate::connection_mode_immutable::{
    get_distribution_config, TenancyConfig, DIRECT, TENANT_ONLY,
};
use crate::distribution_lifecycle::{create_distribution, emulator, update_distribution, Answer};

/// The comment every row's distribution is created with, so a row can find the member in
/// the served document and tell a stored change from the state the create left behind.
const SERVED_COMMENT: &str = "as created";

/// The one tenant parameter a multi-tenant row's document declares.
const TENANT_PARAMETER: &str = "tenantName";

/// The caller reference every row creates under.
const REFERENCE: &str = "served-config-round-trip";

// ---------------------------------------------------------------------------
// The kinds of distribution a served document describes
// ---------------------------------------------------------------------------

/// One kind of distribution the cycle is exercised on: how its document declares its
/// tenancy, and therefore which members the service supplies for it.
#[derive(Debug, Clone, Copy)]
struct Kind {
    case: &'static str,
    /// The `ConnectionMode` the create states, or `None` to leave the member out.
    connection_mode: Option<&'static str>,
    /// Whether the create declares a `TenantConfig`, the member that "only supports
    /// multi-tenant distributions".
    tenant_config: bool,
}

impl Kind {
    /// Whether this kind is a multi-tenant distribution, which is what decides whether the
    /// standard-only members are members of its configuration at all.
    fn multi_tenant(self) -> bool {
        self.connection_mode == Some(TENANT_ONLY) || self.tenant_config
    }

    /// The document the create submits for this kind.
    fn document(self) -> String {
        TenancyConfig {
            comment: SERVED_COMMENT.to_string(),
            parameters: self.tenant_config.then(|| vec![TENANT_PARAMETER]),
            connection_mode: self.connection_mode,
            ..TenancyConfig::new(REFERENCE)
        }
        .xml()
    }
}

/// The three kinds, spanning both tenancies and both ways a document declares a
/// multi-tenant one.
const KINDS: &[Kind] = &[
    Kind {
        case: "multi-tenant by ConnectionMode and TenantConfig",
        connection_mode: Some(TENANT_ONLY),
        tenant_config: true,
    },
    Kind {
        case: "multi-tenant by TenantConfig alone",
        connection_mode: None,
        tenant_config: true,
    },
    Kind {
        case: "standard",
        connection_mode: Some(DIRECT),
        tenant_config: false,
    },
];

// ---------------------------------------------------------------------------
// The one member a row changes
// ---------------------------------------------------------------------------

/// One change a client makes to the document the service served it.
#[derive(Debug, Clone)]
struct Mutation {
    case: String,
    /// The fragment the served document carries, which the row expects to state exactly
    /// once.
    find: String,
    /// What the row puts in its place.
    replace: String,
    /// Where the new value is read back, as a path of element names.
    path: &'static str,
    /// The value that path is expected to report afterwards.
    value: String,
    /// Whether the member is one only a MULTI-TENANT distribution's document carries.
    tenant_member: bool,
}

/// An element stating `value`.
fn element(name: &str, value: &str) -> String {
    format!("<{name}>{value}</{name}>")
}

/// The changes under test.
///
/// The fragments the service itself supplied are built from the values this service resolves
/// them to ([`DEFAULT_HTTP_VERSION`], [`DEFAULT_COMPRESS`]) rather than from a literal, so a
/// row states the member the service actually served. The enum values the rows move those
/// members onto come from the model's own value lists
/// (`smithy_query(query="shape",shape="HttpVersion,ViewerProtocolPolicy")`: `HttpVersion` is
/// one of `http1.1`, `http2`, `http3`, `http2and3`, and `ViewerProtocolPolicy` one of
/// `allow-all`, `https-only`, `redirect-to-https`), and `http2and3` and `redirect-to-https`
/// are values no row creates under.
fn mutations() -> Vec<Mutation> {
    vec![
        Mutation {
            case: "the comment".to_string(),
            find: element("Comment", SERVED_COMMENT),
            replace: element("Comment", "as updated"),
            path: "Comment",
            value: "as updated".to_string(),
            tenant_member: false,
        },
        Mutation {
            case: "the enabled flag".to_string(),
            find: element("Enabled", "true"),
            replace: element("Enabled", "false"),
            path: "Enabled",
            value: "false".to_string(),
            tenant_member: false,
        },
        Mutation {
            case: "the HTTP version the service supplied".to_string(),
            find: element("HttpVersion", DEFAULT_HTTP_VERSION),
            replace: element("HttpVersion", "http2and3"),
            path: "HttpVersion",
            value: "http2and3".to_string(),
            tenant_member: false,
        },
        Mutation {
            case: "the compression flag the service supplied".to_string(),
            find: element("Compress", &DEFAULT_COMPRESS.to_string()),
            replace: element("Compress", &(!DEFAULT_COMPRESS).to_string()),
            path: "DefaultCacheBehavior/Compress",
            value: (!DEFAULT_COMPRESS).to_string(),
            tenant_member: false,
        },
        Mutation {
            case: "the viewer protocol policy".to_string(),
            find: element("ViewerProtocolPolicy", "allow-all"),
            replace: element("ViewerProtocolPolicy", "redirect-to-https"),
            path: "DefaultCacheBehavior/ViewerProtocolPolicy",
            value: "redirect-to-https".to_string(),
            tenant_member: false,
        },
        Mutation {
            case: "the origin's domain name".to_string(),
            find: element("DomainName", "origin.example.com"),
            replace: element("DomainName", "second-origin.example.com"),
            path: "Origins/Items/Origin/DomainName",
            value: "second-origin.example.com".to_string(),
            tenant_member: false,
        },
        Mutation {
            case: "a tenant parameter's name".to_string(),
            find: element("Name", TENANT_PARAMETER),
            replace: element("Name", "tenantRegion"),
            path: "TenantConfig/ParameterDefinitions/member/Name",
            value: "tenantRegion".to_string(),
            tenant_member: true,
        },
    ]
}

/// `document` with `find` — which it must state exactly once — replaced by `replacement`.
fn replace_once(document: &str, find: &str, replacement: &str, case: &str) -> String {
    let occurrences = document.matches(find).count();
    assert_eq!(
        occurrences, 1,
        "case {case}: expected the served configuration to state {find} exactly once, got \
         {occurrences} occurrences in {document}"
    );
    document.replace(find, replacement)
}

/// Stand `kind` up and read its stored configuration back: the `(id, etag, document)` a
/// read-modify-write cycle starts from.
async fn served_configuration(
    router: &smithy_server_runtime::MultiServiceRouter,
    kind: Kind,
) -> (String, String, String) {
    let case = kind.case;
    let created = create_distribution(router, &kind.document()).await;
    assert_eq!(
        created.status, 201,
        "case {case}: expected HTTP 201 from the create, got {} with {}",
        created.status, created.raw
    );
    let id = created
        .root()
        .find_text("Id")
        .unwrap_or_else(|| panic!("case {case}: the create carries no Id: {}", created.raw))
        .to_string();

    let read = get_distribution_config(router, &id).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected HTTP 200 reading the configuration, got {} with {}",
        read.status, read.raw
    );

    // The half of the rule that makes the cycle possible at all: a member the kind of
    // distribution does not have is a member the service supplies no default for, so a
    // multi-tenant document states none of them — and a standard one, whose members they
    // are, states them.
    for member in ["SmoothStreaming", "PriceClass"] {
        let stated = read.raw.contains(&format!("<{member}>"));
        assert_eq!(
            stated,
            !kind.multi_tenant(),
            "case {case}: expected the served configuration to state {}{member}, got {}",
            if kind.multi_tenant() { "no " } else { "" },
            read.raw
        );
    }

    let etag = read.header("etag").to_string();
    assert!(
        !etag.is_empty(),
        "case {case}: expected the configuration read to report a version, got {}",
        read.raw
    );
    (id, etag, read.raw.clone())
}

/// A served configuration with one member changed is admitted back, and the change is what
/// the next read reports.
#[tokio::test]
async fn a_served_configuration_is_admitted_back_with_one_member_changed() {
    for kind in KINDS {
        for mutation in mutations() {
            if mutation.tenant_member && !kind.multi_tenant() {
                continue;
            }
            let case = format!("{}, changing {}", kind.case, mutation.case);
            // One emulator per row, so each cycle starts from a configuration no earlier row
            // has written to.
            let router = emulator();
            let (id, etag, served) = served_configuration(&router, *kind).await;

            let submitted = replace_once(&served, &mutation.find, &mutation.replace, &case);
            let written = update_distribution(&router, &id, &submitted, Some(&etag)).await;
            assert_eq!(
                written.status, 200,
                "case {case}: the configuration the service served, changed at one member, \
                 must be admitted back, got {} with {}",
                written.status, written.raw
            );

            let after = get_distribution_config(&router, &id).await;
            assert_eq!(
                after.status, 200,
                "case {case}: expected HTTP 200 reading the configuration back, got {} with {}",
                after.status, after.raw
            );
            let reported = after.root().text_at(mutation.path);
            assert_eq!(
                reported,
                Some(mutation.value.as_str()),
                "case {case}: expected {} to report {:?}, got {:?} in {}",
                mutation.path,
                mutation.value,
                reported,
                after.raw
            );
            // Nothing BUT that member moved: the stored configuration is the document the
            // client submitted, which is the served one with the change in it.
            assert_eq!(
                after.raw, submitted,
                "case {case}: expected the stored configuration to be the submitted document, \
                 got {}",
                after.raw
            );
            // And the cycle composes: the document the second read served is admitted back
            // too, under the version that read reported.
            let again = update_distribution(
                &router,
                &id,
                &after.raw,
                Some(after.header("etag")),
            )
            .await;
            assert_eq!(
                again.status, 200,
                "case {case}: the second cycle must be admitted back too, got {} with {}",
                again.status, again.raw
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The members a cycle may NOT move
// ---------------------------------------------------------------------------

/// One immutable member a row moves in the served document, and the refusal that move earns.
#[derive(Debug, Clone)]
struct Refusal {
    case: String,
    kind: Kind,
    find: String,
    replace: String,
    /// The wire error code the move is refused with.
    code: &'static str,
    /// The sentence it carries, which states the rule and nothing from the request.
    message: &'static str,
}

/// The moves under test: the connection mode, in both directions, and the caller reference,
/// on every kind.
fn refusals() -> Vec<Refusal> {
    let mut rows: Vec<Refusal> = Vec::new();
    for kind in KINDS {
        if let Some(mode) = kind.connection_mode {
            let other = if mode == TENANT_ONLY {
                DIRECT
            } else {
                TENANT_ONLY
            };
            rows.push(Refusal {
                case: format!("{}, moving ConnectionMode to {other}", kind.case),
                kind: *kind,
                find: element("ConnectionMode", mode),
                replace: element("ConnectionMode", other),
                code: "IllegalUpdate",
                message: ILLEGAL_UPDATE_CONNECTION_MODE_MESSAGE,
            });
        }
        rows.push(Refusal {
            case: format!("{}, moving CallerReference", kind.case),
            kind: *kind,
            find: element("CallerReference", REFERENCE),
            replace: element("CallerReference", "another-reference"),
            code: "IllegalUpdate",
            message: ILLEGAL_UPDATE_DISTRIBUTION_CALLER_REFERENCE_MESSAGE,
        });
    }
    rows
}

/// The wire message of a refusal, or `""`.
fn message_of(answer: &Answer) -> &str {
    answer.root().find_text("Message").unwrap_or_default()
}

/// A served configuration whose one changed member is one no update may move is refused for
/// THAT reason, and writes nothing.
#[tokio::test]
async fn a_served_configuration_changed_at_an_immutable_member_is_refused_for_that_member() {
    for row in refusals() {
        let case = row.case.clone();
        let router = emulator();
        let (id, etag, served) = served_configuration(&router, row.kind).await;

        let submitted = replace_once(&served, &row.find, &row.replace, &case);
        let refused = update_distribution(&router, &id, &submitted, Some(&etag)).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected HTTP 400, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            row.code,
            "case {case}: expected {}, got {:?} in {}",
            row.code,
            refused.error_code(),
            refused.raw
        );
        let message = message_of(&refused);
        assert_eq!(
            message, row.message,
            "case {case}: expected {:?}, got {message:?}",
            row.message
        );
        assert!(
            !message.contains(&id) && !message.contains(REFERENCE),
            "case {case}: expected a sentence carrying nothing from the request or the record, \
             got {message:?}"
        );

        // The refusal wrote nothing: the stored configuration and its version are the ones
        // the read served.
        let after = get_distribution_config(&router, &id).await;
        assert_eq!(
            after.status, 200,
            "case {case}: expected HTTP 200 reading the configuration back, got {} with {}",
            after.status, after.raw
        );
        assert_eq!(
            after.raw, served,
            "case {case}: expected the stored configuration to be untouched, got {}",
            after.raw
        );
        assert_eq!(
            after.header("etag"),
            etag,
            "case {case}: expected the version token to be untouched, got {:?}",
            after.header("etag")
        );
    }
}

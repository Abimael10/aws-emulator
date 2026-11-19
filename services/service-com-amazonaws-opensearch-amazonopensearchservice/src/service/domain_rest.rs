// SPDX-License-Identifier: CC0-1.0

//! The DOMAIN's own REST data plane: how a request a client addresses to the endpoint a
//! domain of this service published is recognised, and how it is answered out of the
//! indexes, templates and security configuration that domain holds.
//!
//! A domain this service creates publishes the host a client sends index, search and
//! cluster requests to — `DomainStatus$Endpoint`, "the domain-specific endpoint used to
//! submit index, search, and data upload requests" — and that host is NOT this API's
//! endpoint: its first label is the domain's own, and the path it carries names no modelled
//! operation. Both halves of the front door's resolution therefore missed it, and every
//! signed request to the one address this service told the caller to use was answered `404
//! UnknownService`. Nothing served a domain's own traffic at all, so a client that
//! provisions a domain and then polls it waits for an answer that never comes.
//!
//! So this module is the second half of [`crate::service::EmulatedService`]: the claim
//! ([`claims_parts`], read by `Service::can_handle`) and the serving ([`serve`], reached
//! from `Service::override_operation` for a request carrying a SigV4 credential — so the
//! router verifies that credential before anything is served — and from
//! `Service::override_routing_error` for one carrying none), plus the serving of a request
//! that arrived on the LISTENER one domain publishes ([`serve_published`]), which the core
//! hands this module rather than the router.
//!
//! # What a request addresses
//!
//! A client on the machine this emulator runs on resolves a domain's endpoint host nowhere
//! at all, so it reaches the emulator and carries what it was told; three addressings
//! resolve:
//!
//! * THE PUBLISHED ADDRESS, which is what `DomainStatus$Endpoint` carries: the `host:port` of
//!   the listener this emulator opened for that one domain
//!   ([`crate::state::domain_plane`]), where the ADDRESS names the domain and the whole path
//!   of the request stays the client's. That is the form the member has to have, because a
//!   client takes the value as the HOST it is — it signs it, hands it to an OpenSearch client
//!   as one, and joins an absolute request path onto it. Such a request arrives on the
//!   domain's own socket rather than at this front door ([`serve_published`]); it is resolved
//!   HERE as well, by the authority it addresses, so an in-process client handed that address
//!   reaches the domain too;
//! * PREFIXED PATH-STYLE: the data plane is named by the reserved namespace segment
//!   (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`) and
//!   [`DATA_PLANE_SEGMENT`], then the domain's name or its endpoint host, then the request
//!   path — `/_aws/es/{domain-or-host}/{path}`. That segment begins with `_`, which no
//!   modelled path segment of any service does, so the prefix collides with no
//!   control-plane route. It is what a caller inside this emulator uses, and what a domain
//!   publishes when no listener of its own could be opened (see
//!   `crate::state::published_domain_address`);
//! * HOST-STYLE, the addressing a client that resolves AWS's own hosts uses: the request's
//!   host IS a host some domain of this store publishes — the public host AWS names its
//!   endpoint (`crate::state::domain_endpoint`), the dual-stack host
//!   `DomainStatus$EndpointV2` reports beside it, or one of the VPC-access hosts
//!   `DomainStatus$Endpoints` reports — and the whole path is the request path.
//!
//! Either way the claim is narrowed to a domain this service HOLDS, read from the one
//! shared store, so one request keeps one claimant — this service's signing name is shared
//! with the legacy Amazon Elasticsearch Service model, whose own modelled path space is
//! disjoint from both addressings above.
//!
//! Whichever addressing a request arrived under, the names its path carries are the VALUES
//! its segments stand for and not the text they arrived as: a path segment travels
//! percent-encoded, so an index name or a document id holding a character that would divide
//! the path is read from the decoded segment ([`path_segments`]) — which is what a client
//! indexing documents keyed by anything hierarchical addresses them by.
//!
//! # What it answers, and what decides it
//!
//! Every status, header and byte is computed from what a create, an update or a write
//! persisted: the cluster banner and the cluster health from the domain's own
//! configuration, an index's settings and mappings from the index record, a search and a
//! count from the documents the index holds. A read writes NOTHING; the writes are the
//! requests that are writes — an index create, a document write, a template or a security
//! record.
//!
//! One AUTHENTICATION decision and two authorization decisions, in the order AWS takes
//! them, and none of them re-implemented here:
//!
//! 0. The CLUSTER's own authentication, on a domain with fine-grained access control
//!    switched on: such a cluster admits nobody anonymously, and a request presenting HTTP
//!    Basic credentials is admitted only as a user its own INTERNAL USER DATABASE holds, at
//!    that user's password — the one decision [`crate::state::fgac::authentication`] owns.
//!    A request it refuses is answered `401`, and never reaches either decision below; one
//!    it admits is decided for the user it authenticated AS, which is why the security
//!    plugin's own records ([`security_records`], [`internal_users`]) are reachable with a
//!    Basic credential and not only with an AWS one.
//! 1. IAM, at the service's front door: the principal's identity policies and the domain's
//!    own access policy, against the data-plane action the request's METHOD names
//!    (`es:ESHttpGet`, `es:ESHttpPost`, …) on the domain resource its PATH names — the one
//!    decision [`crate::state::ingest::iam_decision`] owns. A request the cluster
//!    authenticated out of its internal user database speaks for an identity that exists
//!    only inside the cluster, so there is no AWS principal for this decision to be taken
//!    for and it is not taken.
//! 2. The CLUSTER: on a domain with fine-grained access control switched on, a principal
//!    that is neither the master user nor bound by a role mapping may do nothing, and one
//!    its mappings DO bind may do only what one of its roles permits — the action groups and
//!    index patterns that role's own document carries
//!    ([`crate::state::fgac_permissions`]). What a role narrows beside that is WHICH
//!    documents of a named index it may read and which fields of each (`dls`/`fls`): neither
//!    refuses a request, both narrow what it is answered with, so a document or a field a
//!    role hides is answered exactly as one the index does not hold.
//!
//! The REST surface served is the one this emulator can answer out of persisted state:
//! the cluster banner and health, the cluster's nodes and its own settings, the compact
//! listings of its indexes and nodes ([`cat`]), an index's create/read/delete, its own
//! settings read and updated ([`index_settings`]) and its mappings read and added to
//! ([`index_mapping`]), the aliases that stand for it ([`aliases`]), a document write, read,
//! partial update ([`document_update`]) and delete — each numbered with the revision a
//! conditional write is taken against — the
//! newline-delimited stream of them the bulk API takes ([`bulk`]), a count and a search over
//! a bounded query vocabulary ([`selector`]) whose `match` is evaluated over the terms the
//! field's own analysis chain produces, the buckets such a search groups the documents it
//! selected into ([`aggregations`]), the documents such a query selects removed
//! ([`delete_by_query`]), the tokens a text produces through that chain
//! ([`analyze`]), index templates and the component templates they are composed of
//! ([`component_templates`]), ingest pipelines (declared, simulated and applied to a write
//! that names one), the index-management plugin's policy API, the security plugin's
//! role and role-mapping API ([`security_records`]), its internal user database and account
//! API ([`internal_users`]), its AUDIT CONFIGURATION — which of the decisions this plane
//! takes about a request leave a record in the log group the domain publishes `AUDIT_LOGS`
//! to ([`security_audit`], [`state::audit`]) — the ALERTING plugin's monitors, the runs of
//! one and the alerts
//! they leave ([`alerting`], [`monitor_run`]), and the NOTIFICATIONS plugin's channel
//! configurations a monitor's action sends through ([`notifications`]), and the SNAPSHOT API:
//! the repositories a cluster registers, the snapshots it takes into the Amazon S3 bucket one
//! names, and the restore of one ([`snapshot`]).
//!
//! A path outside it is answered the way the engine answers a path no handler is registered
//! for, rather than with an invented body — including a RESERVED path ([`reserved`]), which
//! names an API of the engine and is never read as an index name.
//!
//! Each plugin of the Open Distro era is reachable under its legacy `_opendistro/<plugin>`
//! prefix as well as its current `_plugins/<plugin>` one
//! ([`current_plugin_prefix`]): the engine kept both, and both spellings reach one handler
//! here.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;
use serde_json::{json, Map, Value};
use smithy_server_runtime::{http, OperationContext};

use ::service_common::data_plane_addressing::{
    first_segment, host_only, EMULATOR_PATH_NAMESPACE,
};

use crate::state::{
    self, fgac, fgac_permissions, ingest, service_state,
    ComAmazonawsOpensearchAmazonopensearchserviceState as Store, Domain, IndexTemplate,
};

/// The segment that names THIS service's data plane behind `EMULATOR_PATH_NAMESPACE`: the
/// `arnNamespace`/`endpointPrefix` this service publishes.
const DATA_PLANE_SEGMENT: &str = state::ARN_NAMESPACE;

/// The ingest pipeline engine: what a pipeline's processors are and what they do to a
/// document, read once and used by both the write path and the simulate API.
mod pipeline;

/// The analyze API: the tokens a text produces through the chain a request resolves.
mod analyze;

/// The sections of a search body that decide HOW its hits are answered: the order they are
/// answered in, and the fragments each carries.
mod search;

/// The AGGREGATIONS a search asks over the documents its query selected.
mod aggregations;

/// The BULK API: the newline-delimited action/source stream a client loads documents with,
/// and the per-item answer each action earns.
mod bulk;

/// The PARTIAL-DOCUMENT UPDATE: the `doc` object a caller sends instead of the whole
/// document, merged onto the one the index holds — the rule the update API and a bulk
/// `update` action share.
mod document_update;

/// The ALIAS API: the alias actions a client points a virtual index name with.
mod aliases;

/// The index-settings API: the settings one index holds, and the update that changes the
/// dynamic ones.
mod index_settings;

/// The index-mapping API: the field mappings one index holds, and the update that adds to
/// them.
mod index_mapping;

/// The `_cat` APIs: the compact listings of a cluster's indexes and nodes.
mod cat;

/// The COMPONENT TEMPLATE API: the building blocks a composable index template is
/// composed of.
mod component_templates;

/// The ALERTING plugin's API: the monitors a client writes, the runs of one, and the alerts
/// its triggers left.
mod alerting;

/// The DELETE BY QUERY API: the documents a query selects, removed.
mod delete_by_query;

/// RUNNING one monitor: the queries its inputs ask, whether each trigger's condition holds,
/// and what each action of a firing trigger sends.
mod monitor_run;

/// The NOTIFICATIONS plugin's channel API: where an alerting action sends.
mod notifications;

/// The PATTERNS an index template applies to: the names a write may carry, and whether two
/// templates of the same priority can match one index name.
mod template_patterns;

/// The SNAPSHOT API: the repositories a cluster registers, the snapshots it takes into one,
/// and the restore of one — served against the Amazon S3 bucket the repository names.
mod snapshot;

/// The security plugin's INTERNAL USER DATABASE: the users a domain holds of its own, and
/// the account API that tells one who it is.
mod internal_users;

/// The security plugin's ROLE and ROLE-MAPPING records: what a role permits, and which
/// principals hold it.
mod security_records;
use security_records::{security_role, security_role_mapping};

/// The security plugin's AUDIT CONFIGURATION: which of the decisions the cluster takes
/// about a request leave a record in the log group the domain publishes `AUDIT_LOGS` to.
mod security_audit;

/// The query-string syntax a `query_string` clause and the URI form of a search both state
/// their clauses in.
mod query_string;

/// The most bytes one data-plane request body is read as JSON.
///
/// The emulator's OWN bound, like [`crate::state::INDEX_QUOTA`]: every byte lives in this
/// process's heap, so a body past it is refused rather than parsed. A document write is
/// bounded again, more tightly, by [`ingest::MAX_DOCUMENT_BYTES`].
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

/// How many index templates one domain may hold before a write is refused — the same kind
/// of bound [`crate::state::INDEX_QUOTA`] is, for the same reason.
pub const TEMPLATE_QUOTA: usize = 100;

/// How many roles, and how many role mappings, one domain's security configuration may
/// hold before a write is refused — the same kind of bound, for the same reason: a caller
/// looping with fresh names must not grow this process's state without end.
pub const SECURITY_RECORD_QUOTA: usize = 100;

/// The most hits one search answers with when the request names no `size`, which is the
/// engine's own default.
const DEFAULT_SEARCH_SIZE: usize = 10;

/// The most hits one search answers with however large a `size` it names — the engine's own
/// `index.max_result_window` default, which bounds `from + size`.
const MAX_RESULT_WINDOW: usize = 10_000;

/// How far a `bool` query may nest before the engine refuses it — its own
/// `indices.query.bool.max_nested_depth` default.
const MAX_BOOL_NESTING: usize = 20;

/// How many clauses of one query may ask an ANALYZED question — the engine's own
/// `indices.query.bool.max_clause_count` default, which bounds a query's BREADTH the way
/// [`MAX_BOOL_NESTING`] bounds its depth: each such clause has a chain resolved for it per
/// index the request names, so without it the work one request causes would be sized by the
/// body it sent.
const MAX_QUERY_CLAUSES: usize = 1_024;

/// How many clauses one query body may NAME at all, compositions included.
///
/// The emulator's OWN bound beside the engine's ceiling above, which counts only the clauses
/// a composition holds and not the compositions themselves: walking one `bool`'s own lists is
/// work every document the scan reaches pays for, so without this a body nesting empty
/// compositions would multiply the scan by a number the caller chose while counting no
/// clause at all. Left comfortably above the engine's ceiling, so a query that body for body
/// is within what the engine evaluates is never refused by this one.
const MAX_QUERY_CLAUSE_NODES: usize = 4_096;

/// How much text one search may analyze, over the whole scan: its bytes, plus
/// [`ANALYSIS_OVERHEAD`] for each analysis.
///
/// The emulator's OWN bound on the TIME one request can occupy, and on the PRODUCT the
/// per-factor caps leave open: a search analyzes each document's own text once per analyzed
/// clause, so its work is the documents it visits times the clauses it carries times the
/// text each holds at those fields — and it analyzes each CLAUSE's own text once per index
/// the request resolved, so a request naming every index multiplies that work too. Counting
/// the text rather than the analyses is what ties the budget to the work itself —
/// tokenizing, stemming and expanding all cost the length of what they run over — and the
/// per-analysis floor is what keeps a scan of very many tiny values bounded too. The budget
/// is charged AND checked at each analysis, query text and document text alike
/// ([`charged`]), so no analysis past it is performed, whatever one document's pass or one
/// index's resolution carries; the request that reached it is then refused rather than
/// reporting part of a scan as the whole. The data plane serves a request on the calling
/// task while it holds this service's store, so the span this bounds is one no other caller
/// can be made to wait past.
const MAX_ANALYZED_BYTES: usize = 8 * 1024 * 1024;

/// What one analysis costs against [`MAX_ANALYZED_BYTES`] beside the text's own length, so
/// an analysis of nothing still costs something.
const ANALYSIS_OVERHEAD: usize = 64;

/// How many distinct analysis chains one search may build for ONE index.
///
/// The emulator's own bound on what resolving a query's chains COSTS: a chain materialises
/// the whole dictionary its filters read (bounded per dictionary by
/// [`state::analysis::MAX_DICTIONARY_ENTRIES`]), so without it a query whose clauses named
/// a distinct analyzer each — as many as [`MAX_QUERY_CLAUSES`] admits — would choose how
/// many copies of a dictionary this process holds at once. With it the memory one request's
/// resolution costs is the product of two constants; past it the search is refused rather
/// than building another chain.
const MAX_CHAINS_PER_INDEX: usize = 32;

/// The engine's error type for a query past that ceiling.
const TOO_MANY_CLAUSES: &str = "too_many_clauses";

/// How many terms ONE `terms` or `ids` clause may name — the engine's own
/// `index.max_terms_count` default.
///
/// Every term of such a clause is compared against every document the scan reaches, so this
/// is what keeps one clause's own breadth from sizing the work; past it the query is
/// refused, as the engine refuses it.
const MAX_TERMS_COUNT: usize = 65_536;

/// The most shards and replica copies one index's settings may ask for, as the engine's
/// own `index.number_of_shards` limit (`cluster.max_shards_per_node` times a node) bounds
/// them. A settings value past this is clamped for the health arithmetic rather than
/// multiplied out.
const MAX_SHARDS_PER_INDEX: i64 = 1_024;

/// The engine's error type for a request whose body is not the JSON the handler expects.
const PARSE_EXCEPTION: &str = "parse_exception";

/// The engine's error type for a query clause the request names and the engine does not
/// register.
const PARSING_EXCEPTION: &str = "parsing_exception";

/// The engine's error type for a create of an index that already exists.
const RESOURCE_ALREADY_EXISTS_EXCEPTION: &str = "resource_already_exists_exception";

/// The engine's error type for a name no index may carry.
const INVALID_INDEX_NAME_EXCEPTION: &str = "invalid_index_name_exception";

/// The engine's error type for a write that may not replace a document the index already
/// holds.
const VERSION_CONFLICT_ENGINE_EXCEPTION: &str = "version_conflict_engine_exception";

/// The engine's error type for an UPDATE of a document the index does not hold.
const DOCUMENT_MISSING_EXCEPTION: &str = "document_missing_exception";

/// The most documents ONE simulate request runs a pipeline over.
///
/// The emulator's OWN bound on the work one request does: every document is run through every
/// processor the pipeline declares ([`pipeline::MAX_PROCESSORS`]), so the product of the two
/// is what this caps.
const MAX_SIMULATED_DOCUMENTS: usize = 1_000;

/// The second segment of a `_nodes` path that names an API OF ITS OWN rather than a node:
/// the node statistics, the hot threads, the feature usage and the secure-settings reload.
///
/// This emulator serves none of them, so such a request is answered the way the engine
/// answers a path it has no handler for — instead of being read as a filter naming a node
/// nothing is called, which would answer an empty node set for a request that asked for
/// something else entirely.
const NODES_SUB_APIS: [&str; 4] = ["hot_threads", "reload_secure_settings", "stats", "usage"];

/// The most comma-separated index patterns one request may name.
///
/// The emulator's OWN bound on how much work one request's index resolution does, beside
/// the de-duplication below: the resolved set is bounded by the indexes a domain holds, so
/// this bounds the resolution itself.
const MAX_NAMED_INDEX_PATTERNS: usize = 100;

/// The most documents one count or search VISITS, which is the whole store at most once:
/// every index a domain may hold ([`state::INDEX_QUOTA`]) times the documents one index may
/// hold ([`ingest::INDEX_DOCUMENT_QUOTA`]). The de-duplicated index set cannot exceed it,
/// so this is a proof rather than a cut-off, and a request that would is refused rather
/// than scanning further.
const MAX_DOCUMENTS_VISITED: usize = state::INDEX_QUOTA * ingest::INDEX_DOCUMENT_QUOTA;

/// One hit a search answers with: the index the document came from, its id and its source.
type Hit = (String, String, Value);

/// How many documents a request SELECTED, the page of hits it asked for, and what the
/// aggregations it named observed over every one of them.
type Selected = (usize, Vec<Selection>, aggregations::Aggregations);

/// What the query asks of each field, as ONE index's chains analyze it: the field, and every
/// term a clause on it asks about — which is what a fragment is wrapped around.
type Wanted = BTreeMap<String, BTreeSet<String>>;

/// One document on the page a search answers with: the hit itself, the values the request's
/// sort keys read off it, and the fragments its highlight produced.
#[derive(Debug)]
struct Selection {
    hit: Hit,
    sort: Vec<Value>,
    highlight: Option<Value>,
}

/// The separator a cross-cluster index pattern names its remote cluster with:
/// `<cluster>:<index>`.
const REMOTE_CLUSTER_SEPARATOR: char = ':';

/// What the remote clusters of a cross-cluster search answered: the documents each
/// `<cluster>:<index>` pattern of ONE request resolved to, and the aliases it named that
/// this domain has no connection to search through.
///
/// A cross-cluster search runs against ANOTHER domain's indexes, which this plane reaches
/// through the store rather than through the one domain it serves — so the remote half is
/// resolved out of the store BEFORE that domain is borrowed, and carried in. It is empty
/// for every request that names no remote cluster, which is the whole surface apart from a
/// search or a count.
#[derive(Debug, Default)]
struct CrossCluster {
    /// The hits each pattern resolved to, keyed by the pattern AS WRITTEN. Each hit names
    /// its index the way a cross-cluster hit does — `<cluster>:<index>` — so a client can
    /// tell which cluster answered.
    resolved: BTreeMap<String, Vec<Hit>>,
    /// The cluster aliases the request named that resolve to no connection this domain can
    /// search through: none at all, or one that is not yet (or no longer) `ACTIVE`.
    unknown: BTreeSet<String>,
}

/// What a request ADDRESSES, as the wire carries it: the host it was sent to — absent on a
/// request that names neither a `Host` header nor an absolute URI — and its path.
///
/// Built from BOTH shapes the router hands this module, the buffered parts the claim reads
/// and the runtime-API request the serving seam reads, so one rule resolves both and the
/// claim cannot resolve a different domain than the answer does.
#[derive(Debug)]
struct Addressed<'a> {
    host: Option<&'a str>,
    path: &'a str,
}

impl<'a> Addressed<'a> {
    /// The addressing of the buffered request these parts came from.
    ///
    /// The host is the one reading of which host a request addressed
    /// (`service_common::data_plane_addressing::addressed_authority`): the authority of an
    /// absolute-form request target when the request carries one, and the `Host` header
    /// otherwise — so a request another service RELAYS here resolves to the domain the relay
    /// addressed rather than to the host the relay's own client named.
    fn of_parts(parts: &'a http::request::Parts) -> Self {
        Self {
            host: ::service_common::data_plane_addressing::addressed_authority_of(
                parts.uri.authority().map(http::uri::Authority::as_str),
                parts
                    .headers
                    .get("host")
                    .and_then(|host| host.to_str().ok()),
            ),
            path: parts.uri.path(),
        }
    }

    /// The addressing of a runtime-API request, whose URI arrives as a string and is parsed
    /// by the `http` crate rather than split by hand.
    fn of_request(req: &'a RtRequest, uri: &'a http::Uri) -> Self {
        Self {
            host: ::service_common::data_plane_addressing::addressed_authority(
                req.uri(),
                req.headers().get("host"),
            ),
            path: uri.path(),
        }
    }
}

/// What a request resolves to: the domain it addresses, and the path WITHIN that domain's
/// own endpoint — which is what the REST surface is read against.
#[derive(Debug)]
struct Resolved {
    domain_name: String,
    request_path: String,
}

/// The request URI of a runtime-API request, parsed by the `http` crate. A URI this parse
/// rejects is one the server would not have accepted, and it resolves to `http::Uri`'s own
/// default (`/`), which names no domain and claims nothing.
fn parsed_uri(uri: &str) -> http::Uri {
    uri.parse::<http::Uri>().unwrap_or_default()
}

/// Whether this data plane serves the request these `parts` came from — read by
/// `Service::can_handle`, so a request addressed to a domain this service holds reaches
/// this service instead of the front door's `UnknownService`.
pub fn claims_parts<Eng: Send + Sync + 'static>(
    engine: &Eng,
    parts: &http::request::Parts,
) -> bool {
    resolve(engine, &Addressed::of_parts(parts)).is_some()
}

/// [`claims_parts`] for the runtime-API request the serving seams are handed, resolved by
/// the same rule.
pub fn claims_request<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> bool {
    let uri = parsed_uri(req.uri());
    resolve(engine, &Addressed::of_request(req, &uri)).is_some()
}

/// Whether `req` presents a SigV4 credential, which is what decides whether the router
/// verifies a signature before this plane serves it.
pub fn carries_sigv4_credential(req: &RtRequest) -> bool {
    presents_sigv4(req.headers().get("authorization"), req.uri())
}

/// Whether a request presenting `authorization` and addressing `uri` carries a SigV4
/// credential: the `Authorization` header's own scheme, or a presigned URL's signature.
fn presents_sigv4(authorization: Option<&str>, uri: &str) -> bool {
    authorization.is_some_and(|value| {
        value
            .get(..SIGV4_SCHEME.len())
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case(SIGV4_SCHEME))
    }) || uri
        .split(['?', '&'])
        .any(|parameter| parameter.starts_with("X-Amz-Signature="))
}

/// The scheme an AWS API call's credential is presented under.
const SIGV4_SCHEME: &str = "AWS4-HMAC-SHA256";

/// The scheme HTTP Basic credentials are presented under (RFC 7617 §2).
const BASIC_SCHEME: &str = "Basic ";

/// The most bytes of a presented Basic credential this plane decodes. The emulator's OWN
/// bound, like [`MAX_REQUEST_BODY_BYTES`]: a user name and a password are each bounded by
/// the model (`MasterUserName` 1..64, `MasterUserPassword` 8..128), so a header past this
/// names no credential of any domain and is not decoded at all.
const MAX_CREDENTIAL_BYTES: usize = 1024;

/// The user name and password `authorization` presents as HTTP Basic credentials — `Basic`
/// followed by the base64 of `user-id ":" password` (RFC 7617 §2) — or `None` when it
/// presents none.
///
/// A header this reading cannot take apart presents no credential, which is the same
/// position as carrying none: what the cluster then requires of the request is the
/// authentication decision's ([`fgac::authentication`]), not this reading's.
fn presented_credential(authorization: Option<&str>) -> Option<(String, String)> {
    let header = authorization?;
    if header.len() > MAX_CREDENTIAL_BYTES {
        return None;
    }
    let scheme = header.get(..BASIC_SCHEME.len())?;
    if !scheme.eq_ignore_ascii_case(BASIC_SCHEME) {
        return None;
    }
    let encoded = header.get(BASIC_SCHEME.len()..)?.trim();
    let decoded = ::aws_smithy_types::base64::decode(encoded).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, password) = text.split_once(':')?;
    (!user.is_empty()).then(|| (user.to_string(), password.to_string()))
}

/// The realm the security plugin names in the challenge it answers an unauthenticated
/// request with.
const SECURITY_REALM: &str = "OpenSearch Security";

/// What the security plugin answers a request it authenticated nobody for: `401`, and the
/// one word it says about it.
///
/// The body names no cluster, no domain and no user: nothing about the cluster has been
/// authorized to be told yet, so the only thing the answer states is that the request was
/// not authenticated. `challenged` is a request that presented no credential at all, which
/// is the one the plugin tells HOW to present one — the `WWW-Authenticate` challenge; a
/// credential that was presented and refused is told nothing further.
fn unauthorized(challenged: bool) -> RtResponse {
    let mut response = RtResponse::new(
        StatusCode::try_from(401).unwrap_or_else(|_| fallback_status()),
        SdkBody::from(UNAUTHORIZED),
    );
    let mut header = |name: &str, value: String| {
        if let Err(refused) = response.headers_mut().try_insert(name.to_string(), value) {
            ::tracing::warn!(
                error = %refused,
                "the data plane's authentication header cannot be sent on the wire and is \
                 left off"
            );
        }
    };
    header("content-type", "text/plain; charset=UTF-8".to_string());
    if challenged {
        header(
            "www-authenticate",
            format!("Basic realm=\"{SECURITY_REALM}\""),
        );
    }
    response
}

/// The body that answer carries.
const UNAUTHORIZED: &str = "Unauthorized";

/// Which domain `addressed` addresses, and the path within its endpoint, or `None` when
/// this data plane serves the request not at all.
fn resolve<Eng: Send + Sync + 'static>(engine: &Eng, addressed: &Addressed<'_>) -> Option<Resolved> {
    let state = service_state(engine)?;
    let store = state.lock();

    // PREFIXED PATH-STYLE, the one addressing that is explicit about naming this plane,
    // and so the one read whatever the request's host is.
    if let Some((named, path)) = prefixed_addressing(addressed.path) {
        let domain = addressed_domain(&store, named)?;
        return Some(Resolved {
            domain_name: domain,
            request_path: path.to_string(),
        });
    }
    // A path under the reserved namespace that did NOT name this plane names another one,
    // which serves its own requests.
    if first_segment(addressed.path).is_some_and(|(first, _)| first == EMULATOR_PATH_NAMESPACE) {
        return None;
    }
    // A path this model's own routing table serves is a CONTROL-plane request, claimed by
    // the service's modelled path space rather than by this plane, whatever host it
    // reached.
    if first_segment(addressed.path)
        .is_some_and(|(first, _)| crate::ROUTE_PATH_SEGMENTS.contains(&first))
    {
        return None;
    }
    // HOST-STYLE: the host IS a host some domain of this store publishes.
    //
    // The ADDRESS a domain's own listener answers at is matched first, and as a whole
    // `host:port`: the port is what tells one domain's listener from another's, and from this
    // emulator's own endpoint. It is matched here as well as served on its own socket so an
    // in-process client handed that address — a running function, another service's handler —
    // reaches the domain through the front door too, without opening a connection.
    let addressed_authority = addressed.host.map(str::to_ascii_lowercase);
    if let Some(authority) = &addressed_authority {
        if let Some(domain) = store
            .domains
            .values()
            .find(|domain| publishes_authority(domain, authority))
        {
            return Some(Resolved {
                domain_name: domain.domain_name.clone(),
                request_path: addressed.path.to_string(),
            });
        }
    }
    let host = host_only(addressed.host?);
    let domain = store
        .domains
        .values()
        .find(|domain| publishes_host(domain, host))?;
    Some(Resolved {
        domain_name: domain.domain_name.clone(),
        request_path: addressed.path.to_string(),
    })
}

/// Whether `domain` publishes its data plane AT the whole authority `authority` — the
/// `host:port` of the listener this emulator opened for it
/// ([`crate::state::domain_plane`]), which is the address `DomainStatus$Endpoint` carries.
///
/// Matched as a whole and case-insensitively, because the port is part of what identifies the
/// listener: another port of the same host is another domain's plane, or this emulator's own
/// endpoint.
fn publishes_authority(domain: &Domain, authority: &str) -> bool {
    domain
        .plane_authority
        .as_ref()
        .is_some_and(|published| published.eq_ignore_ascii_case(authority))
}

/// Whether `domain` publishes `host`: its public endpoint, the dual-stack public host
/// beside it (`DomainStatus$EndpointV2`), or one of the VPC-access hosts
/// `DomainStatus$Endpoints` reports for a domain provisioned inside a VPC.
///
/// Every host the control plane publishes for the domain's data plane is here, because
/// each of them is an address a client was told to send index and search requests to:
/// a published address this plane did not resolve would be one the service announced and
/// then did not serve.
fn publishes_host(domain: &Domain, host: &str) -> bool {
    if domain.endpoint == host {
        return true;
    }
    if domain
        .public_endpoint_v2()
        .is_some_and(|published| published == host)
    {
        return true;
    }
    domain
        .vpc_access_endpoints()
        .is_some_and(|endpoints| endpoints.values().any(|published| published == host))
}

/// The domain a prefixed addressing NAMES: its own `DomainName`, or a host it publishes.
fn addressed_domain(store: &Store, named: &str) -> Option<String> {
    if store.domains.contains_key(named) {
        return Some(named.to_string());
    }
    store
        .domains
        .values()
        .find(|domain| publishes_host(domain, named))
        .map(|domain| domain.domain_name.clone())
}

/// The domain a prefixed path names and the request path that follows it, or `None` when
/// the path is not of that shape.
fn prefixed_addressing(path: &str) -> Option<(&str, &str)> {
    let (namespace, rest) = first_segment(path)?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = first_segment(rest)?;
    if plane != DATA_PLANE_SEGMENT {
        return None;
    }
    let (named, request_path) = first_segment(rest)?;
    (!named.is_empty()).then_some((named, request_path))
}

/// Answer a request this plane claimed, out of the records the control plane holds — and
/// trace any notification the work it did owed a channel and could not be handed one.
///
/// This is the entry for a request carrying NO AWS credential, which is answered before the
/// router authenticates anything and so has no operation context to reach another service
/// through. [`served`] is the entry for every other request, whose notifications ARE handed
/// over; what the two ANSWER is the one thing [`serve_resolved`] decides, so the two paths
/// cannot serve different surfaces.
pub fn serve<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ctx: Option<&OperationContext<'_>>,
) -> RtResponse {
    let answered = served(engine, req, ctx);
    state::notification_publication::trace_undelivered(&answered.notifications);
    answered.response
}

/// One whole data-plane answer: the response, and the notifications the work it did owes a
/// channel.
///
/// The notifications are handed over by the caller, once this service's store is released and
/// before the request is answered ([`state::notification_publication::publish`]), so a client
/// that reads a subscribed queue after its request answered finds them there.
pub struct Served {
    /// What the request is answered with.
    pub response: RtResponse,
    /// The notifications a monitor run this request drove owes its channels.
    pub notifications: Vec<state::notification_publication::Publication>,
}

impl Served {
    /// An answer that owes no notification — every answer but a firing monitor run's.
    fn of(response: RtResponse) -> Self {
        Self {
            response,
            notifications: Vec::new(),
        }
    }
}

/// Answer a request this plane claimed, out of the records the control plane holds.
///
/// `ctx` is present for a request the router named an operation for, which is every request
/// carrying a credential; it is what the calling principal is resolved from.
pub fn served<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ctx: Option<&OperationContext<'_>>,
) -> Served {
    let uri = parsed_uri(req.uri());
    let Some(resolved) = resolve(engine, &Addressed::of_request(req, &uri)) else {
        // The domain was released by another request between the claim and here, so the
        // host it published answers nothing.
        return Served::of(unavailable());
    };
    // The principal the two authorization decisions are taken for. An empty ARN is a
    // request this emulator can resolve no identity for — an engine carrying no STS or IAM
    // store, or an expired session — which no role mapping binds, so fine-grained access
    // control refuses it exactly as it refuses an unmapped one.
    //
    // The identity policies live in ANOTHER service's store, so both are read before this
    // one is locked: no call holds two service stores at once. `None` is a caller this
    // emulator holds no policy evidence for — the account's own credential — which is the
    // posture `crate::state::authorization` documents; a caller carrying a grant that cannot
    // be READ is reported with that grant marked, which is a different thing.
    let caller = Caller {
        principal: ctx
            .and_then(|ctx| state::authorization::request_principal_arn(engine, ctx))
            .unwrap_or_default(),
        identity: ctx.and_then(|ctx| state::authorization::request_identity(engine, ctx)),
    };
    let authorization = req.headers().get("authorization");
    answered(
        engine,
        &Incoming {
            resolved,
            method: req.method().to_ascii_uppercase(),
            query: uri.query().unwrap_or_default(),
            body: req.body().bytes().unwrap_or_default(),
            presented: presented_credential(authorization),
            sigv4: presents_sigv4(authorization, req.uri()),
            caller,
            can_notify: ctx.is_some(),
        },
    )
}

/// Answer a request that arrived on the LISTENER one domain is published on
/// ([`crate::state::domain_plane`]), where the ADDRESS names the domain and the whole path is
/// the request path.
///
/// The same function serves it as serves the front door's own addressings ([`answered`]), so
/// the two addressings of one domain cannot diverge in what they answer. What differs is where
/// each reads the request from: the router hands the front door a runtime-API request and an
/// operation context, and the core hands this one a plain HTTP request and no context at all —
/// so the calling principal is resolved from the credential the request PRESENTS
/// ([`state::authorization::key_principal`]), against the same identity records the front
/// door's resolution reads.
pub fn serve_published<Eng: Send + Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    req: &::http::Request<Vec<u8>>,
) -> ::http::Response<Vec<u8>> {
    let authorization = req
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok());
    // As at the front door: resolved BEFORE this service's store is locked, and from the
    // credential the request presents, read by the emulator's OWN reading of a SigV4
    // `Authorization` header — the one `state::authorization::request_principal_arn` reads at
    // the front door, so the two paths resolve one credential to one principal.
    let resolved =
        state::authorization::key_principal(engine, &::engine::extract_akid(req.headers()));
    let answered = answered(
        engine,
        &Incoming {
            resolved: Resolved {
                domain_name: domain_name.to_string(),
                request_path: req.uri().path().to_string(),
            },
            method: req.method().as_str().to_ascii_uppercase(),
            query: req.uri().query().unwrap_or_default(),
            body: req.body(),
            presented: presented_credential(authorization),
            sigv4: presents_sigv4(authorization, &req.uri().to_string()),
            caller: Caller {
                principal: resolved.arn,
                identity: resolved.identity,
            },
            can_notify: false,
        },
    );
    // Nothing on this plane can hand a notification over — reaching another service needs the
    // operation context the router builds — so a monitor run driven here traces what it owed,
    // exactly as the front door's anonymous path does ([`serve`]).
    state::notification_publication::trace_undelivered(&answered.notifications);
    as_http(answered.response)
}

/// One answer of this surface as a plain HTTP response, which is what a core HTTP listener
/// writes — the same status, headers and bytes the front door sends.
///
/// Every header the answer carries is copied, not a named few: what this plane answers is one
/// function's ([`answered`]), so a header it comes to set must reach a client at either address
/// or the two addressings of one domain diverge again.
fn as_http(response: RtResponse) -> ::http::Response<Vec<u8>> {
    let status = response.status().as_u16();
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let body = response.body().bytes().unwrap_or_default().to_vec();
    let mut builder = ::http::Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder.body(body).unwrap_or_else(|_| {
        // Every part came off a response this module built, so the builder cannot fail;
        // answer a bare 500 rather than panicking if it ever does.
        let mut fallback = ::http::Response::new(Vec::new());
        *fallback.status_mut() = ::http::StatusCode::INTERNAL_SERVER_ERROR;
        fallback
    })
}

/// Who a data-plane request speaks for, as both entries resolve it.
#[derive(Debug, Default)]
struct Caller {
    /// The ARN of the principal the request speaks for, empty when none was resolved.
    principal: String,
    /// What this emulator can show about the identity policies in force on it, and `None` for
    /// a caller it holds no evidence about.
    identity: Option<state::authorization::HeldIdentity>,
}

/// One data-plane request as it arrives, whichever address it arrived at: what it addresses,
/// what it asks, what it presents and who it speaks for.
///
/// Both entries build one of these — [`served`] from the runtime-API request the router hands
/// the front door, [`serve_published`] from the plain HTTP request the core hands a domain's
/// own listener — so [`answered`] below is the ONE reading of what a domain answers.
struct Incoming<'r> {
    resolved: Resolved,
    method: String,
    query: &'r str,
    body: &'r [u8],
    /// The user name and password an HTTP Basic credential presented, if any.
    presented: Option<(String, String)>,
    /// Whether the request presents an AWS credential.
    sigv4: bool,
    caller: Caller,
    /// Whether a notification the work this request does owes a channel can be handed over at
    /// all, which needs the operation context only the router builds.
    can_notify: bool,
}

/// The answer this plane gives one request, out of the records the control plane holds.
fn answered<Eng: Send + Sync + 'static>(engine: &Eng, incoming: &Incoming<'_>) -> Served {
    let resolved = &incoming.resolved;
    if incoming.body.len() > MAX_REQUEST_BODY_BYTES {
        return Served::of(error(
            413,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "request body is larger than the maximum of {MAX_REQUEST_BODY_BYTES} bytes \
                 this cluster takes"
            ),
        ));
    }
    let now = SystemTime::now();
    let Some(state) = service_state(engine) else {
        return Served::of(unavailable());
    };
    // AUTHENTICATION, before anything of the cluster is read: on a domain with
    // fine-grained access control switched on the cluster admits nobody anonymously, and a
    // request presenting HTTP Basic credentials is admitted only as a user its own internal
    // database holds ([`fgac::authentication`]). The decision is taken under a lock of its
    // own and the store released again, the way the claim above resolves the domain, so the
    // serving below still reads the domain exactly once.
    let (authenticated, refused_login) = {
        let store = state.lock();
        let Some(domain) = store.domains.get(&resolved.domain_name) else {
            return Served::of(unavailable());
        };
        let authenticated = fgac::authentication(
            domain,
            incoming
                .presented
                .as_ref()
                .map(|(user, password)| (user.as_str(), password.as_str())),
            incoming.sigv4,
        );
        // A credential the cluster REFUSES is a decision of its REST layer, and the
        // security plugin's audit log has a record of it where the domain's audit
        // configuration says so — stated for the user the credential claimed to be, which
        // is all that is known about a caller the cluster did not admit. Built here, where
        // the domain is already read, and delivered below once this store is released.
        let refused_login = match authenticated {
            fgac::Authentication::Refused => state::log_publishing::records(
                domain,
                &[state::log_publishing::Served::Audited(state::audit::Event {
                    category: state::audit::CATEGORY_FAILED_LOGIN,
                    layer: state::audit::Layer::Rest,
                    effective_user: incoming
                        .presented
                        .as_ref()
                        .map_or("", |(user, _)| user.as_str()),
                    rest_path: &resolved.request_path,
                    rest_method: &incoming.method,
                    privilege: None,
                    index: None,
                })],
                now,
            ),
            _ => Vec::new(),
        };
        (authenticated, refused_login)
    };
    // Who the request speaks for: the user the cluster authenticated it as, where it
    // authenticated one, and otherwise the AWS principal that was resolved for it.
    let internal_user = matches!(authenticated, fgac::Authentication::User(_));
    let principal = match authenticated {
        fgac::Authentication::Refused => {
            state::log_publishing::deliver(engine, &refused_login);
            return Served::of(unauthorized(incoming.presented.is_none()));
        }
        fgac::Authentication::User(name) => name,
        fgac::Authentication::Unauthenticated => incoming.caller.principal.clone(),
    };
    let request = Request {
        method: &incoming.method,
        path: &resolved.request_path,
        segments: path_segments(&resolved.request_path),
        query: incoming.query,
        body: incoming.body,
        principal: &principal,
        identity: incoming
            .caller
            .identity
            .as_ref()
            .map(state::authorization::HeldIdentity::policies)
            .unwrap_or_default(),
        started: ::std::time::Instant::now(),
        at: now,
        published: ::std::cell::RefCell::new(Vec::new()),
        notifications: ::std::cell::RefCell::new(Vec::new()),
        pending_run: ::std::cell::RefCell::new(None),
        pending_snapshot: ::std::cell::RefCell::new(None),
        can_notify: incoming.can_notify,
        internal_user,
    };
    let mut store = state.lock();
    // The dictionary files this domain's packages mount, resolved out of the store before
    // the domain this request addresses is borrowed: the content belongs to the PACKAGES
    // the account holds, which sit beside the domain.
    let files = analysis_files(&mut store, &resolved.domain_name, &request, now);
    // The remote half of a cross-cluster search: resolved out of the store before the
    // domain this request addresses is borrowed, since the indexes it reads belong to
    // ANOTHER domain of the same store.
    let across = cross_cluster(&mut store, &resolved.domain_name, &request, now);
    // The read materialises any configuration change the clock already owes this domain,
    // which this request neither starts nor changes.
    let Some(domain) = store.settled_mut(&resolved.domain_name, now) else {
        return Served::of(unavailable());
    };
    if !domain.created {
        return Served::of(error(
            503,
            "cluster_block_exception",
            "blocked by: [SERVICE_UNAVAILABLE/1/state not recovered / initialized]",
        ));
    }
    // The cluster ADMITTED this request, which is a decision of its REST layer: the
    // security plugin's audit log has a record of it where the domain's audit configuration
    // says so. Noted before the request is served, as the plugin states it, and so before
    // the transport-layer decision the route it reaches takes ([`guarded`]).
    request.record_audited(
        domain,
        state::audit::CATEGORY_AUTHENTICATED,
        state::audit::Layer::Rest,
        None,
    );
    let answer = serve_resolved(domain, &request, now, &across, &files);
    // The records the work this request did produced go to the log groups the domain
    // publishes them to — after THIS service's store is released, so the two stores are
    // never held at once (`crate::state::log_publishing`'s lock order).
    drop(store);
    // A MONITOR RUN is planned under that guard and finished here: the step that performs the
    // caller's own source — the trigger conditions and the message templates — runs with no
    // store held and off the async worker, and the report it answers REPLACES the placeholder
    // the planning route returned ([`monitor_run`]). Every other request has planned none.
    let answer = match request.pending_run.borrow_mut().take() {
        Some(pending) => {
            let evaluated = monitor_run::off_the_async_worker(|| pending.evaluate());
            finished_run(engine, &resolved.domain_name, &request, &evaluated, now)
        }
        None => answer,
    };
    // REPOSITORY work is planned under that guard and performed here for the same reason: the
    // blobs a snapshot writes, reads or removes live in the object store of ANOTHER service,
    // and no call holds two service stores at once. The answer it comes to REPLACES the
    // placeholder the planning route returned ([`snapshot`]); every other request has planned
    // none.
    let answer = match request.pending_snapshot.borrow_mut().take() {
        Some(planned) => snapshot::performed(engine, &resolved.domain_name, planned, now),
        None => answer,
    };
    state::log_publishing::deliver(engine, &request.published.into_inner());
    Served {
        response: answer,
        notifications: request.notifications.into_inner(),
    }
}

/// What a domain whose record this plane cannot read answers: the cluster behind the address
/// is not there — a domain released between the claim and the serving, or an engine that vends
/// no store of this service at all.
fn unavailable() -> RtResponse {
    error(
        503,
        "cluster_block_exception",
        "blocked by: [SERVICE_UNAVAILABLE]",
    )
}

/// Record what an evaluated monitor run came to and answer the report it reports.
///
/// The store is taken again for this: the alerts a firing leaves and the channel each firing
/// action sends through are the domain's own records. A domain released while the run was
/// being evaluated answers nothing, the way it does for any other request.
fn finished_run<Eng: Send + Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    request: &Request<'_>,
    evaluated: &monitor_run::EvaluatedRun,
    now: SystemTime,
) -> RtResponse {
    let unavailable = || {
        error(
            503,
            "cluster_block_exception",
            "blocked by: [SERVICE_UNAVAILABLE]",
        )
    };
    let Some(state) = service_state(engine) else {
        return unavailable();
    };
    let mut store = state.lock();
    let Some(domain) = store.settled_mut(domain_name, now) else {
        return unavailable();
    };
    let report = monitor_run::finish(domain, request, evaluated);
    ok(&report)
}

/// The dictionary files the packages IN PLACE on the domain mount, for a request whose
/// answer the analysis chain decides; no file at all for every other request, which reads
/// none.
///
/// Which requests those are is the ANSWER's own business: the analyze API reports what a
/// chain produces, and a search or a count evaluates a `match` over the terms a field's
/// chain produces. Everything else is answered without opening a dictionary, so nothing is
/// read for it.
///
/// The clock's owed moves on the packages and on this domain's associations are
/// materialised first, exactly as a read of those records would have — so a package whose
/// copy has landed, and an association whose move has, are in place here.
fn analysis_files(
    store: &mut Store,
    domain_name: &str,
    request: &Request<'_>,
    now: SystemTime,
) -> state::analysis::PackageFiles {
    if !reads_analysis(request) {
        return state::analysis::PackageFiles::default();
    }
    store.settle_packages(now);
    if let Some(domain) = store.settled_mut(domain_name, now) {
        domain.settle_package_associations(now);
    }
    match store.domains.get(domain_name) {
        Some(domain) => state::analysis::package_files(&store.packages, domain),
        None => state::analysis::PackageFiles::default(),
    }
}

/// Whether what `request` is answered with depends on an index's analysis chain.
fn reads_analysis(request: &Request<'_>) -> bool {
    matches!(
        request.segments().as_slice(),
        ["_analyze"]
            | [_, "_analyze"]
            // A search or a count reads the chains of every index it is about, whether the
            // path names them or addresses the whole cluster.
            | ["_search" | "_count"]
            | [_, "_search" | "_count"]
            // The documents a delete-by-query removes are the ones the same query SELECTS,
            // so it reads the same chains a search does.
            | [_, "_delete_by_query"]
            // A monitor RUN searches the monitor's own indexes through the same search
            // ([`monitor_run`]), so its answer depends on the same chains a client's search
            // does and the same files have to be in hand.
            | ["_plugins", "_alerting", "monitors", "_execute"]
            | ["_plugins", "_alerting", "monitors", _, "_execute"]
    )
}

/// The documents the remote clusters of a cross-cluster search hold for the patterns
/// `request` names, or an empty set for a request that names no remote cluster.
///
/// A cross-cluster query names its cluster in the index pattern itself
/// (`<cluster>:<index>`), where `<cluster>` is the `ConnectionAlias` the source domain's
/// own outbound connection carries. Only a connection that is `ACTIVE` can be searched
/// through — the status move the clock owes a connection is materialised first, so a
/// connection that became active while nobody was looking is searchable — and only the
/// domain that connection names is read. Every other alias is reported back as a cluster
/// this domain cannot search.
///
/// Reading the remote domain materialises what the clock already owed IT, exactly as a
/// read of that domain would have; nothing else is written.
fn cross_cluster(
    store: &mut Store,
    local_domain: &str,
    request: &Request<'_>,
    now: SystemTime,
) -> CrossCluster {
    let mut across = CrossCluster::default();
    let patterns: Vec<&str> = remote_patterns(request);
    if patterns.is_empty() {
        return across;
    }
    store.settle_connections(now);
    // ONE budget over the whole request rather than one per pattern, so what a request
    // materialises from the remote clusters is bounded by the same count a local search
    // visits however many patterns it names.
    let mut budget = MAX_DOCUMENTS_VISITED;
    for pattern in patterns {
        let Some((alias, named)) = pattern.split_once(REMOTE_CLUSTER_SEPARATOR) else {
            continue;
        };
        let remote_domain = store.connections.values().find_map(|connection| {
            (connection.connection_alias == alias
                && connection.local_domain.domain_name == local_domain
                && connection.status == state::CONNECTION_STATUS_ACTIVE)
                .then(|| connection.remote_domain.domain_name.clone())
        });
        let Some(remote_domain) = remote_domain else {
            across.unknown.insert(alias.to_string());
            continue;
        };
        let Some(remote) = store.settled_mut(&remote_domain, now) else {
            across.unknown.insert(alias.to_string());
            continue;
        };
        let mut hits = Vec::new();
        let mut names = false;
        for index in remote
            .indexes
            .values()
            .filter(|index| matched_index(named, &index.index_name))
        {
            names = true;
            for document in index.documents.values() {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                hits.push((
                    format!("{alias}{REMOTE_CLUSTER_SEPARATOR}{}", index.index_name),
                    document.id.clone(),
                    document.source.clone(),
                ));
            }
        }
        // An index the remote cluster does not hold resolves to no entry at all, so the
        // search refuses the pattern the same way it refuses a local index that is absent.
        if names {
            across.resolved.insert(pattern.to_string(), hits);
        }
    }
    across
}

/// Whether `pattern` — one index name, or one wildcard pattern, or `_all` — names `index`.
/// The same reading the local resolution applies, so a remote pattern and a local one
/// select by the same rule.
fn matched_index(pattern: &str, index: &str) -> bool {
    if pattern == "_all" || pattern.contains('*') || pattern.contains('?') {
        return ::service_common::iam_authorization::wildcard_match(
            if pattern == "_all" { "*" } else { pattern },
            index,
            true,
        );
    }
    pattern == index
}

/// The `<cluster>:<index>` patterns `request` names, which only a search or a count can
/// carry: those are the two routes whose first segment is an index pattern AND that read
/// documents, so nothing else resolves a remote cluster.
fn remote_patterns<'r>(request: &'r Request<'r>) -> Vec<&'r str> {
    let segments = request.segments();
    let [patterns, "_search" | "_count"] = segments.as_slice() else {
        return Vec::new();
    };
    patterns
        .split(',')
        .filter(|pattern| pattern.contains(REMOTE_CLUSTER_SEPARATOR))
        .take(MAX_NAMED_INDEX_PATTERNS)
        .collect()
}

/// The comma-separated index patterns `named` carries, refused past
/// [`MAX_NAMED_INDEX_PATTERNS`] so one request's index resolution cannot be sized by the
/// path it sent. An empty element names nothing and is dropped.
fn named_patterns(named: &str) -> Result<Vec<&str>, Box<RtResponse>> {
    let patterns: Vec<&str> = named.split(',').filter(|name| !name.is_empty()).collect();
    if patterns.len() > MAX_NAMED_INDEX_PATTERNS {
        return Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "a request may name at most {MAX_NAMED_INDEX_PATTERNS} indexes, and this one \
                 names {}",
                patterns.len()
            ),
        )));
    }
    Ok(patterns)
}

/// The indexes of `domain` ONE pattern names: every index a wildcard pattern or `_all`
/// matches, every index an ALIAS of that name points at, or the one index a plain name
/// carries — and the engine's own not-found for a name no index and no alias of the domain
/// carries.
///
/// The ONE reading of what a pattern names, so every route that takes one — a search, a
/// count, the index-settings and index-mapping APIs, a refresh, the cluster health and the
/// `_cat` listings — resolves it alike, through the one alias-aware resolution the domain
/// record owns ([`state::Domain::resolved_indexes`]).
fn matched_indexes<'a>(
    domain: &'a Domain,
    pattern: &str,
) -> Result<Vec<&'a str>, Box<RtResponse>> {
    let resolved = domain.resolved_indexes(pattern);
    if resolved.is_empty() {
        // A wildcard that matches no index selects none, which is what the engine answers
        // it with; a plain name nothing of the domain carries is the missing index it
        // names.
        if pattern == state::ALL_INDEXES || pattern.contains('*') || pattern.contains('?') {
            return Ok(Vec::new());
        }
        return Err(Box::new(index_not_found(pattern)));
    }
    // The names are answered as the store's own, in its order, so a resolution through an
    // alias and one through an index name report the same indexes in the same sequence.
    Ok(domain
        .indexes
        .keys()
        .filter(|name| resolved.contains(name.as_str()))
        .map(String::as_str)
        .collect())
}

/// The indexes `named` names, in the store's own order and de-duplicated: one index by its
/// name, several by a comma-separated list, a set of them by a wildcard pattern or `_all`,
/// and every index the domain holds for a path that names none.
///
/// Composed of the two readings a search's own resolution is composed of
/// ([`named_patterns`], [`matched_indexes`]), so which of a domain's indexes a path is about
/// is decided in one place. A `<cluster>:<index>` pattern names ANOTHER cluster's index
/// ([`CrossCluster`]), of which this domain holds nothing, so it resolves to no local index
/// here.
fn resolved_indexes<'a>(
    domain: &'a Domain,
    named: Option<&str>,
) -> Result<Vec<&'a str>, Box<RtResponse>> {
    let Some(named) = named else {
        return Ok(domain.indexes.keys().map(String::as_str).collect());
    };
    let mut resolved: BTreeSet<&str> = BTreeSet::new();
    for pattern in named_patterns(named)? {
        if pattern.contains(REMOTE_CLUSTER_SEPARATOR) {
            continue;
        }
        resolved.extend(matched_indexes(domain, pattern)?);
    }
    Ok(resolved.into_iter().collect())
}

/// The VALUES a request target's path carries, one per non-empty segment: each segment
/// PERCENT-DECODED, which is what the surface below reads every route against.
///
/// A URI's path is a sequence of percent-encoded segments (RFC 3986 §3.3), so a character
/// that would otherwise divide the path — the `/` inside a document id taken from a
/// hierarchical key, say — reaches the server escaped, and the value the segment names is
/// what the escape STANDS FOR and never the escape text. Decoding here is what makes that
/// one rule hold for every route at once: the index a path addresses, the id a document is
/// written and read under, and every other name a segment carries.
///
/// The decoding is `percent-encoding`'s, the same one a query value is read with
/// ([`Request::parameter`]), so this states no grammar of its own; a segment whose escapes
/// do not decode as UTF-8 is taken as it arrived, which is the most that can be said about
/// it. An empty segment is dropped, so a trailing or doubled `/` names no segment.
fn path_segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|part| !part.is_empty())
        .map(|part| {
            ::percent_encoding::percent_decode_str(part)
                .decode_utf8()
                .map_or_else(|_| part.to_string(), ::std::borrow::Cow::into_owned)
        })
        .collect()
}

/// One data-plane request, as the surface below reads it.
struct Request<'a> {
    method: &'a str,
    path: &'a str,
    /// The VALUES the request target's path segments carry, which is what every route of
    /// this surface is read against ([`Request::segments`]) — resolved once, when the
    /// request is read, because a segment's value is not a slice of the target it arrived
    /// in.
    segments: Vec<String>,
    query: &'a str,
    body: &'a [u8],
    /// The ARN of the principal the request speaks for, empty when none was resolved.
    principal: &'a str,
    /// What this emulator can show about the identity-based policies in force on that
    /// principal: the documents it holds for it, and whether the principal carries a grant
    /// whose document cannot be read here.
    identity: ingest::IdentityPolicies<'a>,
    /// When this plane began serving the request, so the work it does can state how long it
    /// really took ([`Request::took`]) — which is what a slow-log threshold is compared
    /// against.
    started: ::std::time::Instant,
    /// The instant the request is served at, which every record it produces is stamped with.
    at: SystemTime,
    /// The records the domain's log publishing has of the work this request did — filled in
    /// by the step that did it ([`Request::record_served`]) and delivered once this
    /// service's store is released, so no two stores are held at once.
    published: ::std::cell::RefCell<Vec<state::log_publishing::PublishedRecord>>,
    /// The notifications the work this request did owes a channel — filled in by the
    /// monitor run that decided a trigger fired ([`monitor_run`]) and published once this
    /// service's store is released, for the same reason the log records are.
    notifications: ::std::cell::RefCell<Vec<state::notification_publication::Publication>>,
    /// The monitor run this request PLANNED, for the step that performs the caller's own
    /// source: it is evaluated once this service's store is released and off the async
    /// worker, and the report it answers replaces the placeholder the planning route
    /// returned ([`monitor_run`]). `None` for every request but an alerting execute.
    pending_run: ::std::cell::RefCell<Option<monitor_run::PendingRun>>,
    /// The repository work this request PLANNED, for the step that performs it against the
    /// object store: it is performed once this service's store is released, because no call
    /// holds two service stores at once, and what it comes to REPLACES the placeholder the
    /// planning route returned ([`snapshot`]). `None` for every request but a snapshot
    /// repository's own write.
    pending_snapshot: ::std::cell::RefCell<Option<snapshot::PendingWork>>,
    /// Whether a notification the work this request does owes can be handed to its channel
    /// at all: the in-process route to another service is the operation context's, which a
    /// request carrying no AWS credential has none of. An action of such a request reports
    /// that rather than a send nothing performed.
    can_notify: bool,
    /// Whether the CLUSTER authenticated this request out of its own internal user
    /// database rather than the front door having authenticated an AWS credential for it
    /// ([`fgac::authentication`]).
    ///
    /// Such a request speaks for an identity that exists only inside the cluster, so there
    /// is no AWS principal for IAM to decide about and the cluster's own decision is the
    /// whole of its authorization ([`serve_resolved`]).
    internal_user: bool,
}

impl Request<'_> {
    /// How long serving this request has taken so far — the duration the work it did really
    /// occupied, never a figure chosen for a record.
    fn took(&self) -> ::std::time::Duration {
        self.started.elapsed()
    }

    /// Note what the work this request did produced, as the records `domain`'s enabled
    /// log-publishing options have of it ([`state::log_publishing::records`]).
    ///
    /// Called by the step that DID the work, because that step is what knows which index was
    /// read or written and under which id; nothing is recorded for a domain that publishes
    /// nothing, or for work no threshold of its index makes slow.
    fn record_served(&self, domain: &Domain, served: &[state::log_publishing::Served<'_>]) {
        let records = state::log_publishing::records(domain, served, self.at);
        if !records.is_empty() {
            self.published.borrow_mut().extend(records);
        }
    }

    /// Note the record the security plugin's audit log has of ONE decision the cluster took
    /// about this request: the `category` the plugin states that decision under, at the
    /// `layer` it was taken at, and the cluster action it was about where it was about one.
    ///
    /// Called by the step that TOOK the decision, because that step is what knows which one
    /// it was; whether a record is produced at all is the domain's own audit configuration's
    /// ([`state::audit::records`]), so a domain that has configured none produces none.
    fn record_audited(
        &self,
        domain: &Domain,
        category: &str,
        layer: state::audit::Layer,
        privilege: Option<&str>,
    ) {
        self.record_served(
            domain,
            &[state::log_publishing::Served::Audited(state::audit::Event {
                category,
                layer,
                effective_user: self.principal,
                rest_path: self.path,
                rest_method: self.method,
                privilege,
                index: self.addressed_index(),
            })],
        );
    }
    /// The path's segments, with empty ones dropped so a trailing or doubled `/` names no
    /// segment of its own.
    ///
    /// Each one is the VALUE its segment carries, not the text it arrived as: a path
    /// segment is percent-encoded on the wire, so what the caller named is what the escapes
    /// stand for ([`path_segments`]).
    fn segments(&self) -> Vec<&str> {
        self.segments.iter().map(String::as_str).collect()
    }

    /// The index this request's own path ADDRESSES, or `None` for a path that addresses the
    /// cluster rather than one index.
    ///
    /// Every index-level route of this surface carries that index as its FIRST segment, and
    /// a first segment beginning with the reserved prefix names an API of the engine's own
    /// and never an index ([`reserved`]) — so this is the ONE reading of which index a
    /// request is about, which is what the cluster's own authorization decision is narrowed
    /// by ([`guarded`]).
    fn addressed_index(&self) -> Option<&str> {
        self.segments()
            .first()
            .copied()
            .filter(|first| !reserved(first))
    }

    /// The value the query string gives `name`, PERCENT-DECODED, or `None`.
    ///
    /// A query string carries its values percent-encoded, so what the caller named is what
    /// the escapes stand for and not the escapes themselves — a text to analyze, a status
    /// to wait for, a pipeline id. The decoding is `percent-encoding`'s, so this states no
    /// grammar of its own; a value whose escapes do not decode as UTF-8 is answered as it
    /// arrived, which is the most that can be said about it.
    fn parameter(&self, name: &str) -> Option<String> {
        let raw = self.query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then_some(value)
        })?;
        Some(
            ::percent_encoding::percent_decode_str(raw)
                .decode_utf8()
                .map_or_else(|_| raw.to_string(), ::std::borrow::Cow::into_owned),
        )
    }

    /// The request body as the JSON object the handler expects, `None` for an empty body.
    ///
    /// The refusal is BOXED, as every fallible step of this module's reading is: a rendered
    /// response is far larger than what these functions answer with, and a `Result` whose
    /// error dominates its size is paid for on the success path too.
    fn json(&self) -> Result<Option<Value>, Box<RtResponse>> {
        if self.body.is_empty() {
            return Ok(None);
        }
        ::serde_json::from_slice(self.body)
            .map(Some)
            .map_err(|failed| {
                Box::new(error(
                    400,
                    PARSE_EXCEPTION,
                    &format!("failed to parse: {failed}"),
                ))
            })
    }

    /// This service's data-plane IAM action for the request's method: the `ESHttp*` actions
    /// of the IAM Service Authorization Reference, one per HTTP method.
    fn iam_action(&self) -> &'static str {
        match self.method {
            "GET" => "ESHttpGet",
            "PUT" => "ESHttpPut",
            "POST" => "ESHttpPost",
            "DELETE" => "ESHttpDelete",
            "HEAD" => "ESHttpHead",
            "PATCH" => "ESHttpPatch",
            _ => "ESHttpGet",
        }
    }
}

/// Answer the request against the domain it resolved to, and the remote clusters a
/// cross-cluster search named ([`cross_cluster`]).
///
/// `files` are the dictionary files the domain's packages mount ([`analysis_files`]), which
/// is what the routes whose answer an analysis chain decides read their stop words and
/// synonyms from.
fn serve_resolved(
    domain: &mut Domain,
    request: &Request<'_>,
    now: SystemTime,
    across: &CrossCluster,
    files: &state::analysis::PackageFiles,
) -> RtResponse {
    // IAM first, as AWS takes it: the resource is the domain path the request addresses,
    // built from the VALUES that path's segments carry ([`path_segments`]) — the resource a
    // policy names a document or an index by is the decoded one, so an id a client had to
    // escape to send is decided about under the name it is stored and read under. A request
    // the CLUSTER authenticated out of its own internal user database speaks for an identity
    // that exists only inside the cluster, so there is no AWS principal for IAM to decide
    // about and the cluster's own decision below is the whole of its authorization.
    let resource = format!("{}/{}", domain.arn, request.segments.join("/"));
    if !request.internal_user {
        if let Err(refusal) = ingest::iam_decision(
            domain,
            request.principal,
            request.iam_action(),
            &resource,
            request.identity,
        ) {
            return iam_refused(&refusal);
        }
    }
    let segments = request.segments();
    // The plugins of the Open Distro era answer under the legacy `_opendistro/<plugin>`
    // prefix beside their current `_plugins/<plugin>` one ([`current_plugin_prefix`]), so
    // both spellings of one route reach one handler.
    let segments = current_plugin_prefix(segments);
    match segments.as_slice() {
        [] => match request.method {
            "GET" | "HEAD" => guarded(domain, request, fgac::ACTION_CLUSTER_STATE, |domain| {
                ok(&banner(domain))
            }),
            _ => wrong_method(request, "GET, HEAD"),
        },
        ["_cluster", "health"] => match request.method {
            "GET" => guarded(domain, request, fgac::ACTION_CLUSTER_HEALTH, |domain| {
                cluster_health(domain, request, None)
            }),
            _ => wrong_method(request, "GET"),
        },
        ["_cluster", "health", index] => match request.method {
            "GET" => guarded(domain, request, fgac::ACTION_CLUSTER_HEALTH, |domain| {
                cluster_health(domain, request, Some(index))
            }),
            _ => wrong_method(request, "GET"),
        },
        ["_index_template", name] => template_request(domain, request, name, false),
        ["_template", name] => template_request(domain, request, name, true),
        // The nodes of the cluster, and one node of it by its id.
        ["_nodes"] => match request.method {
            "GET" => guarded(domain, request, fgac::ACTION_NODES_INFO, |domain| {
                ok(&nodes_info(domain, None))
            }),
            _ => wrong_method(request, "GET"),
        },
        ["_nodes", node] if !NODES_SUB_APIS.contains(node) => match request.method {
            "GET" => guarded(domain, request, fgac::ACTION_NODES_INFO, |domain| {
                ok(&nodes_info(domain, Some(node)))
            }),
            _ => wrong_method(request, "GET"),
        },
        // The node STATISTICS, over every node and over one. The segment after `stats` names
        // the statistics asked for, and this cluster publishes ONE of them
        // ([`nodes::PUBLISHED_METRIC`]): a path naming another is answered the way every
        // other path this plane does not serve is, rather than with a report carrying no
        // section for the metric it named.
        ["_nodes", "stats"] => match request.method {
            "GET" => guarded(domain, request, fgac::ACTION_NODES_INFO, |domain| {
                ok(&nodes_stats(domain, None))
            }),
            _ => wrong_method(request, "GET"),
        },
        ["_nodes", "stats", metric] if nodes::publishes_metric(metric) => match request.method {
            "GET" => guarded(domain, request, fgac::ACTION_NODES_INFO, |domain| {
                ok(&nodes_stats(domain, None))
            }),
            _ => wrong_method(request, "GET"),
        },
        ["_nodes", node, "stats"] => match request.method {
            "GET" => guarded(domain, request, fgac::ACTION_NODES_INFO, |domain| {
                ok(&nodes_stats(domain, Some(node)))
            }),
            _ => wrong_method(request, "GET"),
        },
        ["_nodes", node, "stats", metric] if nodes::publishes_metric(metric) => {
            match request.method {
                "GET" => guarded(domain, request, fgac::ACTION_NODES_INFO, |domain| {
                    ok(&nodes_stats(domain, Some(node)))
                }),
                _ => wrong_method(request, "GET"),
            }
        }
        ["_cluster", "settings"] => cluster_settings_request(domain, request),
        // The SNAPSHOT API: the repositories this cluster has registered, the snapshots it
        // has taken into one, and the restore of one. The reserved first segment names it
        // before any index-level arm below could read it as an index name.
        ["_snapshot"] => snapshot::repositories_request(domain, request),
        ["_snapshot", repository] => snapshot::repository_request(domain, request, repository),
        ["_snapshot", repository, named] => {
            snapshot::snapshots_request(domain, request, repository, named, now)
        }
        ["_snapshot", repository, named, "_restore"] => {
            snapshot::restore_request(domain, request, repository, named)
        }
        // The BULK API, and its index-scoped form: the reserved segment names it before
        // any arm below could read it as an index name.
        ["_bulk"] => bulk::bulk_request(domain, request, None, now),
        [index, "_bulk"] if !reserved(index) => {
            bulk::bulk_request(domain, request, Some(index), now)
        }
        // The alias actions, and the per-index spellings of the same.
        ["_aliases"] => aliases::alias_actions(domain, request),
        ["_alias"] => aliases::index_alias_request(domain, request, None, None),
        ["_alias", alias] => aliases::index_alias_request(domain, request, None, Some(alias)),
        [index, "_alias"] if !reserved(index) => {
            aliases::index_alias_request(domain, request, Some(index), None)
        }
        [index, "_alias", alias] if !reserved(index) => {
            aliases::index_alias_request(domain, request, Some(index), Some(alias))
        }
        // An index's own settings and mappings, for every index and for one. The third
        // segment of a settings read is the setting-name filter it narrows the report with.
        ["_settings"] => index_settings::index_settings_request(domain, request, None, None),
        ["_settings", filter] => {
            index_settings::index_settings_request(domain, request, None, Some(filter))
        }
        [index, "_settings"] if !reserved(index) => {
            index_settings::index_settings_request(domain, request, Some(index), None)
        }
        [index, "_settings", filter] if !reserved(index) => {
            index_settings::index_settings_request(domain, request, Some(index), Some(filter))
        }
        ["_mapping"] => index_mapping::index_mapping_request(domain, request, None),
        [index, "_mapping"] if !reserved(index) => {
            index_mapping::index_mapping_request(domain, request, Some(index))
        }
        // The compact listings.
        ["_cat", "indices"] => cat::indices(domain, request, None),
        ["_cat", "indices", index] => cat::indices(domain, request, Some(index)),
        ["_cat", "nodes"] => cat::nodes(domain, request),
        // The component templates a composable index template is composed of.
        ["_component_template"] => {
            component_templates::component_template_request(domain, request, None)
        }
        ["_component_template", name] => {
            component_templates::component_template_request(domain, request, Some(name))
        }
        ["_ingest", "pipeline"] => match request.method {
            "GET" => guarded(domain, request, fgac::ACTION_GET_PIPELINE, |domain| {
                ok(&pipeline_view(domain, None))
            }),
            _ => wrong_method(request, "GET"),
        },
        ["_ingest", "pipeline", id] => pipeline_request(domain, request, id),
        ["_ingest", "pipeline", id, "_simulate"] => match request.method {
            "GET" | "POST" => guarded(domain, request, fgac::ACTION_SIMULATE_PIPELINE, |domain| {
                simulate_pipeline(domain, request, id, now)
            }),
            _ => wrong_method(request, "GET, POST"),
        },
        ["_plugins", "_ism", "policies", id] => ism_policy_request(domain, request, id, now),
        // The ALERTING plugin: the monitors a client writes, the runs of one, the alerts
        // they left, and the plugin's own legacy destinations. The reserved sub-paths come
        // before the arm that reads the segment as a monitor id, so neither is read as one.
        ["_plugins", "_alerting", "monitors"] => alerting::monitors_request(domain, request, now),
        ["_plugins", "_alerting", "monitors", "_execute"] => {
            alerting::execute_submitted(domain, request, now, files)
        }
        ["_plugins", "_alerting", "monitors", "alerts"] => alerting::alerts_request(domain, request),
        ["_plugins", "_alerting", "monitors", id, "_execute"] => {
            alerting::execute_request(domain, request, id, now, files)
        }
        ["_plugins", "_alerting", "monitors", id] => {
            alerting::monitor_request(domain, request, id, now)
        }
        ["_plugins", "_alerting", "destinations"] => {
            alerting::destinations_request(domain, request, now)
        }
        ["_plugins", "_alerting", "destinations", id] => {
            alerting::destination_request(domain, request, id, now)
        }
        // The NOTIFICATIONS plugin's channel configurations, which an alerting action's
        // destination names.
        ["_plugins", "_notifications", "configs"] => {
            notifications::configs_request(domain, request, now)
        }
        ["_plugins", "_notifications", "configs", config_id] => {
            notifications::config_request(domain, request, config_id, now)
        }
        // The security plugin's own "who am I". It is NOT guarded: the plugin answers it
        // for the authenticated user whatever roles that user holds, which is what makes
        // it the route a client checks its own identity and permissions with before it
        // asks for anything. The IAM decision above still applies to it.
        ["_plugins", "_security", "authinfo"] => match request.method {
            "GET" => ok(&auth_info(domain, request)),
            _ => wrong_method(request, "GET"),
        },
        // The plugin's own account route, answered for the authenticated caller for the
        // same reason `authinfo` is: it is what a client checks its credential with.
        ["_plugins", "_security", "api", "account"] => {
            internal_users::account_request(domain, request)
        }
        // The INTERNAL USER DATABASE, which only the master user may change.
        ["_plugins", "_security", "api", "internalusers"] => {
            internal_users::users_request(domain, request, None)
        }
        ["_plugins", "_security", "api", "internalusers", name] => {
            internal_users::users_request(domain, request, Some(name))
        }
        ["_plugins", "_security", "api", "roles", name] => security_role(domain, request, name),
        ["_plugins", "_security", "api", "rolesmapping", name] => {
            security_role_mapping(domain, request, name)
        }
        // The plugin's AUDIT CONFIGURATION: which of the decisions the cluster takes about
        // a request leave a record in the log group the domain publishes `AUDIT_LOGS` to.
        ["_plugins", "_security", "api", "audit"] => {
            security_audit::audit_request(domain, request)
        }
        ["_plugins", "_security", "api", "audit", "config"] => {
            security_audit::audit_config_request(domain, request)
        }
        // The tokens a text produces, for the whole cluster and for one index's own
        // analysis settings. Both arms come before the single- and two-segment arms below,
        // so neither reserved name is read as an index.
        ["_analyze"] => analyze::analyze_request(domain, request, None, files),
        [index, "_analyze"] if !reserved(index) => {
            analyze::analyze_request(domain, request, Some(index), files)
        }
        // A refresh with no index named is the whole cluster's, and this arm comes before
        // the single-segment one below so the reserved name is never read as an index.
        ["_refresh"] => match request.method {
            "GET" | "POST" => guarded(domain, request, fgac::ACTION_REFRESH, |domain| {
                refresh(domain, request, None)
            }),
            _ => wrong_method(request, "GET, POST"),
        },
        // A search or a count with no index named is the whole cluster's — the engine reads
        // every index it holds, which is what `_all` names — and both arms come before the
        // single-segment one below so neither reserved name is read as an index.
        ["_search"] => match request.method {
            "GET" | "POST" => guarded(domain, request, fgac::ACTION_SEARCH, |domain| {
                search(domain, request, state::ALL_INDEXES, across, files)
            }),
            _ => wrong_method(request, "GET, POST"),
        },
        ["_count"] => match request.method {
            "GET" | "POST" => guarded(domain, request, fgac::ACTION_COUNT, |domain| {
                count(domain, request, state::ALL_INDEXES, across, files)
            }),
            _ => wrong_method(request, "GET, POST"),
        },
        // The index-level routes. Each is guarded by [`reserved`]: a first segment that
        // begins with the reserved prefix names an API of the engine's own and never an
        // index, so a reserved path this surface does not serve is answered as an unserved
        // path rather than as a request about an index no cluster could hold.
        [index] if !reserved(index) => index_request(domain, request, index),
        // `_doc` upserts; `_create` must NOT overwrite, and neither must a `_doc` write
        // whose `op_type` asks for a create.
        [index, "_doc"] if !reserved(index) => {
            document_write(domain, request, index, None, Creates::Upsert, now)
        }
        [index, "_doc", id] if !reserved(index) => document_request(
            domain,
            request,
            index,
            id,
            requested_op_type(request),
            now,
            files,
        ),
        [index, "_create", id] if !reserved(index) => {
            document_request(domain, request, index, id, Creates::Only, now, files)
        }
        // The PARTIAL-DOCUMENT UPDATE of one document: the fields the request's `doc`
        // names merged onto the document the index holds.
        [index, "_update", id] if !reserved(index) => {
            document_update::update_request(domain, request, index, id, now)
        }
        [index, "_count"] if !reserved(index) => match request.method {
            "GET" | "POST" => guarded(domain, request, fgac::ACTION_COUNT, |domain| {
                count(domain, request, index, across, files)
            }),
            _ => wrong_method(request, "GET, POST"),
        },
        [index, "_search"] if !reserved(index) => match request.method {
            "GET" | "POST" => guarded(domain, request, fgac::ACTION_SEARCH, |domain| {
                search(domain, request, index, across, files)
            }),
            _ => wrong_method(request, "GET, POST"),
        },
        // The documents a query selects, removed: the write form of the search above, which
        // resolves the same documents the same way.
        [index, "_delete_by_query"] if !reserved(index) => {
            delete_by_query::delete_by_query_request(domain, request, index, files)
        }
        [index, "_refresh"] if !reserved(index) => match request.method {
            "GET" | "POST" => guarded(domain, request, fgac::ACTION_REFRESH, |domain| {
                refresh(domain, request, Some(index))
            }),
            _ => wrong_method(request, "GET, POST"),
        },
        _ => no_handler(request),
    }
}

/// Whether a write may REPLACE a document the index already holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Creates {
    /// `_doc` without an `op_type`: a write to an id the index holds replaces it.
    Upsert,
    /// `_create`, and `_doc` with `op_type=create`: a write to an id the index already
    /// holds is REFUSED rather than replacing the document.
    Only,
}

/// The segment a plugin's API is named by today.
const PLUGIN_SEGMENT: &str = "_plugins";

/// The segment it was named by in the Open Distro era, which the engine still registers
/// every such plugin's routes under.
const LEGACY_PLUGIN_SEGMENT: &str = "_opendistro";

/// The plugins this surface serves that existed under that name, and so answer under both
/// prefixes: the SECURITY plugin, the INDEX-MANAGEMENT plugin and the ALERTING plugin. The
/// notifications plugin came after the rename and has no legacy prefix, which is why it is
/// not here.
const LEGACY_PLUGINS: [&str; 3] = ["_security", "_ism", "_alerting"];

/// `segments` with a legacy plugin prefix read as the current one.
///
/// A plugin of the Open Distro era kept the routes it had then: the same resource answers
/// under `_opendistro/<plugin>/…` and under `_plugins/<plugin>/…`, and the tooling and the
/// setup scripts written before the rename still address the first. Reading the one as the
/// other HERE is what keeps one handler per resource — a second set of arms would be a
/// second place for a route to drift — and the request's own path is untouched, so a refusal
/// still names the URI the caller sent.
fn current_plugin_prefix(segments: Vec<&str>) -> Vec<&str> {
    match segments.split_first() {
        Some((&LEGACY_PLUGIN_SEGMENT, rest))
            if rest
                .first()
                .is_some_and(|plugin| LEGACY_PLUGINS.contains(plugin)) =>
        {
            let mut current = Vec::with_capacity(segments.len());
            current.push(PLUGIN_SEGMENT);
            current.extend_from_slice(rest);
            current
        }
        _ => segments,
    }
}

/// Whether `segment` is a RESERVED first path segment of the engine's REST surface rather
/// than an index name.
///
/// An index name may not begin with the reserved prefix
/// ([`state::RESERVED_SEGMENT_PREFIX`], which `IndexName`'s own pattern forbids as a first
/// character), so a first segment that does names an API — served above, or served by
/// nothing. Every index-level route is guarded by this, which is what keeps a reserved
/// endpoint this surface does not serve from being answered as a request about an index:
/// the refusal then states that the path has no handler instead of listing the methods
/// some other route takes.
fn reserved(segment: &str) -> bool {
    // `_all` is the one reserved name that IS an index selector — it names every index of
    // the cluster — so it is not an API name and the index-level routes take it.
    segment.starts_with(state::RESERVED_SEGMENT_PREFIX) && segment != state::ALL_INDEXES
}

/// What a refusal of ONE write is, apart from how it is rendered: the status, the engine's
/// error type and the reason.
///
/// Held as a value because the same condition is answered two ways — as the whole answer
/// to a single document write, and as ONE item's error inside a bulk answer, which is 200
/// however its items fared — so neither can word a condition the other words differently.
#[derive(Debug, Clone)]
pub(super) struct Refusal {
    status: u16,
    error_type: String,
    reason: String,
}

impl Refusal {
    /// This refusal as the whole answer to one request.
    fn response(&self) -> RtResponse {
        error(self.status, &self.error_type, &self.reason)
    }

    /// The status a bulk item carrying this refusal reports.
    pub(super) fn status(&self) -> u16 {
        self.status
    }

    /// This refusal as the `error` object of one bulk item.
    pub(super) fn item_error(&self) -> Value {
        json!({ "type": self.error_type, "reason": self.reason })
    }

    /// A write this emulator's own bounds, or a member the request did not carry, refuses.
    pub(super) fn of_illegal_argument(reason: &str) -> Self {
        Self {
            status: 400,
            error_type: ingest::ILLEGAL_ARGUMENT_EXCEPTION.to_string(),
            reason: reason.to_string(),
        }
    }

    /// A document these bytes are not: the engine's own parse failure for a source it
    /// could not read as one document.
    pub(super) fn of_mapper_parsing(reason: &str) -> Self {
        Self {
            status: 400,
            error_type: ingest::MAPPER_PARSING_EXCEPTION.to_string(),
            reason: reason.to_string(),
        }
    }

    /// An update of a document the index does not hold, which the engine refuses rather
    /// than creating one for it.
    pub(super) fn of_document_missing(id: &str) -> Self {
        Self {
            status: 404,
            error_type: DOCUMENT_MISSING_EXCEPTION.to_string(),
            reason: format!("[{id}]: document missing"),
        }
    }

    /// An index the cluster does not hold.
    pub(super) fn of_index_not_found(index_name: &str) -> Self {
        Self {
            status: 404,
            error_type: ingest::INDEX_NOT_FOUND_EXCEPTION.to_string(),
            reason: format!("no such index [{index_name}]"),
        }
    }

    /// The refusal the one data-plane write path answered with, in this form.
    pub(super) fn of_write(refusal: &ingest::WriteRefusal) -> Self {        match refusal {
            ingest::WriteRefusal::Unavailable { message } => Self {
                status: 503,
                error_type: "cluster_block_exception".to_string(),
                reason: message.clone(),
            },
            ingest::WriteRefusal::Rejected {
                error_type,
                message,
            } => Self {
                status: match *error_type {
                    ingest::SECURITY_EXCEPTION => 403,
                    // An index the cluster does not hold is a missing resource, not a
                    // malformed request.
                    ingest::INDEX_NOT_FOUND_EXCEPTION => 404,
                    _ => 400,
                },
                error_type: (*error_type).to_string(),
                reason: message.clone(),
            },
        }
    }
}

/// The REVISION a conditional write is taken against: the `if_seq_no`/`if_primary_term`
/// pair "used to prevent concurrent modification", which a client takes from the answer to
/// the write or the read it based its change on.
///
/// Both are read together, as the engine takes them: a request naming one without the
/// other names no revision to compare against.
fn conditional_revision(
    if_seq_no: Option<u64>,
    if_primary_term: Option<u64>,
) -> Option<(u64, u64)> {
    Some((if_seq_no?, if_primary_term?))
}

/// The conditional revision the query string of a single document write names.
fn requested_revision(request: &Request<'_>) -> Option<(u64, u64)> {
    conditional_revision(
        request.parameter("if_seq_no").and_then(|v| v.parse().ok()),
        request
            .parameter("if_primary_term")
            .and_then(|v| v.parse().ok()),
    )
}

/// Why a write to `id` may not be taken, or `None` when it may: the two conditions a write
/// is held to BEFORE the document is stored.
///
/// `Creates::Only` is the one `_create` and `op_type=create` ask for — a write to an id the
/// index already holds is refused rather than replacing the document — and `conditional` is
/// the compare-and-set a client performs with the `_seq_no`/`_primary_term` pair it read:
/// the write proceeds only while the document is still at that revision, which is what
/// makes exactly one of two concurrent writers win.
fn write_conflict(
    domain: &Domain,
    index_name: &str,
    id: Option<&str>,
    creates: Creates,
    conditional: Option<(u64, u64)>,
) -> Option<Refusal> {
    let addressed = id.filter(|id| !id.is_empty());
    let held = addressed
        .and_then(|id| domain.indexes.get(index_name)?.documents.get(id))
        .cloned();
    let id = addressed.unwrap_or_default();
    if creates == Creates::Only {
        if let Some(held) = &held {
            return Some(Refusal {
                status: 409,
                error_type: VERSION_CONFLICT_ENGINE_EXCEPTION.to_string(),
                reason: format!(
                    "[{id}]: version conflict, document already exists (current version [{}])",
                    held.version
                ),
            });
        }
    }
    let (seq_no, primary_term) = conditional?;
    let required =
        format!("[{id}]: version conflict, required seqNo [{seq_no}], primary term [{primary_term}].");
    match held {
        Some(held) if held.seq_no == seq_no && held.primary_term == primary_term => None,
        Some(held) => Some(Refusal {
            status: 409,
            error_type: VERSION_CONFLICT_ENGINE_EXCEPTION.to_string(),
            reason: format!(
                "{required} current document has seqNo [{}] and primary term [{}]",
                held.seq_no, held.primary_term
            ),
        }),
        None => Some(Refusal {
            status: 409,
            error_type: VERSION_CONFLICT_ENGINE_EXCEPTION.to_string(),
            reason: format!("{required} but no document was found"),
        }),
    }
}

/// Run every processor the ingest pipeline `id` declares over `source`, in the order the
/// pipeline declares them — the preprocessing a write naming a pipeline asks for.
///
/// The ONE application of a pipeline to a document on its way in, so a single write and a
/// bulk item are preprocessed by the same processors and refused by the same sentences. A
/// pipeline the cluster does not hold, or a processor that cannot act on this document,
/// refuses the write rather than storing the document unprocessed.
fn apply_pipeline(domain: &Domain, id: &str, source: &mut Value) -> Result<(), Refusal> {
    let Some(pipeline) = domain.ingest_pipelines.get(id) else {
        return Err(Refusal {
            status: 404,
            error_type: RESOURCE_NOT_FOUND_EXCEPTION.to_string(),
            reason: format!("pipeline [{id}] is missing"),
        });
    };
    let failed = |failed: pipeline::ProcessorError| Refusal {
        status: 400,
        error_type: failed.error_type.to_string(),
        reason: failed.reason,
    };
    let processors = pipeline::parse(&pipeline.definition).map_err(failed)?;
    for processor in &processors {
        processor.apply(source).map_err(failed)?;
    }
    Ok(())
}

/// How many DATA nodes the domain's cluster has, which is what decides how many copies of
/// a shard can be assigned: one node holds at most one copy of a shard.
fn data_nodes(domain: &Domain) -> i32 {
    domain.cluster_config.instance_count.unwrap_or(1).max(1)
}

/// How many of an index's `copies` replica copies per shard the cluster can ASSIGN: a
/// replica needs a node other than the one holding its primary, so the primary's node
/// leaves `data_nodes - 1` nodes for them and a copy past that has nowhere to go.
///
/// The ONE arithmetic every reader of the cluster's allocation uses — the cluster health,
/// an index's `_cat` row, a write's `_shards` and a refresh's — so no two of them can
/// report one index's allocation differently.
fn assigned_replicas(copies: i64, data_nodes: i32) -> i64 {
    copies.min(i64::from(data_nodes).saturating_sub(1).max(0))
}

/// The health `unassigned` unassigned shard copies leave the indexes they belong to in:
/// `yellow` while a copy has nowhere to go, `green` with every copy assigned.
///
/// A primary is always assignable here — there is always a data node to hold it — so no
/// index of this cluster is `red`.
fn health_of(unassigned: i64) -> &'static str {
    if unassigned > 0 {
        "yellow"
    } else {
        "green"
    }
}

/// What `op_type` asks of a `_doc` write: `create` is the one value that forbids a
/// replacement, and every other (including none) leaves the write an upsert.
fn requested_op_type(request: &Request<'_>) -> Creates {
    match request.parameter("op_type").as_deref() {
        Some("create") => Creates::Only,
        _ => Creates::Upsert,
    }
}

/// Take the cluster's own authorization decision for `action`, then `answer` the request.
///
/// Every route goes through this, so no route can serve a domain with fine-grained access
/// control switched on without the decision [`fgac`] owns having been taken for it — which
/// is also why the security plugin's audit record of that decision is left HERE
/// ([`Request::record_audited`]): it is the one place the cluster decides what the caller
/// may do.
fn guarded(
    domain: &mut Domain,
    request: &Request<'_>,
    action: &str,
    answer: impl FnOnce(&mut Domain) -> RtResponse,
) -> RtResponse {
    if let Some(reason) =
        fgac::refusal(domain, request.principal, action, request.addressed_index())
    {
        request.record_audited(
            domain,
            state::audit::CATEGORY_MISSING_PRIVILEGES,
            state::audit::Layer::Transport,
            Some(action),
        );
        return error(403, ingest::SECURITY_EXCEPTION, &reason);
    }
    request.record_audited(
        domain,
        state::audit::CATEGORY_GRANTED_PRIVILEGES,
        state::audit::Layer::Transport,
        Some(action),
    );
    answer(domain)
}

/// The security roles this request's caller holds on `domain`, as the plugin reports them:
/// the ones its role mappings and its own user record bind it to, with the all-permitting
/// role first for the MASTER user — the mapping the control plane itself makes when
/// `MasterUserOptions` names a principal.
///
/// One reading, because two routes report it: the plugin's `authinfo` and its account API
/// ([`internal_users`]).
fn held_roles(domain: &Domain, request: &Request<'_>) -> Vec<String> {
    let mut roles = fgac::mapped_roles(domain, request.principal);
    if fgac::is_master(domain, request.principal)
        && !roles.iter().any(|role| role == fgac::ALL_ACCESS_ROLE)
    {
        roles.insert(0, fgac::ALL_ACCESS_ROLE.to_string());
    }
    roles
}

/// `GET /_plugins/_security/authinfo` — who the caller is, as the security plugin knows
/// it.
///
/// The one route of the plugin that is answered for the authenticated user whatever
/// permissions it holds, which is what a client checks its own identity with: it names
/// the user, the BACKEND ROLES that user arrived with — for an AWS principal, the IAM
/// identities its credential speaks for, which is what a role mapping names it by
/// ([`fgac::backend_roles`]) — and the security roles this domain's mappings bind it to.
///
/// Every value is read from the request's principal and the domain's own security
/// configuration.
fn auth_info(domain: &Domain, request: &Request<'_>) -> Value {
    let roles = held_roles(domain, request);
    // "user_requested_tenant" is the tenant this request asked for, and it asked for
    // none; the tenant a user always has is its own.
    let mut tenants = ::serde_json::Map::new();
    tenants.insert(request.principal.to_string(), Value::Bool(true));
    json!({
        "user": fgac::user_description(request.principal),
        "user_name": request.principal,
        "user_requested_tenant": Value::Null,
        "backend_roles": fgac::backend_roles(request.principal),
        "custom_attribute_names": Vec::<String>::new(),
        "roles": roles,
        "tenants": Value::Object(tenants),
        "principal": Value::Null,
    })
}

/// The cluster banner a `GET /` answers: what this cluster IS, from the domain's own
/// record.
fn banner(domain: &Domain) -> Value {
    let (distribution, number) = engine_distribution(&domain.engine_version);
    json!({
        "name": domain.domain_name,
        "cluster_name": cluster_name(domain),
        "cluster_uuid": stable_uuid(&domain.arn),
        "version": { "distribution": distribution, "number": number },
        "tagline": TAGLINE,
    })
}

/// The tagline every OpenSearch cluster answers a `GET /` with.
const TAGLINE: &str = "The OpenSearch Project: https://opensearch.org/";

/// A domain's cluster name, which Amazon OpenSearch Service forms from the account that
/// owns it and the domain's own name.
///
/// The ONE reading is [`state::cluster_name`]'s: the plugin's audit records state the same
/// cluster name as the banner does, so neither can drift from the other.
fn cluster_name(domain: &Domain) -> String {
    state::cluster_name(domain)
}

/// The distribution and version number the domain's `EngineVersion` names: the value is
/// `<EngineType>_<major>.<minor>` (`OpenSearch_2.11`), so the engine type names the
/// distribution and the rest is the version, with the patch level it omits reported as 0.
fn engine_distribution(engine_version: &str) -> (String, String) {
    let (engine_type, version) = engine_version
        .split_once('_')
        .unwrap_or((state::ENGINE_TYPE_OPENSEARCH, engine_version));
    let number = if version.split('.').count() < 3 {
        format!("{version}.0")
    } else {
        version.to_string()
    };
    (engine_type.to_ascii_lowercase(), number)
}

/// A UUID that is a function of what it names, so the same resource reports the same id on
/// every read and across restarts without anything having to be remembered.
fn stable_uuid(seed: &str) -> String {
    ::uuid::Uuid::new_v5(&::uuid::Uuid::NAMESPACE_OID, seed.as_bytes()).to_string()
}

/// The cluster health a `GET /_cluster/health` answers, computed from the domain's own
/// cluster configuration and the indexes the request is about: every index the domain
/// holds, or the ONE the path names.
///
/// A shard copy needs a node of its own, so an index asking for more replica copies than
/// the cluster has nodes to put them on leaves copies unassigned and the cluster is
/// `yellow` — which is the health such a domain really reports. With every copy assigned
/// it is `green`. Lowering the replica count to what the topology can hold is what turns
/// it green again, which is why the arithmetic is the one shared
/// [`assigned_replicas`] and not a test for a single node.
///
/// `wait_for_status` is the one wait this emulator can answer without waiting: the cluster
/// is already settled, so the status it has is the status it will have — a request asking
/// for a healthier one than it reached is answered `timed_out: true` with the status it
/// really is, which is what the engine answers when the wait expires.
fn cluster_health(
    domain: &Domain,
    request: &Request<'_>,
    index_name: Option<&str>,
) -> RtResponse {
    let named: Vec<&state::Index> = match index_name {
        None => domain.indexes.values().collect(),
        Some(name) => {
            let resolved = domain.resolved_indexes(name);
            if resolved.is_empty() {
                return index_not_found(name);
            }
            resolved
                .iter()
                .filter_map(|name| domain.indexes.get(name))
                .collect()
        }
    };
    let data_nodes = data_nodes(domain);
    let master_nodes = if domain.cluster_config.dedicated_master_enabled == Some(true) {
        domain.cluster_config.dedicated_master_count.unwrap_or(0)
    } else {
        0
    };
    let mut primaries = 0_i64;
    let mut replicas = 0_i64;
    let mut assignable = 0_i64;
    for index in named {
        // Both come from the index's own settings, which a caller wrote, so both are
        // clamped before they are multiplied out.
        let shards = setting_number(&index.schema, NUMBER_OF_SHARDS)
            .unwrap_or(1)
            .clamp(1, MAX_SHARDS_PER_INDEX);
        let copies = setting_number(&index.schema, NUMBER_OF_REPLICAS)
            .unwrap_or(1)
            .clamp(0, MAX_SHARDS_PER_INDEX);
        primaries = primaries.saturating_add(shards);
        replicas = replicas.saturating_add(shards.saturating_mul(copies));
        assignable = assignable
            .saturating_add(shards.saturating_mul(assigned_replicas(copies, data_nodes)));
    }
    // Each replica copy needs a node other than the one holding its primary, so the
    // copies past what the topology can hold are the unassigned ones.
    let unassigned = replicas.saturating_sub(assignable);
    let active = primaries.saturating_add(assignable);
    let total = primaries.saturating_add(replicas);
    let status = health_of(unassigned);
    let timed_out = request
        .parameter("wait_for_status")
        .as_deref()
        .and_then(health_rank)
        .is_some_and(|wanted| health_rank(status).is_some_and(|reached| reached > wanted));
    let percent = if total == 0 {
        100.0
    } else {
        #[allow(clippy::cast_precision_loss)]
        {
            (active as f64 / total as f64) * 100.0
        }
    };
    ok(&json!({
        "cluster_name": cluster_name(domain),
        "status": status,
        "timed_out": timed_out,
        "number_of_nodes": i64::from(data_nodes) + i64::from(master_nodes),
        "number_of_data_nodes": i64::from(data_nodes),
        "discovered_master": true,
        "active_primary_shards": primaries,
        "active_shards": active,
        "relocating_shards": 0,
        "initializing_shards": 0,
        "unassigned_shards": unassigned,
        "delayed_unassigned_shards": 0,
        "number_of_pending_tasks": 0,
        "number_of_in_flight_fetch": 0,
        "task_max_waiting_in_queue_millis": 0,
        "active_shards_percent_as_number": percent,
    }))
}

/// How the three cluster health statuses order, worst last — what `wait_for_status`
/// compares against. `None` for a value that names none of them.
fn health_rank(status: &str) -> Option<u8> {
    match status {
        "green" => Some(0),
        "yellow" => Some(1),
        "red" => Some(2),
        _ => None,
    }
}

/// A `GET` or `PUT` on the cluster's own settings.
fn cluster_settings_request(domain: &mut Domain, request: &Request<'_>) -> RtResponse {
    match request.method {
        "GET" => guarded(
            domain,
            request,
            fgac::ACTION_GET_CLUSTER_SETTINGS,
            |domain| {
                ok(&json!({
                    state::SETTINGS_PERSISTENT:
                        reported_text_settings(&domain.cluster_settings.persistent, request),
                    state::SETTINGS_TRANSIENT:
                        reported_text_settings(&domain.cluster_settings.transient, request),
                }))
            },
        ),
        "PUT" => guarded(
            domain,
            request,
            fgac::ACTION_UPDATE_CLUSTER_SETTINGS,
            |domain| update_cluster_settings(domain, request),
        ),
        _ => wrong_method(request, "GET, PUT"),
    }
}

/// Apply the settings update the request's body asks for.
///
/// "Settings can be specified in a flattened or expanded format": either spelling names the
/// same setting, so a nested object is FLATTENED to the dotted name the setting has and one
/// spelling is stored. A setting written `null` is REMOVED from its scope, which is how a
/// client resets one, and a scope the request does not name is left exactly as it was: an
/// update is of the settings it names.
fn update_cluster_settings(domain: &mut Domain, request: &Request<'_>) -> RtResponse {
    let body = match request.json() {
        Ok(Some(Value::Object(members))) => members,
        Ok(_) => {
            return error(
                400,
                PARSE_EXCEPTION,
                "failed to parse: expected the body to be a JSON object naming the settings \
                 scopes to update",
            )
        }
        Err(refusal) => return *refusal,
    };
    // Read and validate the whole request before anything is stored, so a refused update
    // leaves the cluster's settings as they were.
    let mut updates: Vec<ScopeUpdate> = Vec::new();
    for (scope, written) in &body {
        if !state::CLUSTER_SETTING_SCOPES.contains(&scope.as_str()) {
            return error(
                400,
                X_CONTENT_PARSE_EXCEPTION,
                &format!("[cluster_update_settings_request] unknown field [{scope}]"),
            );
        }
        let Some(written) = written.as_object() else {
            return error(
                400,
                X_CONTENT_PARSE_EXCEPTION,
                &format!(
                    "[cluster_update_settings_request] [{scope}] must be an object naming \
                     settings"
                ),
            );
        };
        let mut flattened = Vec::new();
        if let Err(refusal) = flatten_settings(written, &mut String::new(), &mut flattened) {
            return *refusal;
        }
        // The cluster holds its settings as TEXT, so each value an update wrote is read
        // as the text it is held under.
        let mut settings: Vec<SettingUpdate> = Vec::new();
        for (name, value) in flattened {
            let held = match value.as_ref().map(|value| setting_text(&name, value)) {
                Some(Ok(text)) => Some(text),
                Some(Err(refusal)) => return *refusal,
                None => None,
            };
            settings.push((name, held));
        }
        updates.push((scope.as_str() == state::SETTINGS_TRANSIENT, settings));
    }
    // Past the quota the WHOLE update is refused, before anything is stored: a setting a
    // caller wrote and this cluster dropped would be indistinguishable from one it holds,
    // and the scope's bound is a bound on what the cluster takes, not on what it keeps.
    for (transient, settings) in &updates {
        let (scope, held) = if *transient {
            (state::SETTINGS_TRANSIENT, &domain.cluster_settings.transient)
        } else {
            (
                state::SETTINGS_PERSISTENT,
                &domain.cluster_settings.persistent,
            )
        };
        let mut names: BTreeSet<&str> = held.keys().map(String::as_str).collect();
        for (name, value) in settings {
            if value.is_some() {
                names.insert(name.as_str());
            } else {
                names.remove(name.as_str());
            }
        }
        if names.len() > state::CLUSTER_SETTING_QUOTA {
            return error(
                400,
                ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                &format!(
                    "cluster [{}] takes at most {} {scope} settings, so the update naming {} of \
                     them was not applied",
                    cluster_name(domain),
                    state::CLUSTER_SETTING_QUOTA,
                    names.len()
                ),
            );
        }
    }
    for (transient, settings) in updates {
        let held = if transient {
            &mut domain.cluster_settings.transient
        } else {
            &mut domain.cluster_settings.persistent
        };
        for (name, value) in settings {
            match value {
                Some(value) => {
                    held.insert(name, value);
                }
                // "You can reset a setting by assigning it a null value."
                None => {
                    held.remove(&name);
                }
            }
        }
    }
    ok(&json!({
        "acknowledged": true,
        state::SETTINGS_PERSISTENT:
            reported_text_settings(&domain.cluster_settings.persistent, request),
        state::SETTINGS_TRANSIENT:
            reported_text_settings(&domain.cluster_settings.transient, request),
    }))
}

/// One setting an update writes: its flat dotted NAME, and the value it is written with —
/// `None` for a setting the update RESETS.
type SettingUpdate = (String, Option<String>);

/// One setting a settings body names, as [`flatten_settings`] reads it out: its flat
/// dotted NAME, and the value written for it — `None` for a setting the body RESETS.
type WrittenSetting = (String, Option<Value>);

/// The TEXT a cluster setting is held as, for the value an update wrote it with.
///
/// The cluster's settings are strings, so a number or a boolean is held as the text it was
/// written as and one spelling is stored. A LIST is no value one cluster setting takes,
/// which the engine states as a value it could not parse for that setting.
fn setting_text(name: &str, value: &Value) -> Result<String, Box<RtResponse>> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Array(_) => Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("Failed to parse value for setting [{name}]"),
        ))),
        other => Ok(other.to_string()),
    }
}

/// The settings one scope of an update writes: whether that scope is the transient one, and
/// the settings it names.
type ScopeUpdate = (bool, Vec<SettingUpdate>);

/// The engine's error type for a document whose fields are not the ones a request body
/// declares.
const X_CONTENT_PARSE_EXCEPTION: &str = "x_content_parse_exception";

/// Flatten one scope of a settings body to `(the setting's dotted name, its value)` pairs,
/// `None` for a setting the body resets.
///
/// Iterative in the sense that matters: the recursion is on the body's own nesting, and the
/// bound is on the NAME the walk composes rather than on that nesting — a setting's name is
/// as deep as its DOTS say, however the body spelled them, so one key carrying a hundred of
/// them names a setting exactly as deep as a body nesting a hundred levels does.
/// [`state::INDEX_SCHEMA_MAX_DEPTH`] bounds both here, so a name past it is refused rather
/// than stored: that is what keeps the recursion below, and the expansion the report
/// performs over a stored name ([`reported_settings`]), bounded by a constant instead of by
/// the body a caller sent.
fn flatten_settings(
    written: &Map<String, Value>,
    prefix: &mut String,
    into: &mut Vec<WrittenSetting>,
) -> Result<(), Box<RtResponse>> {
    for (key, value) in written {
        let name = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if name.matches('.').count() > state::INDEX_SCHEMA_MAX_DEPTH {
            return Err(Box::new(error(
                400,
                ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                &format!("setting [{name}] nests deeper than a setting name may"),
            )));
        }
        match value {
            Value::Object(nested) => {
                let mut nested_prefix = name;
                flatten_settings(nested, &mut nested_prefix, into)?;
            }
            Value::Null => into.push((name, None)),
            value => into.push((name, Some(value.clone()))),
        }
    }
    Ok(())
}

/// One scope's settings as a report carries them: EXPANDED into the objects their dotted
/// names name, which is the form the settings API answers in, or flat for a request asking
/// for `flat_settings`.
///
/// A name whose expansion would run THROUGH a setting the scope also holds as a value of its
/// own (`a` beside `a.b`) has no object to nest under, so it is reported under the name it
/// has rather than displacing the other: both settings the cluster holds are reported, which
/// is what a client reading its own configuration back needs. So is a name carrying more
/// segments than a setting name may ([`state::INDEX_SCHEMA_MAX_DEPTH`], the bound the write
/// path holds a name to): the expansion a report performs is therefore nested by a constant
/// at most, whatever name the store was found holding.
fn reported_settings<'a>(
    held: impl IntoIterator<Item = (&'a str, Value)>,
    request: &Request<'_>,
) -> Value {
    let flat = request.parameter("flat_settings").as_deref() == Some("true");
    let mut reported = Map::new();
    for (name, value) in held {
        let segments: Vec<&str> = name.split('.').collect();
        if flat || segments.len() == 1 || segments.len() > state::INDEX_SCHEMA_MAX_DEPTH {
            reported.insert(name.to_string(), value);
            continue;
        }
        let mut at = Some(&mut reported);
        for segment in &segments[..segments.len() - 1] {
            at = at.and_then(|held| {
                held.entry((*segment).to_string())
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
            });
        }
        match at {
            Some(at) => {
                at.insert(segments[segments.len() - 1].to_string(), value);
            }
            None => {
                reported.insert(name.to_string(), value);
            }
        }
    }
    Value::Object(reported)
}

/// One settings scope as a report carries it, for a scope the cluster holds as TEXT
/// ([`reported_settings`]).
fn reported_text_settings(held: &BTreeMap<String, String>, request: &Request<'_>) -> Value {
    reported_settings(
        held.iter().map(|(name, value)| (name.as_str(), json!(value))),
        request,
    )
}

/// The pipelines a `GET /_ingest/pipeline` reports: every one the cluster holds, or the ONE
/// the path names.
fn pipeline_view(domain: &Domain, id: Option<&str>) -> Value {
    let mut reported = Map::new();
    for pipeline in domain.ingest_pipelines.values() {
        if id.is_some_and(|named| named != pipeline.id) {
            continue;
        }
        reported.insert(pipeline.id.clone(), pipeline.definition.clone());
    }
    Value::Object(reported)
}

/// A `PUT`, `GET` or `DELETE` on ONE ingest pipeline.
fn pipeline_request(domain: &mut Domain, request: &Request<'_>, id: &str) -> RtResponse {
    match request.method {
        "PUT" => guarded(domain, request, fgac::ACTION_PUT_PIPELINE, |domain| {
            write_pipeline(domain, request, id)
        }),
        "GET" => guarded(domain, request, fgac::ACTION_GET_PIPELINE, |domain| {
            if !domain.ingest_pipelines.contains_key(id) {
                return pipeline_missing(id);
            }
            ok(&pipeline_view(domain, Some(id)))
        }),
        "DELETE" => guarded(domain, request, fgac::ACTION_DELETE_PIPELINE, |domain| {
            match domain.ingest_pipelines.remove(id) {
                Some(_) => ok(&json!({ "acknowledged": true })),
                None => pipeline_missing(id),
            }
        }),
        _ => wrong_method(request, "PUT, GET, DELETE"),
    }
}

/// Declare the pipeline the request names.
///
/// The declaration is READ before it is stored ([`pipeline::parse`]): a pipeline naming a
/// processor type this engine does not register, or a processor missing an option its type
/// requires, is refused with the engine's own sentence rather than stored as a pipeline that
/// would do less than it says.
fn write_pipeline(domain: &mut Domain, request: &Request<'_>, id: &str) -> RtResponse {
    let definition = match request.json() {
        Ok(Some(Value::Object(members))) => Value::Object(members),
        Ok(_) => {
            return error(
                400,
                PARSE_EXCEPTION,
                "failed to parse: expected the body to be a JSON object declaring the pipeline",
            )
        }
        Err(refusal) => return *refusal,
    };
    if let Err(failed) = pipeline::parse(&definition) {
        return error(400, failed.error_type, &failed.reason);
    }
    if !domain.ingest_pipelines.contains_key(id)
        && domain.ingest_pipelines.len() >= state::INGEST_PIPELINE_QUOTA
    {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "cluster [{}] already holds the maximum of {} ingest pipelines, so pipeline \
                 [{id}] could not be created",
                cluster_name(domain),
                state::INGEST_PIPELINE_QUOTA
            ),
        );
    }
    domain.ingest_pipelines.insert(
        id.to_string(),
        state::IngestPipeline {
            id: id.to_string(),
            definition,
        },
    );
    ok(&json!({ "acknowledged": true }))
}

/// The engine's refusal of a pipeline id the cluster holds no pipeline for.
fn pipeline_missing(id: &str) -> RtResponse {
    error(
        404,
        RESOURCE_NOT_FOUND_EXCEPTION,
        &format!("pipeline [{id}] is missing"),
    )
}

/// The engine's error type for a resource the cluster does not hold.
const RESOURCE_NOT_FOUND_EXCEPTION: &str = "resource_not_found_exception";

/// Run the pipeline the path names over the documents the body carries, WITHOUT storing
/// anything: `POST /_ingest/pipeline/{id}/_simulate`.
///
/// The processors are the ones the stored pipeline declares and are run by the same code a
/// write runs them with, so what this answers is what a write through that pipeline would
/// store. `verbose` reports each processor's own result in turn, which is the same walk with
/// every step published instead of only the last.
fn simulate_pipeline(
    domain: &Domain,
    request: &Request<'_>,
    id: &str,
    now: SystemTime,
) -> RtResponse {
    let Some(pipeline) = domain.ingest_pipelines.get(id) else {
        return pipeline_missing(id);
    };
    let processors = match pipeline::parse(&pipeline.definition) {
        Ok(processors) => processors,
        Err(failed) => return error(400, failed.error_type, &failed.reason),
    };
    let body = match request.json() {
        Ok(Some(Value::Object(members))) => Value::Object(members),
        Ok(_) => {
            return error(
                400,
                PARSE_EXCEPTION,
                "failed to parse: expected the body to be a JSON object carrying docs",
            )
        }
        Err(refusal) => return *refusal,
    };
    let Some(docs) = body.get("docs").and_then(Value::as_array) else {
        return error(400, PARSE_EXCEPTION, "[docs] required property is missing");
    };
    if docs.len() > MAX_SIMULATED_DOCUMENTS {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "a simulate request carries at most {MAX_SIMULATED_DOCUMENTS} documents"
            ),
        );
    }
    let verbose = request.parameter("verbose").is_some_and(|value| value != "false");
    let timestamp = state::datetime_text(now);
    // What one simulate run may PRODUCE, in bytes, not only in documents and processors:
    // a processor's declared value is written into every document it runs over, so the
    // answer's size is the product of the counts and that value — three caller-chosen
    // factors, of which the counts alone are no bound. Every value the answer retains is
    // charged against this budget and a run that would exceed it is refused, so a small
    // request cannot make a large answer.
    let mut budget = MAX_SIMULATED_BYTES;
    let mut answered = Vec::new();
    for doc in docs {
        let index = doc
            .get("_index")
            .and_then(Value::as_str)
            .unwrap_or("_index")
            .to_string();
        let document_id = doc
            .get("_id")
            .and_then(Value::as_str)
            .unwrap_or("_id")
            .to_string();
        let mut source = doc.get("_source").cloned().unwrap_or_else(|| json!({}));
        if !source.is_object() {
            return error(
                400,
                PARSE_EXCEPTION,
                "failed to parse: expected [_source] to be a JSON object",
            );
        }
        let mut steps = Vec::new();
        let mut failed = None;
        for processor in &processors {
            match processor.apply(&mut source) {
                Ok(()) => {
                    // The document a processor LEFT is bounded by what an index takes, the
                    // same bound the write path applies to a pipelined document.
                    if rendered_bytes(&source) > ingest::MAX_DOCUMENT_BYTES {
                        return oversized_simulation();
                    }
                    // A step is retained only for a run that asked for the steps: a
                    // non-verbose run answers the last document, not one copy per
                    // processor.
                    if verbose {
                        let step = json!({
                            "processor_type": processor.kind(),
                            "status": "success",
                            "doc": simulated(&index, &document_id, &source, &timestamp),
                        });
                        match budget.checked_sub(rendered_bytes(&step)) {
                            Some(left) => budget = left,
                            None => return oversized_simulation(),
                        }
                        steps.push(step);
                    }
                }
                Err(refusal) => {
                    failed = Some(json!({
                        "type": refusal.error_type,
                        "reason": refusal.reason,
                    }));
                    break;
                }
            }
        }
        let answer = match failed {
            Some(failure) => json!({ "error": failure }),
            None if verbose => json!({ "processor_results": steps }),
            None => json!({ "doc": simulated(&index, &document_id, &source, &timestamp) }),
        };
        // A verbose run has already charged its steps; charging the answer again would
        // double-count them, so only what the answer adds is charged.
        let charged = if verbose { 0 } else { rendered_bytes(&answer) };
        match budget.checked_sub(charged) {
            Some(left) => budget = left,
            None => return oversized_simulation(),
        }
        answered.push(answer);
    }
    ok(&json!({ "docs": answered }))
}

/// The most bytes ONE simulate run may produce.
///
/// The emulator's OWN bound, and the bound that matters for this route: the document count,
/// the processor count and the bytes a processor injects are all caller-chosen, so the
/// counts alone leave the answer's size unbounded. The cap is the one a request body is
/// read under ([`MAX_REQUEST_BODY_BYTES`]), so a simulate answers at most what this cluster
/// would take in.
const MAX_SIMULATED_BYTES: usize = MAX_REQUEST_BODY_BYTES;

/// How many bytes a value occupies on the wire — what [`MAX_SIMULATED_BYTES`] is charged
/// against. A value that cannot be rendered is charged the whole budget, so it cannot be
/// retained for free.
fn rendered_bytes(value: &Value) -> usize {
    ::serde_json::to_vec(value).map_or(usize::MAX, |rendered| rendered.len())
}

/// The engine's refusal of a simulate run whose documents are larger than this cluster
/// takes.
fn oversized_simulation() -> RtResponse {
    error(
        400,
        ingest::ILLEGAL_ARGUMENT_EXCEPTION,
        &format!(
            "a simulate run answers at most {MAX_SIMULATED_BYTES} bytes over documents of at \
             most {} bytes each",
            ingest::MAX_DOCUMENT_BYTES
        ),
    )
}

/// One simulated document, as the simulate API reports it.
fn simulated(index: &str, id: &str, source: &Value, timestamp: &str) -> Value {
    json!({
        "_index": index,
        "_id": id,
        "_source": source,
        "_ingest": { "timestamp": timestamp },
    })
}

/// The number an index's `settings` section gives `name`, under the `index` sub-section or
/// directly, as both spellings a client writes it with.
fn setting_number(schema: &::aws_smithy_types::Document, name: &str) -> Option<i64> {
    let settings = state::json_from_document(schema);
    let section = settings.get("settings")?;
    let value = section
        .get("index")
        .and_then(|nested| nested.get(name))
        .or_else(|| section.get(name))?;
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

/// The setting naming how many primary shards an index has, and the one naming how many
/// replica copies of each — the two the shard arithmetic reads ([`setting_number`]), named
/// once so no reader of the cluster's allocation spells them for itself.
const NUMBER_OF_SHARDS: &str = "number_of_shards";

/// See [`NUMBER_OF_SHARDS`].
const NUMBER_OF_REPLICAS: &str = "number_of_replicas";

/// A `PUT`, `GET`, `HEAD` or `DELETE` on an index itself.
fn index_request(domain: &mut Domain, request: &Request<'_>, index_name: &str) -> RtResponse {
    match request.method {
        "PUT" => guarded(domain, request, fgac::ACTION_CREATE_INDEX, |domain| {
            create_index(domain, request, index_name)
        }),
        "GET" | "HEAD" => guarded(domain, request, fgac::ACTION_GET_INDEX, |domain| {
            read_index(domain, request, index_name)
        }),
        "DELETE" => guarded(domain, request, fgac::ACTION_DELETE_INDEX, |domain| {
            match domain.indexes.remove(index_name) {
                Some(_) => {
                    // Which indexes the cluster holds is what its own application log
                    // states, so the delete that changed it is recorded.
                    request.record_served(
                        domain,
                        &[state::log_publishing::Served::Applied {
                            index: index_name,
                            change: state::log_publishing::IndexChange::Deleted,
                        }],
                    );
                    ok(&json!({ "acknowledged": true }))
                }
                None => index_not_found(index_name),
            }
        }),
        _ => wrong_method(request, "PUT, GET, HEAD, DELETE"),
    }
}

/// Create the index the request names, carrying the schema its body gives and the one a
/// matching index template gives beneath it.
fn create_index(domain: &mut Domain, request: &Request<'_>, index_name: &str) -> RtResponse {
    if !state::is_index_name(index_name) {
        return indexed_error(
            400,
            INVALID_INDEX_NAME_EXCEPTION,
            &format!("Invalid index name [{index_name}]"),
            index_name,
        );
    }
    if domain.indexes.contains_key(index_name) {
        return indexed_error(
            400,
            RESOURCE_ALREADY_EXISTS_EXCEPTION,
            &format!(
                "index [{index_name}/{}] already exists",
                stable_uuid(&format!("{}/{index_name}", domain.arn))
            ),
            index_name,
        );
    }
    if domain.indexes.len() >= state::INDEX_QUOTA {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "domain [{}] already holds the maximum of {} indexes, so index \
                 [{index_name}] could not be created",
                domain.domain_name,
                state::INDEX_QUOTA
            ),
        );
    }
    let requested = match request.json() {
        Ok(Some(Value::Object(members))) => Value::Object(members),
        Ok(None) => json!({}),
        Ok(_) => {
            return error(
                400,
                PARSE_EXCEPTION,
                "failed to parse: expected the body to be a JSON object",
            )
        }
        Err(refusal) => return *refusal,
    };
    // "An index template is a way to tell OpenSearch how to configure an index when it is
    // created": a matching template's schema is what the index starts from, and what the
    // create itself names is merged onto it, so an explicit section wins over the
    // template's.
    let templated = domain.templated_schema(index_name);
    let schema = match templated {
        Some(base) => state::merged_index_schema(&base, &state::document_from_json(&requested)),
        None => state::document_from_json(&requested),
    };
    let depth = state::index_schema_depth(&schema);
    if depth > state::INDEX_SCHEMA_MAX_DEPTH {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("index schema nests {depth} levels, past the maximum this cluster takes"),
        );
    }
    let values = state::index_schema_values(&schema);
    if values > state::INDEX_SCHEMA_QUOTA {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("index schema holds {values} values, past the maximum this cluster takes"),
        );
    }
    // The cluster builds the index's ANALYSIS from the settings it is created with, so a
    // defined analyzer naming a tokenizer, a token filter, a character filter or a built-in
    // analyzer nothing registers cannot be assembled: the create is refused with the
    // engine's own sentence for what could not be resolved, by the same rule the control
    // plane's `CreateIndex` applies to the settings it is handed
    // ([`state::analysis::IndexAnalysis::unbuildable_analyzer`]), so neither path stores an
    // index whose chain the cluster could not build. The dictionary FILES a chain may name
    // are resolved where the index is created through the control plane; a chain that reads
    // one is judged here on the names alone.
    if let Some((_, unresolved)) = state::analysis::IndexAnalysis::of(&schema)
        .unbuildable_analyzer(&state::analysis::PackageFiles::default())
    {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &unresolved.message(false),
        );
    }
    domain.indexes.insert(
        index_name.to_string(),
        state::Index {
            index_name: index_name.to_string(),
            schema,
            documents: BTreeMap::new(),
            // "aliases: the aliases the index is created with": an alias the create
            // declares points at the index from the moment it exists, exactly as one the
            // alias-actions API adds later does.
            aliases: aliases::declared(requested.get("aliases")),
            next_seq_no: 0,
        },
    );
    // An index the cluster did not hold before is a change to the cluster's own state,
    // which its application log states — with the shard layout the created index carries,
    // as the engine's own line carries it.
    let (shards, replicas) = domain.indexes.get(index_name).map_or((1, 1), |index| {
        (
            setting_number(&index.schema, "number_of_shards").unwrap_or(1),
            setting_number(&index.schema, "number_of_replicas").unwrap_or(1),
        )
    });
    request.record_served(
        domain,
        &[state::log_publishing::Served::Applied {
            index: index_name,
            change: state::log_publishing::IndexChange::Created { shards, replicas },
        }],
    );
    ok(&json!({
        "acknowledged": true,
        "shards_acknowledged": true,
        "index": index_name,
    }))
}

/// An index's own settings and mappings, as a `GET /{index}` reports them.
fn read_index(domain: &Domain, request: &Request<'_>, index_name: &str) -> RtResponse {
    let Some(index) = domain.indexes.get(index_name) else {
        return match request.method {
            "HEAD" => status_only(404),
            _ => index_not_found(index_name),
        };
    };
    if request.method == "HEAD" {
        return status_only(200);
    }
    let schema = state::json_from_document(&index.schema);
    let mappings = schema.get("mappings").cloned().unwrap_or_else(|| json!({}));
    ok(&json!({
        index_name: {
            "aliases": aliases::reported(index),
            "mappings": mappings,
            // The index's settings as its own settings API reports them, so two reads of
            // one index cannot report its configuration differently.
            "settings": {
                state::SETTINGS_INDEX_SECTION:
                    index_settings::settings_view(domain, index, request, None),
            },
        }
    }))
}

/// A `GET`, `PUT`, `POST` or `DELETE` on one document addressed by its id.
fn document_request(
    domain: &mut Domain,
    request: &Request<'_>,
    index_name: &str,
    id: &str,
    creates: Creates,
    now: SystemTime,
    files: &state::analysis::PackageFiles,
) -> RtResponse {
    match request.method {
        "PUT" | "POST" => document_write(domain, request, index_name, Some(id), creates, now),
        "GET" => guarded(domain, request, fgac::ACTION_GET_DOCUMENT, |domain| {
            // A read addressed to an alias reads the index that alias names.
            let held = read_index_name(domain, index_name, id);
            read_document(
                domain,
                request,
                held.as_deref().unwrap_or(index_name),
                id,
                files,
            )
        }),
        "DELETE" => guarded(domain, request, fgac::ACTION_DELETE_DOCUMENT, |domain| {
            let index_name = read_index_name(domain, index_name, id)
                .unwrap_or_else(|| index_name.to_string());
            let index_name = index_name.as_str();
            // A delete is a write like any other: it is held to the same conditional
            // compare-and-set, and it takes a sequence number of its own, which is what
            // lets a client tell a delete it made from one someone else made.
            if let Some(refusal) = write_conflict(
                domain,
                index_name,
                Some(id),
                Creates::Upsert,
                requested_revision(request),
            ) {
                return refusal.response();
            }
            let copies = write_shards(domain, index_name);
            let Some(index) = domain.indexes.get_mut(index_name) else {
                return index_not_found(index_name);
            };
            let removed = index.documents.remove(id);
            let version = removed
                .as_ref()
                .map_or(1, |document| document.version.saturating_add(1));
            let seq_no = index.take_seq_no();
            let result = if removed.is_some() {
                "deleted"
            } else {
                "not_found"
            };
            let status = if removed.is_some() { 200 } else { 404 };
            rendered(
                status,
                &json!({
                    "_index": index_name,
                    "_id": id,
                    "_version": version,
                    "result": result,
                    "_shards": copies,
                    "_seq_no": seq_no,
                    "_primary_term": ingest::PRIMARY_TERM,
                }),
            )
        }),
        _ => wrong_method(request, "GET, PUT, POST, DELETE"),
    }
}

/// One document, as a `GET /{index}/_doc/{id}` reports it.
///
/// The `_seq_no`/`_primary_term` pair is reported beside the version because it is what a
/// client feeds back as `if_seq_no`/`if_primary_term` to change the document only while it
/// is still the one that was read.
///
/// What the caller may SEE of it is the narrowing its own roles impose ([`visible_to`]): a
/// document the document-level security in force does not admit is reported exactly as one
/// the index does not hold, and the fields the field-level security hides are not in the
/// source — a realtime read of a document is narrowed the way a search over it is.
fn read_document(
    domain: &Domain,
    request: &Request<'_>,
    index_name: &str,
    id: &str,
    files: &state::analysis::PackageFiles,
) -> RtResponse {
    let Some(index) = domain.indexes.get(index_name) else {
        return index_not_found(index_name);
    };
    let missing = || {
        rendered(
            404,
            &json!({ "_index": index_name, "_id": id, "found": false }),
        )
    };
    let Some(document) = index.documents.get(id) else {
        return missing();
    };
    let visibility = visible_to(domain, request, index_name);
    if !admits_document(index, &visibility, files, id, &document.source) {
        return missing();
    }
    ok(&json!({
        "_index": index_name,
        "_id": id,
        "_version": document.version,
        "_seq_no": document.seq_no,
        "_primary_term": document.primary_term,
        "found": true,
        "_source": visibility.visible(&document.source),
    }))
}

/// What the principal this request speaks for may SEE of the index `index`: the document-
/// and field-level narrowing its own roles impose.
///
/// Narrowed by nothing — the open visibility — for a domain without fine-grained access
/// control and for the master user, which is the principal the cluster grants everything.
fn visible_to(
    domain: &Domain,
    request: &Request<'_>,
    index: &str,
) -> fgac_permissions::Visibility {
    if !fgac::enforced(domain) || fgac::is_master(domain, request.principal) {
        return fgac_permissions::Visibility::default();
    }
    fgac_permissions::visibility(domain, &fgac::mapped_roles(domain, request.principal), index)
}

/// The ONE selector the document-level security queries in force compose: a document is
/// admitted when ANY of them selects it, which is how a principal holding two narrowed
/// roles sees the union of what each lets it see.
///
/// `None` where no query is in force, which is every request but one a role narrowed. A
/// query this surface cannot READ states a narrowing that cannot be evaluated, and a
/// narrowing that cannot be evaluated admits nothing rather than everything — the empty
/// `ids` clause, which no document's id is one of.
fn dls_selector(visibility: &fgac_permissions::Visibility) -> Option<Selector> {
    if visibility.documents.is_empty() {
        return None;
    }
    let clauses = visibility
        .documents
        .iter()
        .map(|document| {
            // The member travels as the query itself, which is the form the plugin's own
            // `dls` carries, and a document that wraps it in `query` names the same thing.
            let query = document.get("query").unwrap_or(document);
            selector(Some(query), 0).unwrap_or_else(|_| Selector::Ids(Vec::new()))
        })
        .collect();
    Some(Selector::Any(clauses))
}

/// Whether the document-level security in force admits the document `id`/`source` of
/// `index` — the composed query, evaluated through that index's own analysis chains exactly
/// as a search evaluates a query over it.
///
/// `true` where no query is in force. A chain the index names that this engine cannot
/// build leaves the narrowing unevaluated, and an unevaluated narrowing admits nothing.
fn admits_document(
    index: &state::Index,
    visibility: &fgac_permissions::Visibility,
    files: &state::analysis::PackageFiles,
    id: &str,
    source: &Value,
) -> bool {
    let Some(select) = dls_selector(visibility) else {
        return true;
    };
    let mut clauses = BTreeSet::new();
    select.analyzed_clauses(&mut clauses);
    let budget = ::std::cell::Cell::new(0_usize);
    let Ok(terms) = Terms::of(
        state::analysis::IndexAnalysis::of(&index.schema),
        files,
        &clauses,
        &BTreeSet::new(),
        &budget,
    ) else {
        return false;
    };
    select.matches(id, source, &terms)
}

/// Write one document into the index the request names, through the ONE data-plane write
/// every producer into a domain of this service goes through.
fn document_write(
    domain: &mut Domain,
    request: &Request<'_>,
    index_name: &str,
    id: Option<&str>,
    creates: Creates,
    now: SystemTime,
) -> RtResponse {
    if !matches!(request.method, "POST" | "PUT") {
        return wrong_method(request, "POST, PUT");
    }
    // A write addressed to an ALIAS lands in the index that alias is the write index of,
    // which is what makes a write alias the address a producer keeps while the index
    // behind it is rotated.
    let index_name = match write_index(domain, index_name) {
        Ok(index_name) => index_name,
        Err(refusal) => return refusal.response(),
    };
    let index_name = index_name.as_str();
    // The two conditions a write is held to before anything is stored: the create-only
    // one `_create` asks for, and the compare-and-set a conditional write performs.
    if let Some(refusal) = write_conflict(
        domain,
        index_name,
        id,
        creates,
        requested_revision(request),
    ) {
        return refusal.response();
    }
    let address = domain.arn.clone();
    // "pipeline: The pipeline ID for preprocessing documents": a write naming one is
    // preprocessed by exactly the processors that pipeline declares, run by the same code
    // the simulate API runs them with, and what is STORED is the document they left. A
    // pipeline the cluster does not hold, or a processor that cannot act on this document,
    // refuses the write rather than storing the document unprocessed.
    let processed = match request.parameter("pipeline") {
        None => None,
        Some(id) => match processed_source(domain, request, &id) {
            Ok(processed) => Some(processed),
            Err(refusal) => return *refusal,
        },
    };
    let write = ingest::DocumentWrite {
        domain: &address,
        index_name,
        document_id: id.filter(|id| !id.is_empty()),
        document: processed.as_deref().unwrap_or(request.body),
        principal_arn: request.principal,
        now,
    };
    // The write goes through the ONE data-plane write path, which owns the auto-created
    // index and the mapping rules — so a document written over the wire and one written by a
    // producer land under one set of cluster rules. The IAM decision about this request was
    // taken at this plane's front door, against the resource the request's own path
    // addresses, and is not taken again here.
    match ingest::index_into(domain, &write) {
        Ok(indexed) => {
            let Some(written) = written_document(domain, &indexed) else {
                return unreadable_write(&indexed).response();
            };
            let created = written.version == 1;
            // The write really happened, and how long it took is what the index's own
            // indexing slow-log threshold is compared against.
            request.record_served(
                domain,
                &[state::log_publishing::Served::Indexed {
                    index: &indexed.index_name,
                    id: &indexed.document_id,
                    took: request.took(),
                    source: ::std::str::from_utf8(request.body).unwrap_or_default(),
                }],
            );
            rendered(
                if created { 201 } else { 200 },
                &json!({
                    "_index": indexed.index_name,
                    "_id": indexed.document_id,
                    "_version": written.version,
                    "result": if created { "created" } else { "updated" },
                    "_shards": write_shards(domain, &indexed.index_name),
                    "_seq_no": written.seq_no,
                    "_primary_term": written.primary_term,
                }),
            )
        }
        // What is left for the write path to refuse is the CLUSTER's own (a principal
        // fine-grained access control binds to no role, or a document the index's mappings
        // do not accept), so it is answered in the engine's envelope, which is right for
        // every one of them. The IAM decision this surface takes is the one at its front
        // door ([`iam_refused`]), where a request is decided once, against the resource its
        // own path addresses.
        Err(refusal) => refused(&refusal),
    }
}

/// The document a write LANDED as, read back out of the index it landed in: the revision
/// the write left it at, which is what the answer reports.
///
/// Read from the store rather than carried out of the write path, because the store is
/// where the document's revision lives: an answer computed from anything else could report
/// a revision the index does not hold. `None` is a store that does not hold what the write
/// path reported storing — a state nothing here can produce — and is answered as the
/// failure it is rather than with an invented revision.
fn written_document(domain: &Domain, indexed: &ingest::Indexed) -> Option<ingest::IndexedDocument> {
    domain
        .indexes
        .get(&indexed.index_name)
        .and_then(|index| index.documents.get(&indexed.document_id))
        .cloned()
}

/// The refusal for a write the store took and a read back could not find: nothing can be
/// reported about a document the cluster does not hold, so the failure is stated instead of
/// a revision being made up for it.
fn unreadable_write(indexed: &ingest::Indexed) -> Refusal {
    ::tracing::error!(
        index = %indexed.index_name,
        document = %indexed.document_id,
        "a document the data-plane write path stored could not be read back"
    );
    Refusal {
        status: 500,
        error_type: ILLEGAL_STATE_EXCEPTION.to_string(),
        reason: format!(
            "[{}]: the document was not readable in index [{}] after it was written",
            indexed.document_id, indexed.index_name
        ),
    }
}

/// The engine's error type for a cluster state its own code holds to be impossible.
const ILLEGAL_STATE_EXCEPTION: &str = "illegal_state_exception";

/// The index a write addressed to `named` LANDS in: the index of that name — which a write
/// may create — or the one index an alias of that name is writable through.
///
/// The ONE resolution every write goes through, so a document written to a write alias and
/// one written to the index behind it land in the same place. A name no index carries and no
/// alias points at is the index the write creates, which is what `action.auto_create_index`
/// decides inside the write path itself.
fn write_index(domain: &Domain, named: &str) -> Result<String, Refusal> {
    match domain.write_index(named) {
        Ok(index_name) => Ok(index_name),
        // Nothing of that name exists yet: the write names the index it would create.
        Err(pointed) if pointed.is_empty() => Ok(named.to_string()),
        // Several indexes carry the alias and none of them is its write index, so the
        // write has no one place to land.
        Err(_) => Err(Refusal::of_illegal_argument(&format!(
            "no write index is defined for alias [{named}]. The write index may be explicitly \
             disabled using is_write_index=false or the alias points to multiple indices \
             without one being designated as a write index"
        ))),
    }
}

/// The index a READ of one document addressed to `named` reads: the index of that name, or
/// the index an alias of that name points at that holds the document — the first of them
/// in name order where several do, since a read of an alias reads the indexes it names.
fn read_index_name(domain: &Domain, named: &str, id: &str) -> Option<String> {
    let resolved = domain.resolved_indexes(named);
    resolved
        .iter()
        .find(|name| {
            domain
                .indexes
                .get(*name)
                .is_some_and(|index| index.documents.contains_key(id))
        })
        .or_else(|| resolved.iter().next())
        .cloned()
}

/// The document bytes a write naming an ingest PIPELINE stores: the request's own source
/// with every processor that pipeline declares applied, in the order it declares them.
fn processed_source(
    domain: &Domain,
    request: &Request<'_>,
    id: &str,
) -> Result<Vec<u8>, Box<RtResponse>> {
    let mut source = match request.json() {
        Ok(Some(source)) if source.is_object() => source,
        Ok(_) => {
            return Err(Box::new(error(
                400,
                ingest::MAPPER_PARSING_EXCEPTION,
                "failed to parse: expected a document to be a JSON object",
            )))
        }
        Err(refusal) => return Err(refusal),
    };
    apply_pipeline(domain, id, &mut source)
        .map_err(|refusal| Box::new(refusal.response()))?;
    ::serde_json::to_vec(&source).map_err(|failed| {
        Box::new(error(
            400,
            ingest::MAPPER_PARSING_EXCEPTION,
            &format!("failed to render the processed document: {failed}"),
        ))
    })
}

/// The `_shards` summary a write answers with, computed from the index's own replica
/// setting and the domain's node count: every copy the cluster asked for is `total`, and
/// the ones a node can hold — the primary, plus the replicas the other nodes can carry —
/// are `successful`.
fn write_shards(domain: &Domain, index_name: &str) -> Value {
    let copies = domain
        .indexes
        .get(index_name)
        .and_then(|index| setting_number(&index.schema, NUMBER_OF_REPLICAS))
        .unwrap_or(1)
        .clamp(0, MAX_SHARDS_PER_INDEX);
    let assigned = assigned_replicas(copies, data_nodes(domain));
    json!({
        "total": copies.saturating_add(1),
        "successful": assigned.saturating_add(1),
        "failed": 0,
    })
}

/// What a `_refresh` answers: the shards the refresh ran on, over the indexes it was asked
/// about — every index the domain holds, or the ONE the path names.
///
/// A refresh "makes recent operations performed on one or more indices available for
/// search": it flushes what is already written, so it changes nothing about a document and
/// reports no document. Every write this emulator takes is visible to the next read the
/// moment it lands, so what is left to answer is the shard summary, computed from each
/// index's own settings and the nodes the domain has — the same arithmetic the cluster
/// health and a write's `_shards` are computed with.
fn refresh(domain: &Domain, request: &Request<'_>, index_name: Option<&str>) -> RtResponse {
    let named: Vec<&state::Index> = match index_name {
        None => domain.indexes.values().collect(),
        Some(name) => {
            let resolved = domain.resolved_indexes(name);
            if resolved.is_empty() {
                return index_not_found(name);
            }
            resolved
                .iter()
                .filter_map(|name| domain.indexes.get(name))
                .collect()
        }
    };
    let _ = request;
    let nodes = data_nodes(domain);
    let mut total = 0_i64;
    let mut successful = 0_i64;
    for index in named {
        // Both come from the index's own settings, which a caller wrote, so both are
        // clamped before they are multiplied out.
        let shards = setting_number(&index.schema, NUMBER_OF_SHARDS)
            .unwrap_or(1)
            .clamp(1, MAX_SHARDS_PER_INDEX);
        let copies = setting_number(&index.schema, NUMBER_OF_REPLICAS)
            .unwrap_or(1)
            .clamp(0, MAX_SHARDS_PER_INDEX);
        // A replica copy needs a node other than the one holding its primary, so only the
        // copies the cluster's other nodes can carry are refreshed.
        let assigned = assigned_replicas(copies, nodes);
        total = total.saturating_add(shards.saturating_mul(copies.saturating_add(1)));
        successful = successful.saturating_add(shards.saturating_mul(assigned.saturating_add(1)));
    }
    ok(&json!({
        "_shards": { "total": total, "successful": successful, "failed": 0 },
    }))
}

/// The count a `_count` answers, over the documents the selected indexes hold.
fn count(
    domain: &Domain,
    request: &Request<'_>,
    index_name: &str,
    across: &CrossCluster,
    files: &state::analysis::PackageFiles,
) -> RtResponse {
    // A count reports HOW MANY documents the query selected and nothing about them, so none
    // of the sections a search answers its hits with belongs to one: the order, the
    // fragments — and the summaries, which a `_count` body does not take at all, so one
    // carrying them is refused rather than answered with the section dropped.
    match request.json() {
        Ok(Some(body)) => {
            if let Some(named) = ["aggs", "aggregations"]
                .into_iter()
                .find(|member| body.get(*member).is_some())
            {
                return error(
                    400,
                    PARSING_EXCEPTION,
                    &format!("request does not support [{named}]"),
                );
            }
        }
        Ok(None) => {}
        Err(refusal) => return *refusal,
    }
    let asked = Answering {
        from: 0,
        size: 0,
        sorting: &search::Sorting::default(),
        highlighting: &search::Highlighting::default(),
    };
    let (total, _, _) = match selected_documents(domain, request, index_name, across, files, &asked)
    {
        Ok(selected) => selected,
        Err(refusal) => return *refusal,
    };
    record_searched(domain, request, index_name);
    ok(&json!({
        "count": total,
        "_shards": { "total": 1, "successful": 1, "skipped": 0, "failed": 0 },
    }))
}

/// The hits a `_search` answers, over the documents the selected indexes hold — this
/// domain's own, and those of a remote cluster the request named ([`CrossCluster`]).
fn search(
    domain: &Domain,
    request: &Request<'_>,
    index_name: &str,
    across: &CrossCluster,
    files: &state::analysis::PackageFiles,
) -> RtResponse {
    let body = match request.json() {
        Ok(body) => body.unwrap_or_else(|| json!({})),
        Err(refusal) => return *refusal,
    };
    let from = usize::try_from(numbered(&body, "from", request, 0)).unwrap_or(0);
    let size = usize::try_from(numbered(
        &body,
        "size",
        request,
        i64::try_from(DEFAULT_SEARCH_SIZE).unwrap_or(10),
    ))
    .unwrap_or(DEFAULT_SEARCH_SIZE);
    // The engine's own bound on how much one search may answer with, which is also what
    // bounds the hits this request materialises: past it the search is refused rather than
    // served.
    if from.saturating_add(size) > MAX_RESULT_WINDOW {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "Result window is too large, from + size must be less than or equal to: \
                 [{MAX_RESULT_WINDOW}] but was [{}].",
                from.saturating_add(size)
            ),
        );
    }
    // Both sections that decide HOW the hits are answered are read before anything is
    // scanned, so a request this surface cannot read either of them from is refused rather
    // than answered as if it had named neither.
    let sorting = match search::Sorting::of(&body, request) {
        Ok(sorting) => sorting,
        Err(refusal) => return *refusal,
    };
    let highlighting = match search::Highlighting::of(&body) {
        Ok(highlighting) => highlighting,
        Err(refusal) => return *refusal,
    };
    let asked = Answering {
        from,
        size,
        sorting: &sorting,
        highlighting: &highlighting,
    };
    let (total, hits, aggregated) =
        match selected_documents(domain, request, index_name, across, files, &asked) {
            Ok(selected) => selected,
            Err(refusal) => return *refusal,
        };
    record_searched(domain, request, index_name);
    let sorted = !sorting.is_empty();
    let hits: Vec<Value> = hits
        .into_iter()
        .map(|selection| {
            let (index, id, source) = selection.hit;
            let mut hit = Map::new();
            hit.insert("_index".to_string(), json!(index));
            hit.insert("_id".to_string(), json!(id));
            hit.insert("_score".to_string(), json!(search::MATCH_SCORE));
            hit.insert("_source".to_string(), source);
            // The values the order was taken on are answered beside the hit, which is what
            // a client pages with; a request naming no order has none to answer.
            if sorted {
                hit.insert(
                    search::SORT_MEMBER.to_string(),
                    Value::Array(selection.sort),
                );
            }
            if let Some(highlight) = selection.highlight {
                hit.insert(search::HIGHLIGHT_MEMBER.to_string(), highlight);
            }
            Value::Object(hit)
        })
        .collect();
    let mut answer = Map::new();
    answer.insert("timed_out".to_string(), json!(false));
    answer.insert(
        "_shards".to_string(),
        json!({ "total": 1, "successful": 1, "skipped": 0, "failed": 0 }),
    );
    answer.insert(
        "hits".to_string(),
        json!({
            "total": { "value": total, "relation": "eq" },
            // The greatest score among the hits the request is ANSWERED with, which a
            // request that asked for no hit at all — one aggregating over the documents
            // rather than reading them — has none of.
            "max_score": if total == 0 || size == 0 {
                Value::Null
            } else {
                json!(search::MATCH_SCORE)
            },
            "hits": hits,
        }),
    );
    // The summaries the request asked OVER those documents, reported beside them; a request
    // that asked for none carries no such section.
    if !aggregated.is_empty() {
        answer.insert("aggregations".to_string(), aggregated.reported());
    }
    ok(&Value::Object(answer))
}

/// Note the read a search or a count performed, for the domain's search slow-log
/// publishing: ONE record per index the path named, because the threshold that decides
/// whether the read was slow is each index's own.
fn record_searched(domain: &Domain, request: &Request<'_>, named: &str) {
    let took = request.took();
    let source = ::std::str::from_utf8(request.body).unwrap_or_default();
    let served: Vec<state::log_publishing::Served<'_>> = resolved_indexes(domain, Some(named))
        .unwrap_or_default()
        .into_iter()
        .map(|index| state::log_publishing::Served::Searched {
            index,
            took,
            source,
        })
        .collect();
    request.record_served(domain, &served);
}

/// The number a search names for `name`, in the body or in the query string, falling back
/// to `default`.
fn numbered(body: &Value, name: &str, request: &Request<'_>, default: i64) -> i64 {
    body.get(name)
        .and_then(Value::as_i64)
        .or_else(|| request.parameter(name).and_then(|text| text.parse().ok()))
        .unwrap_or(default)
        .max(0)
}

/// How a search asks for its answer beside the query that selects the documents: the page it
/// wants, the order it wants that page taken in, and the fragments it wants on each hit.
///
/// A `_count` asks for none of them — it reports how many documents the query selected and
/// materialises no hit at all — so it is the empty one of these.
struct Answering<'a> {
    from: usize,
    size: usize,
    sorting: &'a search::Sorting,
    highlighting: &'a search::Highlighting,
}

/// How many documents the request selects, and the ones on the page it asked for — every
/// document of the indexes its path names that the query it carries matches, in the order
/// the request's `sort` puts them in, or in index then id order where it names none.
///
/// Three bounds, so one request's work is bounded whatever its path says: the number of
/// comma-separated patterns it may name ([`MAX_NAMED_INDEX_PATTERNS`]); the resolved index
/// set is DE-DUPLICATED, so repeating a pattern cannot multiply the scan or double-count a
/// document; and the documents visited are capped at the whole store once
/// ([`MAX_DOCUMENTS_VISITED`]). Only the page is MATERIALISED — with a `sort`, the
/// candidates for it, which [`Scan`] keeps bounded by `from + size` — so what one request
/// allocates is bounded by the page it asked for and not by the documents the domain holds.
fn selected_documents(
    domain: &Domain,
    request: &Request<'_>,
    index_name: &str,
    across: &CrossCluster,
    files: &state::analysis::PackageFiles,
    asked: &Answering<'_>,
) -> Result<Selected, Box<RtResponse>> {
    let Answering {
        from,
        size,
        sorting,
        ..
    } = *asked;
    let body = request.json()?.unwrap_or_else(|| json!({}));
    // The query a request states in its BODY, or — where it states none there — the one its
    // QUERY STRING states, which is the URI form of a search ([`query_string`]).
    let select = match body.get("query") {
        Some(query) => selector(Some(query), 0)?,
        None => match query_string::requested(request)? {
            Some(select) => select,
            None => selector(None, 0)?,
        },
    };
    let patterns = named_patterns(index_name)?;
    // A `BTreeSet` of names, so an index named twice is scanned once and the order is the
    // store's own: two reads of one domain report the same documents in the same order.
    let mut named: BTreeSet<&str> = BTreeSet::new();
    // The remote patterns, de-duplicated the same way, and resolved against what the
    // remote clusters answered rather than against this domain's own indexes.
    let mut remote = BTreeSet::new();
    for pattern in patterns {
        if let Some((alias, _)) = pattern.split_once(REMOTE_CLUSTER_SEPARATOR) {
            if across.unknown.contains(alias) {
                return Err(Box::new(no_such_remote_cluster(alias)));
            }
            if !across.resolved.contains_key(pattern) {
                return Err(Box::new(index_not_found(pattern)));
            }
            remote.insert(pattern);
            continue;
        }
        named.extend(matched_indexes(domain, pattern)?);
    }
    // A clause naming a field PATTERN — a `multi_match` over one, a query-string clause that
    // names no field — asks its question of the fields the NAMED INDEXES map, because which
    // fields such a pattern names is a property of those mappings and not of the query. The
    // expansion is charged against the engine's own clause ceiling AS IT RUNS
    // ([`Selector::expanded`]) and the tree it produces is counted against that ceiling again
    // below, so neither a pattern nor the mappings it meets can multiply what one request
    // evaluates per document.
    let mut breadth = MAX_QUERY_CLAUSES;
    let select = select.expanded(&mapped_fields(domain, &named), &mut breadth)?;
    // A query's BREADTH is bounded as its depth is: past the engine's own clause ceiling
    // the query is refused rather than evaluated clause by clause against every document,
    // so the work one request causes is bounded by this constant and not by the size of its
    // body. EVERY clause counts, analyzed or not, selective or not: each is re-evaluated per
    // document ([`Selector::clauses`]), and the reading itself is bounded by the same
    // ceiling ([`selector`]).
    if select.clauses() > MAX_QUERY_CLAUSES {
        return Err(too_many_clauses());
    }
    // The clauses a `match` asks an ANALYZED question with — the field each names and the
    // text it asks about — so each index resolves its chains and analyzes each query text
    // ONCE, before its documents are scanned, rather than per document.
    let mut analyzed = BTreeSet::new();
    select.analyzed_clauses(&mut analyzed);
    let mut scan = Scan {
        from,
        size,
        visited: 0,
        total: 0,
        page: Vec::new(),
        sorting,
        // The summaries the request asked for, read before anything is scanned for the
        // reason the two sections above are: a section this surface cannot read is refused
        // rather than answered as if the request had named none.
        aggregating: aggregations::Aggregations::of(&body)?,
    };
    // The analysis budget the WHOLE scan shares, held here rather than on the per-index
    // chains so the indexes a request names cannot each spend it over again.
    let budget = ::std::cell::Cell::new(0_usize);
    // This domain's own documents first, then every remote cluster's, each in index then
    // id order — so one query over a local and a remote index reports a stable sequence.
    // Each index's documents are judged by THAT index's own analysis chain, which is what
    // its own settings and mappings compose.
    for name in &named {
        let Some(index) = domain.indexes.get(*name) else {
            continue;
        };
        let analysis = state::analysis::IndexAnalysis::of(&index.schema);
        // An order can only be taken on values the index holds per document, so a key
        // naming a field it holds none for is refused — per shard, as the engine refuses
        // it, since it is that index's own mappings that decide it.
        if let Some(field) = sorting.without_doc_values(&analysis) {
            return Err(Box::new(all_shards_failed(
                &index.index_name,
                ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                &fielddata_disabled(field),
            )));
        }
        // Every aggregation is taken on those same per-document values, so one naming a
        // field the index holds none for is refused the same way rather than answered with
        // no buckets — an empty bucket list says the selected documents hold no such value,
        // which is not what a field that could not be read at all means.
        if let Some(field) = scan.aggregating.without_doc_values(&analysis) {
            return Err(Box::new(all_shards_failed(
                &index.index_name,
                ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                &fielddata_disabled(field),
            )));
        }
        // A bound the field it is compared against reads no value of its type from states no
        // limit at all, so the request is refused rather than answered from a comparison
        // nothing could take — per shard, for the same reason the order above is.
        if let Some(bound) = select.unreadable_bound(&analysis) {
            return Err(Box::new(all_shards_failed(
                &index.index_name,
                PARSE_EXCEPTION,
                &unreadable_date(&bound),
            )));
        }
        // What the caller may SEE of THIS index, which is its own roles' narrowing of it:
        // the documents the document-level security admits are the only ones the scan
        // considers, and the fields the field-level security hides are not in the document
        // the scan is handed at all — so such a field matches no clause of the query and
        // appears in no hit, which is the one effect hiding a field has.
        let visibility = visible_to(domain, request, &index.index_name);
        let reading = Reading::of(
            analysis,
            files,
            &analyzed,
            &select,
            asked,
            &budget,
            dls_selector(&visibility),
        )?;
        for document in index.documents.values() {
            let visible = (!visibility.fields.is_empty())
                .then(|| visibility.visible(&document.source));
            scan.consider(
                &select,
                &index.index_name,
                &document.id,
                visible.as_ref().unwrap_or(&document.source),
                &reading,
            )?;
        }
    }
    // A remote cluster's documents come with no mappings this domain holds, so their text
    // is analyzed with the default chain — which is what an index with no analysis settings
    // of its own uses.
    let remote_reading = Reading::of(
        state::analysis::IndexAnalysis::default(),
        files,
        &analyzed,
        &select,
        asked,
        &budget,
        None,
    )?;
    for (index_name, id, source) in remote
        .into_iter()
        .filter_map(|pattern| across.resolved.get(pattern))
        .flatten()
    {
        scan.consider(&select, index_name, id, source, &remote_reading)?;
    }
    // Taken off the scan before its page is materialised, because the summaries are over
    // every document the query selected rather than over the page.
    let aggregated = ::std::mem::take(&mut scan.aggregating);
    let total = scan.total;
    Ok((total, scan.page(), aggregated))
}

/// What ONE index's documents are read against: the chains and mappings its own settings
/// compose ([`Terms`]), the fields the request reads fragments off it, and what the query
/// asks of each of those fields.
///
/// Resolved once per index, because every document of that index is asked the same questions
/// through the same chains.
struct Reading<'r> {
    terms: Terms<'r>,
    highlighted: search::Highlighted<'r>,
    wanted: Wanted,
    /// The document-level security in force on this index, as the ONE selector its
    /// queries compose ([`dls_selector`]): a document it does not select is not the
    /// caller's to see, so the scan counts it in no total and puts it on no page. `None`
    /// where the caller's roles narrow this index's documents not at all.
    admits: Option<Selector>,
}

impl<'r> Reading<'r> {
    /// What the index whose mappings are `analysis` is read against.
    ///
    /// `admits` is the document-level narrowing in force on that index: its own clauses are
    /// analyzed through the index's chains alongside the request's, because the narrowing is
    /// asked of every document the scan reaches exactly as the query is.
    fn of(
        analysis: state::analysis::IndexAnalysis,
        files: &state::analysis::PackageFiles,
        analyzed: &BTreeSet<(String, String)>,
        select: &Selector,
        asked: &'r Answering<'r>,
        budget: &'r ::std::cell::Cell<usize>,
        admits: Option<Selector>,
    ) -> Result<Self, Box<RtResponse>> {
        let highlighted = asked.highlighting.of_index(&analysis);
        let mut clauses = analyzed.clone();
        if let Some(admits) = &admits {
            admits.analyzed_clauses(&mut clauses);
        }
        let terms = Terms::of(analysis, files, &clauses, &highlighted.fields(), budget)?;
        let mut wanted = Wanted::new();
        if !asked.highlighting.is_empty() {
            select.wanted(&terms, &mut wanted);
        }
        Ok(Self {
            terms,
            highlighted,
            wanted,
            admits,
        })
    }
}

/// The sentence the engine refuses a value a date field reads no instant from with, naming
/// the value and the format set it was read against.
fn unreadable_date(value: &str) -> String {
    format!(
        "failed to parse date field [{value}] with format [{}]",
        state::date_values::DEFAULT_FORMATS
    )
}

/// The fields the indexes `named` MAP, which is the set a field pattern a query carries
/// expands against ([`Selector::expanded`]).
///
/// The union over the indexes the request reads, de-duplicated, and bounded by
/// [`MAX_MATCHED_FIELDS`]: a pattern is asked of every field it names of every document the
/// scan reaches, so the breadth it can reach is a constant rather than the mapping document a
/// caller wrote times the indexes a path names. A field one index of the set maps and another
/// does not is simply held by no document of that other index, so asking it there selects
/// nothing.
fn mapped_fields(domain: &Domain, named: &BTreeSet<&str>) -> Vec<String> {
    let mut fields: BTreeSet<String> = BTreeSet::new();
    for index in named.iter().filter_map(|name| domain.indexes.get(*name)) {
        if fields.len() >= MAX_MATCHED_FIELDS {
            break;
        }
        fields.extend(state::analysis::IndexAnalysis::of(&index.schema).mapped_fields());
    }
    fields.into_iter().take(MAX_MATCHED_FIELDS).collect()
}

/// The sentence the engine refuses a sort or an aggregation on an analyzed text field with:
/// such a field holds only the terms of its analysis, and ordering or bucketing documents by
/// it needs a value per document, which its mapping has to switch on.
fn fielddata_disabled(field: &str) -> String {
    format!(
        "Text fields are not optimised for operations that require per-document field data \
         like aggregations and sorting, so these operations are disabled by default. Please use \
         a keyword field instead. Alternatively, set fielddata=true on [{field}] in order to \
         load field data by uninverting the inverted index. Note that this can use significant \
         memory."
    )
}

/// The refusal a search whose shards all failed answers: the engine runs a query per shard,
/// so a condition one index's own mappings decide is reported as every shard of it having
/// failed, with that condition as the cause rather than as the failure itself.
fn all_shards_failed(index_name: &str, error_type: &str, reason: &str) -> RtResponse {
    let cause = json!({ "type": error_type, "reason": reason });
    rendered(
        400,
        &json!({
            "error": {
                "root_cause": [cause.clone()],
                "type": SEARCH_PHASE_EXECUTION_EXCEPTION,
                "reason": "all shards failed",
                "phase": "query",
                "grouped": true,
                "failed_shards": [{
                    "shard": 0,
                    "index": index_name,
                    "reason": cause.clone(),
                }],
                "caused_by": cause,
            },
            "status": 400,
        }),
    )
}

/// The engine's error type for a search no shard could answer.
const SEARCH_PHASE_EXECUTION_EXCEPTION: &str = "search_phase_execution_exception";

/// One scan of the selected indexes: how many documents matched, and the page the request
/// asked for.
///
/// Only the page is MATERIALISED — with an order, the candidates for it, which is twice that
/// page at most — so what one request allocates is bounded by the `from + size` it asked for
/// and not by the documents it scans; the documents visited are bounded by
/// [`MAX_DOCUMENTS_VISITED`] over the whole scan, local and remote together, and the document
/// text ANALYZED by [`MAX_ANALYZED_BYTES`] — which is what bounds the product of the two
/// factors a search multiplies, the documents it scans and the analyzed clauses it carries.
struct Scan<'s> {
    from: usize,
    size: usize,
    visited: usize,
    total: usize,
    /// The page, or — where the request named an order — the candidates for it, which
    /// [`Self::trimmed`] keeps bounded by `from + size`.
    page: Vec<Selection>,
    /// The order the request asked for, which decides whether a selected document is kept
    /// where the scan reached it or ranked against the ones reached before it.
    sorting: &'s search::Sorting,
    /// The summaries the request asked OVER the documents it selects, which every selected
    /// document is read into — the page it asked for is not what they are computed from.
    aggregating: aggregations::Aggregations,
}

impl Scan<'_> {
    /// Count one document, and keep it where it belongs on the page the request asked for.
    fn consider(
        &mut self,
        select: &Selector,
        index_name: &str,
        id: &str,
        source: &Value,
        reading: &Reading<'_>,
    ) -> Result<(), Box<RtResponse>> {
        let terms = &reading.terms;
        self.visited = self.visited.saturating_add(1);
        if self.visited > MAX_DOCUMENTS_VISITED {
            return Err(Box::new(error(
                400,
                ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                &format!("a request may visit at most {MAX_DOCUMENTS_VISITED} documents"),
            )));
        }
        let selected = select.matches(id, source, terms)
            // The document-level security in force is asked of the document too, and a
            // document it does not admit is not selected at all: it is counted in no total,
            // reported on no page and read into no aggregation, which is what makes it
            // indistinguishable from one the index does not hold.
            && reading
                .admits
                .as_ref()
                .is_none_or(|admits| admits.matches(id, source, terms));
        // The budget the scan's analyses share is CHARGED and CHECKED where each analysis
        // happens ([`charged`]), so nothing past it is ever analyzed; this is where a scan
        // that reached it is refused, rather than answering from a document pass that
        // stopped halfway through its clauses.
        if terms.analyzed() > MAX_ANALYZED_BYTES {
            return Err(Box::new(error(
                400,
                ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                &format!("a request may analyze at most {MAX_ANALYZED_BYTES} bytes of text"),
            )));
        }
        if !selected {
            return Ok(());
        }
        let ordinal = self.total;
        self.total = self.total.saturating_add(1);
        // Every selected document is read into the summaries the request asked for, whether
        // or not it lands on the page: an aggregation is over the documents the query
        // matched.
        self.aggregating.consider(terms, source)?;
        let limit = self.from.saturating_add(self.size);
        // Without an order the page is the window of the scan's own sequence, so a document
        // outside it is counted and dropped; with one, every selected document is a
        // candidate for the page until the order says it is not.
        if self.sorting.is_empty() {
            if ordinal < self.from || self.page.len() >= self.size {
                return Ok(());
            }
        } else if limit == 0 {
            return Ok(());
        }
        // The fragments are built where the document is REACHED, because they are read
        // through the chains of the index being scanned, and each document's text is
        // therefore analyzed for them at most once — charged, like every other analysis of
        // this scan, against the budget it shares.
        let highlight = reading
            .highlighted
            .fragments(terms, &reading.wanted, source);
        self.page.push(Selection {
            hit: (index_name.to_string(), id.to_string(), source.clone()),
            sort: self.sorting.values(terms, source, ordinal),
            highlight,
        });
        // Twice the page is what the candidates are allowed to grow to before they are
        // ranked and cut back to it, so the ordering costs one sort per page's worth of
        // candidates rather than one per document, and what is held is a constant times the
        // page the request asked for.
        if !self.sorting.is_empty() && self.page.len() >= limit.saturating_mul(2) {
            self.trimmed(limit);
        }
        Ok(())
    }

    /// Rank the candidates and keep the best `limit` of them.
    ///
    /// A STABLE sort, so documents whose keys compare equal keep the order the scan reached
    /// them in — index then id — and a page of them is the same page on every read.
    fn trimmed(&mut self, limit: usize) {
        let Self { sorting, page, .. } = self;
        page.sort_by(|one, other| sorting.compare(&one.sort, &other.sort));
        page.truncate(limit);
    }

    /// The page this scan answers with: the window the request asked for, taken from the
    /// ranked candidates where it named an order.
    fn page(mut self) -> Vec<Selection> {
        if self.sorting.is_empty() {
            return self.page;
        }
        let limit = self.from.saturating_add(self.size);
        self.trimmed(limit);
        self.page.drain(..self.from.min(self.page.len()));
        self.page
    }
}

/// How the index being scanned turns a field's text into TERMS — the chain a `match` clause
/// on that field is evaluated over, for the query's text and the document's alike.
///
/// Resolved once per index, and each CHAIN built once per index whatever the clauses: a field
/// names its analyzer, and many fields commonly name one analyzer, so what is kept per field
/// is that NAME and the chains are held beside them under it. That is what keeps the work of
/// one request the number of clauses it carries rather than their product with the
/// dictionaries those chains read — a chain carries its whole stop-word or synonym set.
///
/// The QUERY's own text is analyzed here too, once per clause, since a clause asks the same
/// question of every document; a DOCUMENT's text is analyzed as the scan reaches it. BOTH
/// are charged against the budget the WHOLE scan shares ([`MAX_ANALYZED_BYTES`]) — which is
/// why that budget is borrowed from the scan rather than held per index: the chains are
/// resolved once per index the request names, so without one shared allowance a request
/// naming every index would analyze its own clause texts over again per index.
///
/// The two chains a mapping may name are kept apart: the one a query's text goes through and
/// the one the field's own text went through. A field the index holds its values WHOLE for (a
/// keyword, a number, a date) has no entry, because a `match` on such a field is not analyzed
/// at all. An analyzer the index names that this engine cannot BUILD is not replaced by
/// another chain: the request is refused with what could not be built, exactly as the
/// analyze API refuses it.
struct Terms<'a> {
    /// What the index being scanned MAPS, which is what decides where a field takes its
    /// values from ([`state::analysis::IndexAnalysis::source_field`]) and whether they are
    /// analyzed at all.
    analysis: state::analysis::IndexAnalysis,
    /// The field -> the name of the chain the DOCUMENT's text went through.
    indexing: BTreeMap<String, String>,
    /// The chains those names stand for, each built once.
    chains: BTreeMap<String, state::analysis::Analyzer>,
    /// The field -> the text each clause asks about -> the terms that text produces
    /// through the chain a QUERY's text goes through for that field.
    queries: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    /// How much of the scan's analysis budget has been spent, on query text and on document
    /// text alike ([`MAX_ANALYZED_BYTES`]) — the scan's own counter, shared by every index
    /// it reaches.
    spent: &'a ::std::cell::Cell<usize>,
}

impl<'a> Terms<'a> {
    /// The chains the index whose schema is `schema` resolves for `clauses`, and the terms
    /// each clause's own text produces through them.
    ///
    /// Refused past [`MAX_CHAINS_PER_INDEX`] distinct chains: each carries its own
    /// dictionary, so that bound is what keeps the memory one query's resolution costs a
    /// product of constants rather than a number the query chooses. Refused too past the
    /// scan's own analysis budget ([`charged`]), which the clause texts analyzed here draw
    /// down along with the document text the scan goes on to analyze: the chains are
    /// resolved once per index the request names, so the clause texts of a request naming
    /// many indexes are analyzed once per index and that work is bounded here. Refused too
    /// for a chain this engine cannot build — a tokenizer or filter nothing registers, or a
    /// configuration it cannot run — since answering from another chain's terms would be
    /// answering a question the caller did not ask.
    fn of(
        analysis: state::analysis::IndexAnalysis,
        files: &state::analysis::PackageFiles,
        clauses: &BTreeSet<(String, String)>,
        highlighted: &BTreeSet<String>,
        spent: &'a ::std::cell::Cell<usize>,
    ) -> Result<Self, Box<RtResponse>> {
        let mut searching: BTreeMap<String, String> = BTreeMap::new();
        let mut indexing = BTreeMap::new();
        let mut chains: BTreeMap<String, state::analysis::Analyzer> = BTreeMap::new();
        let mut queries: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
        // A field a HIGHLIGHT reads fragments off is analyzed as the index indexed it, so
        // its indexing chain is resolved here beside the clauses' — under the same bounds,
        // since it is the same work.
        let resolved: Vec<(&String, Option<&String>)> = clauses
            .iter()
            .map(|(field, text)| (field, Some(text)))
            .chain(highlighted.iter().map(|field| (field, None)))
            .collect();
        for (field, text) in resolved {
            if analysis.holds_whole_values(field) {
                continue;
            }
            for (search, into) in [(true, &mut searching), (false, &mut indexing)] {
                let named = match analysis.field_analyzer_name(field, search) {
                    Ok(named) => named,
                    // A field the mappings do not map is analyzed by the index's own
                    // default chain, exactly as the text field a write would dynamically
                    // map is.
                    Err(state::analysis::Unresolved::Field(_)) => {
                        state::analysis::DEFAULT_ANALYZER.to_string()
                    }
                    Err(unresolved) => return Err(unresolved_chain(&unresolved)),
                };
                if !chains.contains_key(&named) {
                    if chains.len() >= MAX_CHAINS_PER_INDEX {
                        return Err(Box::new(error(
                            400,
                            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                            &format!(
                                "a request may analyze through at most \
                                 {MAX_CHAINS_PER_INDEX} analyzers of one index"
                            ),
                        )));
                    }
                    let chain = analysis
                        .analyzer(&named, files)
                        .map_err(|unresolved| unresolved_chain(&unresolved))?;
                    chains.insert(named.clone(), chain);
                }
                into.insert(field.clone(), named);
            }
            let (Some(text), Some(chain)) = (
                text,
                searching.get(field).and_then(|named| chains.get(named)),
            ) else {
                continue;
            };
            // The clause's own text costs what analyzing it costs, charged against the
            // one allowance the whole scan shares BEFORE the analysis is performed.
            charged(spent, text)?;
            queries
                .entry(field.clone())
                .or_default()
                .insert(text.clone(), chain.terms(text));
        }
        Ok(Self {
            analysis,
            indexing,
            chains,
            queries,
            spent,
        })
    }

    /// Whether the index holds `field`'s values WHOLE rather than as the terms of an
    /// analysis — which is what decides how a clause, a sort key and a fragment all read it.
    fn holds_whole_values(&self, field: &str) -> bool {
        self.analysis.holds_whole_values(field)
    }

    /// The TYPE the index maps `field` as, which is what decides how a clause COMPARES its
    /// values — an instant as an instant, a number as a number — rather than how it analyzes
    /// them.
    fn mapped_type(&self, field: &str) -> Option<&str> {
        self.analysis.mapped_type(field)
    }

    /// The path in a document's source the field `field` takes its values from: its own,
    /// or — for a MULTI-FIELD — the field it is another indexing of.
    fn source_field(&self, field: &str) -> String {
        self.analysis.source_field(field)
    }

    /// The values the document `source` holds for `field`, as scalars.
    ///
    /// Addressed through the index's own mappings, so a MULTI-FIELD reads the values of the
    /// field above it — a write stores nothing at a multi-field's own path, because a
    /// multi-field is another analysis of its parent's value.
    fn held(&self, source: &Value, field: &str) -> Vec<Value> {
        held_values(source, &self.source_field(field))
    }

    /// Those values as the TEXTS a term is compared against.
    fn texts(&self, source: &Value, field: &str) -> Vec<String> {
        self.held(source, field).iter().map(scalar_text).collect()
    }

    /// The terms the QUERY's text produces for `field`, analyzed once when this index's
    /// chains were resolved; `None` where that field holds its values whole.
    fn query(&self, field: &str, text: &str) -> Option<&[String]> {
        self.queries.get(field)?.get(text).map(Vec::as_slice)
    }

    /// The terms a DOCUMENT's text produces for `field`, or `None` where that field holds
    /// its values whole.
    ///
    /// The scan's analysis budget is charged AND CHECKED here, where the work is done: an
    /// analysis that would take the scan past [`MAX_ANALYZED_BYTES`] is not performed at
    /// all, and the scan is marked exhausted so [`Scan::consider`] refuses the request
    /// instead of answering from a document pass that stopped halfway. Nothing past the
    /// budget is analyzed, however many clauses one document's pass carries.
    fn document(&self, field: &str, text: &str) -> Option<Vec<String>> {
        Some(
            self.analyzed_tokens(field, text)?
                .into_iter()
                .map(|token| token.token)
                .collect(),
        )
    }

    /// The TOKENS a DOCUMENT's text produces for `field` — each with the span of the text it
    /// came from, which is what a fragment wraps — or `None` where that field holds its
    /// values whole.
    ///
    /// Charged against the same budget, in the same way, as the terms a clause compares: it
    /// is the same analysis.
    fn analyzed_tokens(&self, field: &str, text: &str) -> Option<Vec<state::analysis::Token>> {
        let chain = self.chains.get(self.indexing.get(field)?)?;
        if charged(self.spent, text).is_err() {
            return Some(Vec::new());
        }
        Some(chain.analyze(text))
    }

    /// How much of this scan's budget the text analyzed so far has cost.
    fn analyzed(&self) -> usize {
        self.spent.get()
    }
}

/// Charge analyzing `text` against the budget the whole scan shares, and refuse the request
/// where that takes it past [`MAX_ANALYZED_BYTES`] — the ONE statement of what one analysis
/// costs, so the query's text and a document's are charged alike.
///
/// The charge is taken BEFORE the analysis it pays for, so nothing past the budget is ever
/// analyzed. A document's analysis cannot refuse from where it stands ([`Terms::document`]
/// answers no terms and leaves the scan marked exhausted, which [`Scan::consider`] refuses);
/// the query's can, since no document has been judged yet.
fn charged(spent: &::std::cell::Cell<usize>, text: &str) -> Result<(), Box<RtResponse>> {
    let cost = ANALYSIS_OVERHEAD.saturating_add(text.len());
    let total = spent.get().saturating_add(cost);
    spent.set(total);
    if total > MAX_ANALYZED_BYTES {
        return Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("a request may analyze at most {MAX_ANALYZED_BYTES} bytes of text"),
        )));
    }
    Ok(())
}

/// The refusal a chain this engine cannot build earns: the engine's illegal-argument
/// failure, stating what it could not find or could not run.
///
/// The SAME sentence the analyze API answers, through the same rendering
/// ([`state::analysis::Unresolved::message`]), so a search and an analyze cannot word one
/// condition differently. Worded as an index's — a search always resolves a chain against
/// the index whose documents it reads.
fn unresolved_chain(unresolved: &state::analysis::Unresolved) -> Box<RtResponse> {
    Box::new(error(
        400,
        ingest::ILLEGAL_ARGUMENT_EXCEPTION,
        &unresolved.message(false),
    ))
}

/// A `PUT`, `GET` or `DELETE` on an index template, in the composable
/// (`_index_template`) or the legacy (`_template`) form.
fn template_request(
    domain: &mut Domain,
    request: &Request<'_>,
    name: &str,
    legacy: bool,
) -> RtResponse {
    match request.method {
        "PUT" | "POST" => guarded(domain, request, fgac::ACTION_PUT_TEMPLATE, |domain| {
            write_template(domain, request, name, legacy)
        }),
        "GET" => guarded(domain, request, fgac::ACTION_GET_TEMPLATE, |domain| {
            match domain.index_templates.get(name) {
                Some(template) if legacy => ok(&json!({ name: template.definition })),
                Some(template) => ok(&json!({
                    "index_templates": [
                        { "name": name, "index_template": template.definition }
                    ]
                })),
                None => error(
                    404,
                    ingest::INDEX_NOT_FOUND_EXCEPTION,
                    &format!("index_template [{name}] missing"),
                ),
            }
        }),
        "DELETE" => guarded(domain, request, fgac::ACTION_PUT_TEMPLATE, |domain| {
            match domain.index_templates.remove(name) {
                Some(_) => ok(&json!({ "acknowledged": true })),
                None => error(
                    404,
                    ingest::INDEX_NOT_FOUND_EXCEPTION,
                    &format!("index_template [{name}] missing"),
                ),
            }
        }),
        _ => wrong_method(request, "PUT, POST, GET, DELETE"),
    }
}

/// Write the template the request carries: the patterns it applies to, its priority, and
/// the schema an index it applies to is created with.
fn write_template(
    domain: &mut Domain,
    request: &Request<'_>,
    name: &str,
    legacy: bool,
) -> RtResponse {
    let definition = match request.json() {
        Ok(Some(Value::Object(members))) => Value::Object(members),
        Ok(_) => {
            return error(
                400,
                PARSE_EXCEPTION,
                "failed to parse: expected the body to be a JSON object",
            )
        }
        Err(refusal) => return *refusal,
    };
    // "index_patterns is a required field": a template that applies to no index name is
    // refused rather than stored. The list and its entries are the CALLER's, so both are
    // read under the bounds that keep deciding the overlap below a bounded piece of work
    // ([`template_patterns`]).
    let index_patterns = match template_patterns::bounded_names(&definition, "index_patterns") {
        Ok(index_patterns) => index_patterns,
        Err(refusal) => return *refusal,
    };
    if index_patterns.is_empty() {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            "index_patterns is missing or empty",
        );
    }
    if !domain.index_templates.contains_key(name) && domain.index_templates.len() >= TEMPLATE_QUOTA
    {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "domain [{}] already holds the maximum of {TEMPLATE_QUOTA} index templates",
                domain.domain_name
            ),
        );
    }
    // The composable form nests the index configuration under `template`; the legacy form
    // carries it at the top level. "Of two matching templates the one with the higher
    // priority is applied", and the legacy form spells that number `order`.
    let configuration = if legacy {
        definition.clone()
    } else {
        definition.get("template").cloned().unwrap_or_else(|| json!({}))
    };
    let mut schema = Map::new();
    for section in ["mappings", "settings"] {
        if let Some(value) = configuration.get(section) {
            schema.insert(section.to_string(), value.clone());
        }
    }
    let priority = definition
        .get(if legacy { "order" } else { "priority" })
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // "Multiple index templates may not match during index creation": two COMPOSABLE
    // templates whose patterns overlap at the same priority leave the cluster unable to
    // say which one an index would be created from, so the write is refused rather than
    // stored. A legacy template composes with its peers instead of displacing them, which
    // is why the rule is the composable form's.
    if !legacy {
        if let Some(refusal) =
            template_patterns::equal_priority_overlap(domain, name, &index_patterns, priority)
        {
            return refusal;
        }
    }
    // "composed_of: an ordered list of component template names": the components an index
    // created from this template carries, resolved where the index is minted. Held to the
    // same bounds as the patterns above, for the same reason.
    let composed_of = match template_patterns::bounded_names(&definition, "composed_of") {
        Ok(composed_of) => composed_of,
        Err(refusal) => return *refusal,
    };
    if let Some(missing) = composed_of
        .iter()
        .find(|named| !domain.component_templates.contains_key(*named))
    {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("index template [{name}] specifies component templates [{missing}] that do not exist"),
        );
    }
    let schema = state::document_from_json(&Value::Object(schema));
    let values = state::index_schema_values(&schema);
    if values > state::INDEX_SCHEMA_QUOTA {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("index schema holds {values} values, past the maximum this cluster takes"),
        );
    }
    domain.index_templates.insert(
        name.to_string(),
        IndexTemplate {
            name: name.to_string(),
            index_patterns,
            priority,
            composed_of,
            schema,
            definition,
        },
    );
    ok(&json!({ "acknowledged": true }))
}

/// The answer AWS's own IAM layer refuses a signed data-plane request with, BEFORE the
/// cluster is asked anything: `403` carrying a `Message` member — the envelope every AWS
/// service refuses an unauthorized request in, and not one of the engine's error documents,
/// because the engine never saw the request. The sentence is the one
/// [`ingest::iam_decision`] words: the principal that signed, the action its method asks
/// for, and which of IAM's rules refused; every value in it comes from the request and from
/// the stored policies.
///
/// The CLUSTER's own refusal of a request IAM allowed — a principal fine-grained access
/// control binds to no role — is the engine's `security_exception` document instead
/// ([`refused`]), because that one IS the cluster answering.
fn iam_refused(refusal: &ingest::WriteRefusal) -> RtResponse {
    match refusal {
        ingest::WriteRefusal::Rejected {
            error_type: ingest::SECURITY_EXCEPTION,
            message,
        } => rendered(403, &json!({ "Message": message })),
        // Every other condition is the cluster's or the domain's own, worded as the engine
        // words it: this decision states only the authorization one.
        other => refused(other),
    }
}

/// The answer a write refused by the one data-plane write path renders: the engine's own
/// error type decides the status, since the refusal already carries which condition held.
fn refused(refusal: &ingest::WriteRefusal) -> RtResponse {
    match refusal {
        ingest::WriteRefusal::Unavailable { message } => {
            error(503, "cluster_block_exception", message)
        }
        ingest::WriteRefusal::Rejected {
            error_type,
            message,
        } => {
            let status = match *error_type {
                ingest::SECURITY_EXCEPTION => 403,
                // An index the cluster does not hold is a missing resource, not a malformed
                // request, so it carries the status the engine answers a missing index with.
                ingest::INDEX_NOT_FOUND_EXCEPTION => 404,
                _ => 400,
            };
            error(status, error_type, message)
        }
    }
}

/// The refusal the engine answers a path it has no handler registered for.
fn no_handler(request: &Request<'_>) -> RtResponse {
    rendered(
        400,
        &json!({
            "error": format!(
                "no handler found for uri [{}] and method [{}]",
                request.path, request.method
            )
        }),
    )
}

/// The refusal the engine answers a path it DOES serve, reached with a method that path
/// does not take: a different class of failure from a path it serves nothing for, and
/// worded as the engine words it, naming the methods that are allowed.
fn wrong_method(request: &Request<'_>, allowed: &str) -> RtResponse {
    rendered(
        405,
        &json!({
            "error": format!(
                "Incorrect HTTP method for uri [{}] and method [{}], allowed: [{allowed}]",
                request.path, request.method
            )
        }),
    )
}

/// The refusal for an index the domain does not hold.
fn index_not_found(index_name: &str) -> RtResponse {
    indexed_error(
        404,
        ingest::INDEX_NOT_FOUND_EXCEPTION,
        &format!("no such index [{index_name}]"),
        index_name,
    )
}

/// The refusal for a cluster alias the request named that this domain cannot search
/// through: the engine's own answer for a remote cluster it has no connection to.
///
/// 400 rather than 404: the request named a cluster the local cluster does not know, which
/// the engine reports as an argument it cannot act on rather than as a missing resource.
fn no_such_remote_cluster(alias: &str) -> RtResponse {
    error(
        400,
        NO_SUCH_REMOTE_CLUSTER_EXCEPTION,
        &format!("no such remote cluster: [{alias}]"),
    )
}

/// The engine's error type for a cross-cluster request naming a cluster it has no
/// connection to.
const NO_SUCH_REMOTE_CLUSTER_EXCEPTION: &str = "no_such_remote_cluster_exception";

/// An error naming the index it is about, which is what the engine's index errors carry
/// beside the type and the reason.
fn indexed_error(status: u16, error_type: &str, reason: &str, index_name: &str) -> RtResponse {
    let cause = json!({
        "type": error_type,
        "reason": reason,
        "index": index_name,
        "resource.type": "index_or_alias",
        "resource.id": index_name,
    });
    rendered(
        status,
        &json!({
            "error": {
                "root_cause": [cause.clone()],
                "type": error_type,
                "reason": reason,
                "index": index_name,
                "resource.type": "index_or_alias",
                "resource.id": index_name,
            },
            "status": status,
        }),
    )
}

/// The engine's error envelope: the type and the reason, with the status repeated in the
/// body as every OpenSearch error carries it.
fn error(status: u16, error_type: &str, reason: &str) -> RtResponse {
    rendered(
        status,
        &json!({
            "error": {
                "root_cause": [{ "type": error_type, "reason": reason }],
                "type": error_type,
                "reason": reason,
            },
            "status": status,
        }),
    )
}

/// A 200 answer carrying `body`.
fn ok(body: &Value) -> RtResponse {
    rendered(200, body)
}

/// A status with no body at all — what a `HEAD` answers.
fn status_only(status: u16) -> RtResponse {
    RtResponse::new(
        StatusCode::try_from(status).unwrap_or_else(|_| fallback_status()),
        SdkBody::empty(),
    )
}

/// The status a status this emulator could not build falls back to.
fn fallback_status() -> StatusCode {
    StatusCode::try_from(500).expect("500 is a status")
}

/// One rendered JSON answer, as the wire carries it.
fn rendered(status: u16, body: &Value) -> RtResponse {
    let payload = ::serde_json::to_vec(body).unwrap_or_default();
    let mut response = RtResponse::new(
        StatusCode::try_from(status).unwrap_or_else(|_| fallback_status()),
        SdkBody::from(payload),
    );
    if let Err(refused) = response
        .headers_mut()
        .try_insert("content-type", "application/json")
    {
        ::tracing::warn!(
            error = %refused,
            "the data plane's content type cannot be sent on the wire and is left off"
        );
    }
    response
}

#[cfg(test)]
mod tests;
/// WHICH documents a query selects: the clause tree a request's query states, and how that
/// tree is asked of one document of one index.
mod selection;

/// The index-management plugin's policy API, and the defaults the plugin stores a policy
/// under.
mod ism;
use ism::*;

/// The NODES APIs: the cluster's nodes, and the statistics of one.
mod nodes;
use nodes::*;

use selection::*;

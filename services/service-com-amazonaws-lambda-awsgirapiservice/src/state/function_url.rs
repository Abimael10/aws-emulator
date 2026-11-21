// SPDX-License-Identifier: CC0-1.0

//! How a request ADDRESSES a function URL, which stored configuration it names, and which
//! address this service ANNOUNCES for one.
//!
//! `CreateFunctionUrlConfig` hands out "a dedicated HTTP(S) endpoint that you can use to
//! invoke your function", whose documented form [`super::function_url`] mints:
//! `https://<url-id>.lambda-url.<region>.on.aws/`. The first label of that host names no
//! service, so the emulator's front door resolves no target from it and the request would
//! be answered `UnknownService` — for an address this service itself told the caller to
//! use. This module is the one reading of that addressing: it answers which function URL a
//! request names, out of the SAME records `CreateFunctionUrlConfig` persisted, so the URL
//! this service mints and the request it claims back cannot disagree. It is also where the
//! announced address is chosen ([`announced_function_url`]), so a caller is never handed an
//! address no addressing here resolves.
//!
//! THREE addressings resolve, for the reason the shared data-plane reading states
//! ([`service_common::data_plane_addressing`]): a client on the machine this emulator runs
//! on resolves the published host nowhere, so it reaches the emulator's own endpoint and
//! carries in the PATH what it cannot carry in the host.
//!
//! * HOST-STYLE, the addressing `FunctionUrlConfig$FunctionUrl` documents: the host's
//!   first label is the url id and the whole path is the request path.
//! * LOCAL-ENDPOINT style, behind the reserved
//!   [`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`] segment, read on
//!   ANY host because the marker is explicit: `/_aws/lambda-url/<url-id>/<path>`, and the
//!   endpoint HOST spelling `/_aws/<url-id>.lambda-url.<region>.on.aws/<path>`.
//! * PATH-STYLE: the url id is the FIRST PATH SEGMENT and the request path is what follows
//!   it, read only on a host that names NO service (the emulator's own endpoint) or names
//!   THIS one, so a first segment that happens to equal a url id cannot take a request away
//!   from the service whose endpoint the host names.
//!
//! Every one of them resolves only to a configuration this store HOLDS, which is what keeps
//! exactly one service claiming a request: a host or a segment this store knows no function
//! URL for is claimed by nobody here.

use super::*;

/// The label every function URL host carries after its url id, as
/// [`super::function_url`] spells it: `<url-id>.lambda-url.<region>.on.aws`.
pub const FUNCTION_URL_LABEL: &str = FUNCTION_URL_HOST_SUFFIX;

/// The domain a function URL host ends in, after the region label.
const FUNCTION_URL_DOMAIN: &str = "on.aws";

/// The ceiling the model puts on `FunctionUrl`
/// (`smithy_query(query="shape", shape="FunctionUrl")`: `@length(min: 40, max: 100)`), which
/// is what every announced endpoint has to fit inside — see [`announced_function_url`].
const FUNCTION_URL_MAX_LEN: usize = 100;

/// What a request ADDRESSES, read from the two parts of it that carry an address.
///
/// Both are passed in rather than a whole request, because the framework hands
/// `Service::can_handle` the request's PARTS and the serving seams a runtime-API request:
/// each has these two things and no common type. Which of the two carries the address the
/// request was SENT TO is the shared data-plane reading's question, not this module's
/// ([`service_common::data_plane_addressing::addressed_authority`]: the authority of an
/// absolute-form request target when the request carries one, the `Host` header otherwise),
/// so every plane reads an address the same way — and a request another service RELAYED here
/// resolves to the function URL that relay addressed rather than to the host its own client
/// named.
#[derive(Debug, Clone)]
pub struct AddressedFunctionUrl<'r> {
    /// The host the request is addressed to, when it names one.
    pub host: Option<String>,
    /// The request's path.
    pub path: &'r str,
}

impl<'r> AddressedFunctionUrl<'r> {
    /// The reading of the request parts `Service::can_handle` is handed.
    #[must_use]
    pub fn of_parts(parts: &'r ::smithy_server_runtime::http::request::Parts) -> Self {
        Self {
            host: ::service_common::data_plane_addressing::addressed_authority_of(
                parts
                    .uri
                    .authority()
                    .map(::smithy_server_runtime::http::uri::Authority::as_str),
                parts
                    .headers
                    .get("host")
                    .and_then(|host| host.to_str().ok()),
            )
            .map(str::to_string),
            path: parts.uri.path(),
        }
    }

    /// The reading of the runtime-API request the serving seams are handed, which carries
    /// its URI as a string — the absolute form of which carries the authority instead of a
    /// `Host` header, which is the case the shared reading answers.
    #[must_use]
    pub fn of_request(req: &'r ::aws_smithy_runtime_api::http::Request) -> Self {
        Self {
            host: ::service_common::data_plane_addressing::addressed_authority(
                req.uri(),
                req.headers().get("host"),
            )
            .map(str::to_string),
            path: ::service_common::request_path(req.uri()),
        }
    }
}

/// The url id and the region of `host` when `host` is a function URL endpoint this service
/// publishes, or `None` when it is not one.
///
/// The shape is the one [`super::function_url`] mints —
/// `<url-id>.lambda-url.<region>.on.aws` — so a first label, then the
/// [`FUNCTION_URL_LABEL`], then a non-empty region label, then
/// [`FUNCTION_URL_DOMAIN`]. Any userinfo and port are stripped first, because a `Host`
/// header may carry them.
#[must_use]
pub fn function_url_host_parts(host: &str) -> Option<(&str, &str)> {
    let host = ::service_common::data_plane_addressing::host_only(host);
    let (url_id, rest) = host.split_once('.')?;
    if url_id.is_empty() {
        return None;
    }
    let (label, rest) = rest.split_once('.')?;
    if label != FUNCTION_URL_LABEL {
        return None;
    }
    let (region, domain) = rest.split_once('.')?;
    (!region.is_empty() && domain == FUNCTION_URL_DOMAIN).then_some((url_id, region))
}

/// The url id of `host` when `host` is a function URL endpoint this service publishes; see
/// [`function_url_host_parts`].
#[must_use]
pub fn function_url_id_of_host(host: &str) -> Option<&str> {
    function_url_host_parts(host).map(|(url_id, _region)| url_id)
}

/// The function URL ENDPOINT HOST of `url_id` in `region`, as AWS documents the form:
/// `<url-id>.lambda-url.<region>.on.aws`.
///
/// The one spelling of that host — [`super::function_url`] builds the documented URL from
/// it, [`function_url_host_parts`] reads it back, and the data plane reports it as the
/// event's `requestContext.domainName` — so the host this service mints, the host it claims
/// and the host it tells a handler about cannot drift.
#[must_use]
pub fn function_url_endpoint_host(region: &str, url_id: &str) -> String {
    format!("{url_id}.{FUNCTION_URL_LABEL}.{region}.{FUNCTION_URL_DOMAIN}")
}

/// The `FunctionUrl` this service ANNOUNCES for the function URL `url_id`, resolved once
/// when the URL is created and stored on its [`FunctionUrlConfigRecord`], so every later
/// read of the same function URL reports the same endpoint.
///
/// The member exists for a client to FETCH — "the HTTP URL endpoint for your function", "a
/// dedicated HTTP(S) endpoint that you can use to invoke your function" — so the address
/// announced has to be one this emulator itself answers on. `endpoint` is the `host:port` of
/// the LISTENER opened for this URL ([`super::function_url_endpoint`]) when one could be
/// opened, and that address is the one announced: it is the URL's OWN, so a consumer handed
/// this endpoint resolves it from the authority alone and supplies its own path, exactly as
/// it does for the documented host. Which address it is follows the addressings the module
/// doc above states, in the same order the API Gateway V2 plane announces an api's endpoint
/// in:
///
/// * the URL's own listener, when this engine vends one;
/// * a request that addressed the EMULATOR'S OWN endpoint
///   ([`::service_common::data_plane_addressing::reached_emulator_endpoint`]) comes from a
///   client that resolves nothing else, so the URL is announced on that same endpoint with
///   its own id behind the reserved data-plane marker and the label that names this plane
///   (`/_aws/lambda-url/<url-id>/`), the LOCAL-ENDPOINT addressing [`addressed_url`] serves.
///   It is the SHORTER of the two spellings of that addressing, which is what keeps the
///   announced value inside the modelled length however long the authority a client reaches
///   this emulator at is; the endpoint's Region is not needed in the address, because the
///   function a URL resolves to is the one the stored record names and the plane reports
///   that record's own Region as the event's `requestContext.domainName`;
/// * a URL created through any other request on an emulator that IS served on a port
///   ([`::service_common::data_plane_addressing::served_endpoint_authority`]) is announced
///   on that port, the one address this process answers on, so a URL minted by an
///   in-process call of another service announces an address its caller can reach too;
/// * a request that addressed one of AWS's own hosts on an emulator served on no port at
///   all comes from a client that resolves them, so the URL is announced as
///   [`super::function_url()`] mints it — the form `FunctionUrlConfig$FunctionUrl`
///   documents, which this module claims host-style.
///
/// Every form stays inside the model's 40..100 `FunctionUrl` length constraint: the
/// documented URL is 69 characters for a Region label of nine, and the local form is 58
/// characters plus the authority it is served at — which is why the listener's own address
/// carries that same marked path, rather than being announced bare and falling short of the
/// member's own minimum. The authority comes off a caller-supplied `Host`, which a DNS name
/// or a full IPv6 literal may make longer than that leaves room for, so an announced local
/// form past [`FUNCTION_URL_MAX_LEN`] falls back to the documented one rather than answering
/// a value the member's own constraint refuses.
///
/// The scheme is `https` in every form rather than the one the endpoint base carries,
/// because a function URL IS an HTTPS endpoint — `CreateFunctionUrlConfig` hands out "a
/// dedicated HTTP(S) endpoint" and every listener of this process presents the core's
/// certificate, so the scheme is the resource's and not the request's.
#[must_use]
pub fn announced_function_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    url_id: &str,
    endpoint: Option<&str>,
) -> String {
    let addressed = AddressedFunctionUrl::of_parts(ctx.parts());
    let served = endpoint
        .map(str::to_string)
        .or_else(|| {
            ::service_common::data_plane_addressing::reached_emulator_endpoint(
                "",
                addressed.host.as_deref(),
            )
        })
        .or_else(|| {
            ::service_common::data_plane_addressing::served_endpoint_authority(
                ctx.endpoint().as_str(),
            )
        });
    match served {
        Some(authority) => {
            let announced = format!(
                "https://{authority}/{marker}/{FUNCTION_URL_LABEL}/{url_id}/",
                marker = ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE,
            );
            if announced.len() <= FUNCTION_URL_MAX_LEN {
                return announced;
            }
            function_url(region, url_id)
        }
        None => function_url(region, url_id),
    }
}

/// What a request ADDRESSES, read from the address ALONE: which function URL it names and
/// the path within it.
///
/// Pure, so the cheap half of the claim happens before any store guard is taken: a request
/// that addresses no function URL at all — every control-plane call, and every other
/// service's traffic — is answered `None` here and never reaches
/// [`held_function_url`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressedUrl<'r> {
    /// The url id the address names.
    pub url_id: &'r str,
    /// The path within the URL.
    pub request_path: &'r str,
    /// The region the address names, for the addressings that carry one.
    pub region: Option<&'r str>,
}

/// The function URL `addressed` names, read from the address alone, or `None` when it names
/// none.
#[must_use]
pub fn addressed_url<'r>(
    addressed: &'r AddressedFunctionUrl<'r>,
) -> Option<AddressedUrl<'r>> {
    if let Some(named) = named_endpoint(addressed) {
        return Some(named);
    }
    // PATH-STYLE, on the emulator's own endpoint or on this service's own host only: any
    // other host is another service's, and whatever its path carries is that service's to
    // answer.
    if names_control_plane(addressed.path) {
        return None;
    }
    let names_other_service = addressed
        .host
        .as_deref()
        .and_then(::service_common::data_plane_addressing::service_named_by_host)
        .is_some_and(|service| service != crate::ROUTE_KEY);
    if names_other_service {
        return None;
    }
    let (url_id, request_path) = split_first_segment(addressed.path)?;
    named(url_id, request_path, None)
}

/// The function URL `addressed` names by one of the two addressings that name THIS plane and
/// nothing else, or `None` when it names neither.
///
/// The two are the ones a caller cannot write by accident: the endpoint HOST
/// `FunctionUrlConfig$FunctionUrl` documents, whose shape is this plane's own, and the
/// reserved local-endpoint path, whose marker and plane label are explicit. Both name this
/// plane whatever this store HOLDS, which is what lets the plane answer for an address of
/// its own that no configuration sits behind — a URL whose configuration was deleted is
/// still an address of this service, and what a caller reading it is told is this service's
/// to say.
///
/// PATH-STYLE is deliberately not one of them: its first segment is a url id only because
/// this store holds one under it, so reading it for an id this store does not hold would
/// claim every one-segment path on the emulator's own endpoint.
#[must_use]
pub fn named_endpoint<'r>(
    addressed: &'r AddressedFunctionUrl<'r>,
) -> Option<AddressedUrl<'r>> {
    // This service's own CONTROL-plane path space is never a function URL's, on any host: a
    // request whose first segment is one of `crate::ROUTE_PATH_SEGMENTS` is a control-plane
    // call wherever it arrives, and claiming it here would take `/2015-03-31/functions`
    // away from the operations that serve it.
    if names_control_plane(addressed.path) {
        return None;
    }
    // LOCAL-ENDPOINT style first, because its marker is unambiguous on any host.
    if let Some((url_id, request_path, region)) = local_endpoint_path(addressed.path) {
        return named(url_id, request_path, region);
    }
    let (url_id, region) = addressed
        .host
        .as_deref()
        .and_then(function_url_host_parts)?;
    named(url_id, addressed.path, Some(region))
}

/// Whether `path` is in this service's own control-plane path space.
fn names_control_plane(path: &str) -> bool {
    first_segment(path).is_some_and(|first| crate::ROUTE_PATH_SEGMENTS.contains(&first))
}

/// One addressing, once its url id is known to be non-empty — an empty first label names no
/// function URL.
fn named<'r>(
    url_id: &'r str,
    request_path: &'r str,
    region: Option<&'r str>,
) -> Option<AddressedUrl<'r>> {
    (!url_id.is_empty()).then_some(AddressedUrl {
        url_id,
        request_path,
        region,
    })
}

/// The function URL a request names: the configuration it resolved, the function and
/// qualifier that configuration is attached to, and the path WITHIN the URL.
#[derive(Debug, Clone)]
pub struct FunctionUrlTarget {
    /// The url id the request addressed, which is the configuration's own.
    pub url_id: String,
    /// The bare name of the function the URL is attached to.
    pub function_name: String,
    /// The qualifier the URL was created on: [`VERSION_LATEST`] for the unpublished
    /// version, or an alias name.
    pub qualifier: String,
    /// The ARN of the function AT that qualifier — the unqualified function ARN for a URL on
    /// the unpublished version and the `:<qualifier>`-suffixed one otherwise
    /// ([`FunctionRecord::versioned_arn`]), read off the record that HOLDS the URL.
    ///
    /// It is the resource an invocation through this endpoint is authorized on, and the ARN a
    /// permission granted on that qualifier names, so it is carried here rather than rebuilt
    /// from the address: a client reaching the emulator's own endpoint names no Region at all,
    /// while the function it reaches has one.
    pub resource_arn: String,
    /// The `AuthType` the URL enforces, as stored.
    pub auth_type: String,
    /// The `InvokeMode` the URL SERVES IN, which is the one configured on it or the
    /// documented default when none ever was
    /// ([`FunctionUrlConfigRecord::served_invoke_mode`]).
    pub invoke_mode: String,
    /// The `Cors` settings the URL was configured with, as stored, and absent when it was
    /// configured with none — which is what decides whether this endpoint answers a
    /// browser's preflight itself or hands it to the function
    /// ([`crate::service::function_url`]).
    pub cors: Option<Cors>,
    /// The path within the URL, which is the `rawPath` of the request the handler is given.
    pub request_path: String,
    /// The region the ADDRESS named, for the addressings that carry one.
    pub addressed_region: Option<String>,
    /// The HOST of the address this URL was PUBLISHED at ([`announced_function_url`], as
    /// stored on the configuration), which is the host a client addressed it by, without the
    /// port that address is served on — so it is the host the handler is told about, in the
    /// event's `headers.host` and `requestContext.domainName`, both of which name a DOMAIN
    /// and not a socket ([`announced_host`]).
    pub endpoint_host: String,
}

/// The function URL PUBLISHED at the authority `addressed` names, or `None` when no
/// configuration this store holds was published there.
///
/// This is the addressing a CONSUMER of a published endpoint uses: it was handed the URL and
/// carries its AUTHORITY, supplying a request path of its own — a CloudFront
/// `Origin$DomainName`, an integration's host, an operator's `curl`. The address it names is
/// the listener opened for one function URL ([`super::function_url_endpoint`]), so the whole
/// request path is the path within that URL.
///
/// Only an authority a configuration was PUBLISHED on resolves, read off the stored record's
/// own `endpoint_authority`: the emulator's shared endpoint is nobody's function URL, so a
/// URL announced there resolves by its path alone and this reading claims no request that
/// arrives at the front door.
#[must_use]
pub fn published_function_url<'r>(
    store: &ComAmazonawsLambdaAwsgirapiserviceState,
    addressed: &'r AddressedFunctionUrl<'r>,
) -> Option<FunctionUrlTarget> {
    let host = addressed.host.as_deref()?;
    let authority = host.rsplit('@').next().unwrap_or(host).to_ascii_lowercase();
    store.functions.values().find_map(|function| {
        function
            .url_configs
            .iter()
            .find(|(_, config)| {
                config
                    .endpoint_authority
                    .as_ref()
                    .is_some_and(|published| published.eq_ignore_ascii_case(&authority))
            })
            .map(|(qualifier, config)| {
                target_of_record(function, qualifier, config, addressed.path, None)
            })
    })
}

/// The stored function URL `addressed` names, or `None` when this store holds none under
/// that id.
///
/// Read out of the stored [`FunctionUrlConfigRecord`]s themselves, so nothing is claimed
/// that this service cannot answer about.
#[must_use]
pub fn held_function_url(
    store: &ComAmazonawsLambdaAwsgirapiserviceState,
    addressed: &AddressedUrl<'_>,
) -> Option<FunctionUrlTarget> {
    store.functions.values().find_map(|function| {
        function
            .url_configs
            .iter()
            .find(|(_, config)| config.url_id == addressed.url_id)
            .map(|(qualifier, config)| {
                target_of_record(
                    function,
                    qualifier,
                    config,
                    addressed.request_path,
                    addressed.region,
                )
            })
    })
}

/// The target one stored configuration is, for a request that carried `request_path` and —
/// for the addressings that carry one — named `region`. The ONE place a
/// [`FunctionUrlTarget`] is built, so every addressing resolves the same facts about the same
/// configuration.
fn target_of_record(
    function: &FunctionRecord,
    qualifier: &str,
    config: &FunctionUrlConfigRecord,
    request_path: &str,
    region: Option<&str>,
) -> FunctionUrlTarget {
    FunctionUrlTarget {
        url_id: config.url_id.clone(),
        function_name: function.config.name.clone(),
        qualifier: qualifier.to_string(),
        resource_arn: function.versioned_arn(qualifier),
        auth_type: config.auth_type.clone(),
        invoke_mode: config.served_invoke_mode(),
        cors: config.cors.clone(),
        request_path: normalized_path(request_path),
        addressed_region: region.map(str::to_string),
        endpoint_host: announced_host(&config.url, &function.region, &config.url_id),
    }
}

/// The HOST of an announced function URL: the host NAME of the address the configuration
/// holds, or — for a stored value carrying none — the documented endpoint host of that url
/// id, so a handler is never told about an address that is not one.
///
/// The host NAME and not the whole authority: what a function URL event reports is the
/// endpoint's DOMAIN NAME (`requestContext.domainName`, and the `host` the request carried
/// under it), and a transport port is no part of a domain name — AWS's own announced value
/// (`<url-id>.lambda-url.<region>.on.aws`) names none, and this emulator announces the same
/// resource at an address whose port is the socket it could bind rather than anything about
/// the function URL. So the value a handler is told about is the one every announced form of
/// this endpoint agrees on ([`::service_common::data_plane_addressing::host_only`], which
/// also drops any userinfo a `Host` may carry).
fn announced_host(url: &str, region: &str, url_id: &str) -> String {
    url.split_once("://")
        .map(|(_scheme, rest)| rest.split('/').next().unwrap_or(rest))
        .map(::service_common::data_plane_addressing::host_only)
        .filter(|host| !host.is_empty())
        .map_or_else(
            || function_url_endpoint_host(region, url_id),
            str::to_string,
        )
}

/// The path a request names WITHIN the URL: the one it carried, and `/` for a request that
/// carried none at all — the path a function URL's own root is addressed by.
fn normalized_path(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    path.to_string()
}

/// The url id, request path and region of a request that names the endpoint INSIDE ITS
/// PATH, behind the reserved namespace marker, or `None` for a path carrying no marker.
///
/// Both spellings of the endpoint behind the marker resolve — the endpoint LABEL followed by
/// the url id (`/_aws/lambda-url/<url-id>/…`) and the endpoint HOST itself
/// (`/_aws/<url-id>.lambda-url.<region>.on.aws/…`) — because what a rewriter has to carry
/// across is the host, and those are the two ways to write it. Only the second carries the
/// region.
fn local_endpoint_path(path: &str) -> Option<(&str, &str, Option<&str>)> {
    let (marker, rest) = split_first_segment(path)?;
    if marker != ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (endpoint, rest) = split_first_segment(rest)?;
    if let Some((url_id, region)) = function_url_host_parts(endpoint) {
        return Some((url_id, rest, Some(region)));
    }
    (endpoint == FUNCTION_URL_LABEL)
        .then(|| split_first_segment(rest))
        .flatten()
        .map(|(url_id, rest)| (url_id, rest, None))
}

/// The first path segment and the path that follows it, with the leading `/` kept on the
/// remainder so it stays a path, through the shared data-plane reading.
fn split_first_segment(path: &str) -> Option<(&str, &str)> {
    ::service_common::data_plane_addressing::first_segment(path)
}

/// The first path segment alone, for the control-plane path-space test.
fn first_segment(path: &str) -> Option<&str> {
    split_first_segment(path).map(|(first, _)| first)
}

/// The `NextMarker` a `ListFunctionUrlConfigs` page hands back once entries remain after
/// it: the position it ended on, carried inside a token this store MINTED for it.
///
/// `NextMarker` is "The pagination token that's included if more results are available"
/// (`smithy_query(query="shape", shape="ListFunctionUrlConfigsResponse")`), so what it
/// names is a place in the listing and nothing else. It is therefore not any of the
/// identifiers this API addresses a resource by — least of all the qualifier the entry
/// sits on, which a caller would read as naming a function version or an alias — and a
/// caller cannot act on it except by handing it back.
///
/// The position travels inside the token behind an authenticator over it
/// ([`PageTokenKey`]), so the only values this listing continues from are the ones it
/// issued, and the token is spelled the way every paginated listing of this service spells
/// one (`super::page_marker`).
#[must_use]
pub fn function_url_marker(key: &PageTokenKey, qualifier: &str) -> String {
    super::minted_marker(key, qualifier)
}

/// The position `marker` names, or `None` for any value this store did not mint — a value
/// of the wrong spelling, and one whose authenticator does not match the position it
/// travels with.
///
/// A listing refuses a value this answers `None` for with the one sentence this service
/// words for a marker it did not issue (`super::INVALID_MARKER_MESSAGE`).
#[must_use]
pub fn function_url_marker_cursor(key: &PageTokenKey, marker: &str) -> Option<String> {
    super::marker_cursor(key, marker)
}

/// The time-zone designator a function URL's own instants carry: the UTC DESIGNATOR `Z`.
///
/// `FunctionUrlConfig$CreationTime` and `$LastModifiedTime` are each documented "in
/// ISO-8601 format (YYYY-MM-DDThh:mm:ss.sTZD)" linking the W3C NOTE-datetime profile
/// (`smithy_query(query="doc", shape="FunctionUrlConfig", member="CreationTime")`), and the
/// `TZD` of that profile is either this designator or an offset written `+hh:mm` — a
/// colon-less four-digit offset is not one of its forms. So the instants of THIS resource
/// are rendered in the designator form, and a strict ISO-8601 reader parses every one of
/// them.
///
/// It is the function URL's instants that are rendered this way and not every instant of
/// this service: `FunctionConfiguration$LastModified` and its siblings are recorded by AWS
/// in the NUMERIC offset form their own documentation gives an example of, which is what
/// `super::UTC_DESIGNATOR` spells and what `super::iso8601` renders.
pub const FUNCTION_URL_UTC_DESIGNATOR: &str = "Z";

/// `at` as the ISO-8601 instant a function URL's `CreationTime` and `LastModifiedTime`
/// describe, with the designator [`FUNCTION_URL_UTC_DESIGNATOR`] spells.
///
/// The ONE rendering behind every function-URL answer: the two write paths
/// (`CreateFunctionUrlConfig`, `UpdateFunctionUrlConfig`) store what this renders and every
/// read projects the stored text, so the single-configuration reads and the list projection
/// cannot disagree about the form.
///
/// The instant itself is rendered by the one renderer this service has
/// (`super::designated_iso8601`, which states the truncation and the fixed width every
/// instant member of this service is reported at); all this adds is WHICH designator these
/// members carry.
#[must_use]
pub fn function_url_iso8601(at: ::std::time::SystemTime) -> String {
    super::designated_iso8601(at, FUNCTION_URL_UTC_DESIGNATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A function URL's instant renders in the designator form the member's own profile
    /// admits, at a fixed width, and the text orders as the instants do.
    ///
    /// The rows span what the rendering generalises over: the epoch itself, a subsecond
    /// field of every magnitude (none, milliseconds, and a sub-millisecond remainder that
    /// is truncated away), and instants far apart.
    #[test]
    fn a_function_urls_instant_carries_the_utc_designator_at_a_fixed_width() {
        let epoch = ::std::time::SystemTime::UNIX_EPOCH;
        let rows: &[(&str, ::std::time::Duration)] = &[
            ("the epoch", ::std::time::Duration::from_secs(0)),
            ("a whole second", ::std::time::Duration::from_secs(1_767_225_600)),
            (
                "a millisecond field",
                ::std::time::Duration::new(1_767_225_600, 333_000_000),
            ),
            (
                "a microsecond remainder, truncated away",
                ::std::time::Duration::new(1_767_225_600, 333_444_000),
            ),
            (
                "a nanosecond remainder, truncated away",
                ::std::time::Duration::new(1_767_225_600, 333_999_999),
            ),
            (
                "a later instant",
                ::std::time::Duration::new(1_800_000_000, 1_000),
            ),
        ];
        let mut rendered: Vec<(&str, String)> = Vec::new();
        for (case, since_epoch) in rows {
            let text = function_url_iso8601(epoch + *since_epoch);
            assert!(
                text.ends_with(FUNCTION_URL_UTC_DESIGNATOR) && text.contains('T'),
                "case {case:?}: expected an ISO-8601 instant carrying the \
                 {FUNCTION_URL_UTC_DESIGNATOR:?} designator, got {text:?}"
            );
            assert_eq!(
                text.len(),
                "2026-01-01T00:00:00.000Z".len(),
                "case {case:?}: expected the fixed width of a three-digit subsecond field, \
                 got {text:?}"
            );
            let parsed = ::aws_smithy_types::DateTime::from_str(
                &text,
                ::aws_smithy_types::date_time::Format::DateTime,
            );
            assert!(
                parsed.is_ok(),
                "case {case:?}: a strict ISO-8601 reader must parse {text:?}, got {parsed:?}"
            );
            rendered.push((case, text));
        }
        // The millisecond truncation is what makes the text order the instant order: a
        // sub-millisecond remainder renders as the millisecond instant below it, never
        // above it.
        for window in rendered.windows(2) {
            let (before, after) = (&window[0], &window[1]);
            assert!(
                before.1 <= after.1,
                "case {:?} then {:?}: the text must order as the instants do, got {:?} then \
                 {:?}",
                before.0,
                after.0,
                before.1,
                after.1
            );
        }
    }

    /// A page marker round-trips under the key that minted it, and no value another store
    /// minted or a caller assembled names a position at all.
    #[test]
    fn a_page_marker_round_trips_and_names_no_position_under_another_key() {
        let key = PageTokenKey::new();
        let other = PageTokenKey::new();
        // The qualifiers a function URL can sit on: the unpublished version, and alias
        // names spanning the lengths and characters the model's pattern admits.
        for qualifier in [VERSION_LATEST, "a", "live", "Blue_green-2"] {
            let marker = function_url_marker(&key, qualifier);
            assert_eq!(
                function_url_marker_cursor(&key, &marker).as_deref(),
                Some(qualifier),
                "case {qualifier:?}: expected the marker to name the position it was \
                 minted for, got {:?}",
                function_url_marker_cursor(&key, &marker)
            );
            assert!(
                marker != qualifier && marker.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "case {qualifier:?}: a marker is a hexadecimal token of this store's own, \
                 never the position it continues after, got {marker:?}"
            );
            for (case, value) in [
                ("another store's key", function_url_marker(&other, qualifier)),
                ("the bare qualifier", qualifier.to_string()),
                (
                    "a token shaped like a minted one",
                    ::hex::encode(
                        [
                            &[0_u8; ::service_common::page_tokens::AUTHENTICATOR_BYTES][..],
                            qualifier.as_bytes(),
                        ]
                        .concat(),
                    ),
                ),
                ("not a hexadecimal token at all", format!("{marker}!!")),
            ] {
                assert_eq!(
                    function_url_marker_cursor(&key, &value),
                    None,
                    "case {qualifier:?} / {case}: expected no position from {value:?}"
                );
            }
        }
    }

    /// A host reads as a function URL endpoint exactly when it is shaped like the one this
    /// service mints. The rows span what the reading generalises over: the minted form, a
    /// host carrying a port and userinfo, the region label, and every way the shape can
    /// fail.
    #[test]
    fn a_function_url_host_names_its_url_id() {
        let minted = function_url(DEFAULT_REGION, "0123456789abcdef0123456789abcdef");
        let host = minted
            .strip_prefix("https://")
            .and_then(|url| url.split('/').next())
            .expect("the minted URL carries a host");
        let cases: &[(&str, Option<&str>)] = &[
            (host, Some("0123456789abcdef0123456789abcdef")),
            ("abc.lambda-url.us-west-2.on.aws", Some("abc")),
            ("abc.lambda-url.us-west-2.on.aws:443", Some("abc")),
            ("user@abc.lambda-url.eu-west-3.on.aws", Some("abc")),
            // Not a function URL endpoint at all.
            ("abc.lambda-urlx.us-west-2.on.aws", None),
            ("lambda-url.us-west-2.on.aws", None),
            ("abc.lambda-url..on.aws", None),
            ("abc.lambda-url.us-west-2.amazonaws.com", None),
            ("lambda.us-east-1.amazonaws.com", None),
            ("127.0.0.1:9297", None),
        ];
        for (host, expected) in cases {
            let read = function_url_id_of_host(host);
            assert_eq!(
                read, *expected,
                "case {host:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: what the handler is told about the endpoint it was reached at is the HOST of the
    /// announced address and never the port it is served on — a domain name carries none —
    /// and a stored value naming no address at all falls back to the documented host of that
    /// url id, so a handler is never told about something that is no host.
    ///
    /// The rows span the address forms an announced value takes: the documented host, a
    /// loopback literal with a port and a path, a loopback NAME with a port, a bracketed
    /// IPv6 literal with a port (whose own colons are inside the brackets), a host carrying
    /// userinfo, an address with no port at all, and the values that name no authority.
    #[test]
    fn the_announced_host_is_the_published_addresss_host_without_its_port() {
        let url_id = "0123456789abcdef0123456789abcdef";
        let documented = format!("{url_id}.{FUNCTION_URL_LABEL}.{DEFAULT_REGION}.on.aws");
        // (case, the stored announced value, the host the handler is told about)
        let rows: &[(&str, String, String)] = &[
            (
                "the documented host",
                function_url(DEFAULT_REGION, url_id),
                documented.clone(),
            ),
            (
                "a loopback literal with a port, carrying the url id in its path",
                format!("https://127.0.0.1:9297/_aws/{FUNCTION_URL_LABEL}/{url_id}/"),
                "127.0.0.1".to_string(),
            ),
            (
                "a loopback NAME with a port",
                format!("https://localhost:39355/_aws/{FUNCTION_URL_LABEL}/{url_id}/"),
                "localhost".to_string(),
            ),
            (
                "a bracketed IPv6 literal with a port",
                format!("https://[::1]:39355/_aws/{FUNCTION_URL_LABEL}/{url_id}/"),
                "[::1]".to_string(),
            ),
            (
                "an address carrying userinfo",
                format!("https://user@127.0.0.1:9297/_aws/{FUNCTION_URL_LABEL}/{url_id}/"),
                "127.0.0.1".to_string(),
            ),
            (
                "an address with no port at all",
                format!("https://127.0.0.1/_aws/{FUNCTION_URL_LABEL}/{url_id}/"),
                "127.0.0.1".to_string(),
            ),
            (
                "a stored value naming no authority",
                "https:///nowhere".to_string(),
                documented.clone(),
            ),
            (
                "a stored value that is no URL at all",
                String::new(),
                documented.clone(),
            ),
        ];
        for (case, announced, expected) in rows {
            let told = announced_host(announced, DEFAULT_REGION, url_id);
            assert_eq!(
                told, *expected,
                "case {case:?}: expected the handler to be told {expected:?}, got {told:?}"
            );
            assert!(
                !told.contains(':') || told.starts_with('['),
                "case {case:?}: a domain name carries no port, got {told:?}"
            );
        }
    }
}

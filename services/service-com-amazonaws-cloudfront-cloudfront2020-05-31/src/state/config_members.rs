// SPDX-License-Identifier: CC0-1.0

//! The judgement a submitted `DistributionConfig`'s OWN MEMBERS are admitted by.
//!
//! One rule for the whole document: a member is stored only where the value it carries is
//! inside the domain CloudFront documents for that member and agrees with the members it
//! depends on, and a configuration carrying a value outside it is refused with the error the
//! model declares for THAT member — before anything is minted, stored or replaced. Every
//! payload that writes a whole `DistributionConfig` is held to it
//! ([`config_member_refusal`] from the stateless half, [`absent_trusted_key_group`] for the
//! one reference that is resolved against the account's records), so a create and an update
//! cannot drift apart and nothing reaches the store that a read would answer as valid.
//!
//! The rules, each with what states it:
//!
//! * the published per-list quotas a configuration's own counts are bounded by — the
//!   alternate domain names, origins, origin groups and origin custom headers of a
//!   distribution, and the cookie names, headers and query string keys of one cache
//!   behaviour's legacy cache key;
//! * `DistributionConfig$DefaultRootObject` names "the object name or a path to the object
//!   name", and the service states the characters such a name may carry
//!   ([`DEFAULT_ROOT_OBJECT_CHARACTERS`]) and the length it may reach
//!   ([`MAX_DEFAULT_ROOT_OBJECT`]) — a value outside either names no object;
//! * `CustomErrorResponse` "controls whether CloudFront replaces HTTP status codes in the 4xx
//!   and 5xx range with custom error messages": `ErrorCode` is such a status, `ResponseCode`
//!   is the status "CloudFront returns to the viewer along with the custom error page" — an
//!   HTTP status code — and `ResponsePagePath` is "the path to the custom error page", a
//!   relative path beginning with a slash and carrying nothing a URL path has to have
//!   percent-encoded;
//! * a member documented as an amount of time in SECONDS states a span, and no span is negative
//!   ([`duration_members`] lists them and the sentences that make them durations), so a negative
//!   value names no duration the configuration could run under;
//! * `GeoRestriction` states the agreement between its members: the `Location` items "specify
//!   the countries" a `whitelist` or a `blacklist` covers, and "when it is not enabled,
//!   `Quantity` is `0`, and you can omit `Items`" — so an enabled restriction covering no
//!   country, and a disabled one naming countries, are both unusable; each `Location` is "a
//!   two-letter, uppercase country code" of `ISO 3166-1-alpha-2`;
//! * `ForwardedValues$QueryString`: "if you specify `false` for `QueryString`, CloudFront
//!   doesn't forward any query string parameters to the origin, and doesn't cache based on
//!   query string parameters" — so query string cache keys stated beside it key the cache on
//!   parameters that never reach the cache key at all;
//! * a cache behaviour's three TTLs are ordered, pair by pair ([`ttl_order_message`]) — the
//!   same rule [`super::TTL_ORDER_MESSAGE`] states for a cache policy's three;
//! * `CacheBehavior$TargetOriginId` is "the value of the `ID` field of the origin that you
//!   want CloudFront to route requests to", so it resolves against the `Origins` and
//!   `OriginGroups` of the SAME configuration;
//! * `TrustedKeyGroups$Items` are "the identifiers of key groups" whose public keys verify a
//!   signed URL, so each one resolves against the key groups the account holds.
//!
//! The parent module holds the records these configurations are stored on; nothing here
//! mutates anything.

use super::*;

/// What a submitted distribution configuration is refused with, named by the MODELLED error
/// shape the refusal carries.
///
/// Every operation that writes a whole `DistributionConfig` declares all of these, so a
/// handler maps a variant onto its own operation's error enum and nothing here names one
/// operation's channel.
#[derive(Debug, Clone)]
pub enum ConfigMemberRefusal {
    /// `TooManyDistributionCNAMEs` (HTTP 400): more alternate domain names than
    /// [`MAX_DISTRIBUTION_CNAMES`].
    TooManyDistributionCNAMEs(crate::ops::types::TooManyDistributionCNAMEs),
    /// `TooManyOrigins` (HTTP 400): more origins than [`MAX_ORIGINS_PER_DISTRIBUTION`].
    TooManyOrigins(crate::ops::types::TooManyOrigins),
    /// `TooManyOriginGroupsPerDistribution` (HTTP 400): more origin groups than
    /// [`MAX_ORIGIN_GROUPS_PER_DISTRIBUTION`].
    TooManyOriginGroupsPerDistribution(crate::ops::types::TooManyOriginGroupsPerDistribution),
    /// `TooManyOriginCustomHeaders` (HTTP 400): more origin custom headers than
    /// [`MAX_ORIGIN_CUSTOM_HEADERS`].
    TooManyOriginCustomHeaders(crate::ops::types::TooManyOriginCustomHeaders),
    /// `TooManyCookieNamesInWhiteList` (HTTP 400): one behaviour whitelists more cookie
    /// names than [`MAX_WHITELISTED_COOKIE_NAMES`].
    TooManyCookieNamesInWhiteList(crate::ops::types::TooManyCookieNamesInWhiteList),
    /// `TooManyHeadersInForwardedValues` (HTTP 400): one behaviour forwards more headers
    /// than [`MAX_FORWARDED_HEADERS`].
    TooManyHeadersInForwardedValues(crate::ops::types::TooManyHeadersInForwardedValues),
    /// `TooManyQueryStringParameters` (HTTP 400): one behaviour keys on more query string
    /// parameters than [`MAX_QUERY_STRING_CACHE_KEYS`].
    TooManyQueryStringParameters(crate::ops::types::TooManyQueryStringParameters),
    /// `InvalidDefaultRootObject` (HTTP 400): the default root object names no object.
    InvalidDefaultRootObject(crate::ops::types::InvalidDefaultRootObject),
    /// `InvalidGeoRestrictionParameter` (HTTP 400): the geographic restriction's members
    /// disagree about whether it restricts anything.
    InvalidGeoRestrictionParameter(crate::ops::types::InvalidGeoRestrictionParameter),
    /// `InvalidLocationCode` (HTTP 400): a geographic restriction names something that is no
    /// country code.
    InvalidLocationCode(crate::ops::types::InvalidLocationCode),
    /// `InvalidTTLOrder` (HTTP 400): a behaviour's three TTLs are not ordered.
    InvalidTTLOrder(crate::ops::types::InvalidTTLOrder),
    /// `NoSuchOrigin` (HTTP 404): a cache behaviour targets an origin the configuration does
    /// not carry.
    NoSuchOrigin(crate::ops::types::NoSuchOrigin),
    /// `TrustedKeyGroupDoesNotExist` (HTTP 400): a behaviour trusts a key group the account
    /// does not hold.
    TrustedKeyGroupDoesNotExist(crate::ops::types::TrustedKeyGroupDoesNotExist),
    /// `InvalidArgument` (HTTP 400): a member whose value CloudFront refuses as the
    /// PARAMETER it arrived in ([`invalid_parameter`]) rather than under a code of its own.
    InvalidArgument(crate::ops::types::InvalidArgument),
}

/// The refusal a member whose value CloudFront cannot read carries: the modelled
/// `InvalidArgument`, naming the PARAMETER that carried it.
///
/// The sentence states which parameter the caller has to fix and nothing else — not the value
/// it carried, nor the domain it is outside of — and it is the form this service's refusals of
/// an unusable parameter already take (`super::invalid_function_association_message` names
/// its parameter the same way). Recorded behaviour: the condition is served as the
/// `InvalidArgument` every distribution-writing operation models rather than under a code
/// named for the member.
pub fn invalid_parameter(parameter: &str) -> ConfigMemberRefusal {
    ConfigMemberRefusal::InvalidArgument(crate::ops::types::InvalidArgument {
        message: Some(invalid_parameter_message(parameter)),
    })
}

/// The refusal a member whose value CloudFront can read and will not take carries: the modelled
/// `InvalidArgument`, naming the PARAMETER that carried it and the fact that its value is outside
/// the range the member admits ([`super::parameter_out_of_range_message`]).
///
/// The companion of [`invalid_parameter`] for a value that is the right KIND of thing and outside
/// the domain of the member — an amount of time that is no duration, a number outside the range
/// its member admits — so the two sentences cannot drift apart between the members they refuse.
pub fn parameter_out_of_range(parameter: &str) -> ConfigMemberRefusal {
    ConfigMemberRefusal::InvalidArgument(crate::ops::types::InvalidArgument {
        message: Some(parameter_out_of_range_message(parameter)),
    })
}

/// The most origins ONE distribution's configuration may state.
///
/// CloudFront's published quota is 25 origins per distribution (the Amazon CloudFront
/// Developer Guide's "Quotas", the general quotas on distributions), the same scope
/// [`MAX_CACHE_BEHAVIORS`] carries. Over it, the refusal is the modelled `TooManyOrigins`.
pub const MAX_ORIGINS_PER_DISTRIBUTION: usize = 25;

/// The most origin groups ONE distribution's configuration may state.
///
/// CloudFront's published quota is 10 origin groups per distribution ("Quotas", the general
/// quotas on distributions). Over it, the refusal is the modelled
/// `TooManyOriginGroupsPerDistribution`.
pub const MAX_ORIGIN_GROUPS_PER_DISTRIBUTION: usize = 10;

/// The most origin custom headers ONE distribution's configuration may state, counted across
/// its origins.
///
/// CloudFront's published quota is 10 custom headers that you can configure CloudFront to
/// forward to the origin ("Quotas", the headers quotas), and the modelled refusal names the
/// same scope — "more OriginCustomHeaders than allowed per distribution" — so the count is
/// the one the whole configuration states rather than one origin's.
pub const MAX_ORIGIN_CUSTOM_HEADERS: usize = 10;

/// The most cookie names ONE cache behaviour may whitelist in its legacy cache key.
///
/// CloudFront's published quota is 10 cookies per cache behaviour ("Quotas", the cache-key
/// quotas), and the modelled refusal names that scope: "more cookie names in the whitelist
/// than are allowed per cache behavior".
pub const MAX_WHITELISTED_COOKIE_NAMES: usize = 10;

/// The most headers ONE cache behaviour may forward in its legacy cache key: the same
/// published quota [`MAX_WHITELISTED_COOKIE_NAMES`] states for cookies, under
/// `TooManyHeadersInForwardedValues`.
pub const MAX_FORWARDED_HEADERS: usize = 10;

/// The most query string parameters ONE cache behaviour may key its cache on in its legacy
/// cache key: the same published quota [`MAX_WHITELISTED_COOKIE_NAMES`] states for cookies,
/// under `TooManyQueryStringParameters`.
pub const MAX_QUERY_STRING_CACHE_KEYS: usize = 10;

/// The longest a default root object's name may be.
///
/// `DistributionConfig$DefaultRootObject` is "the object name or a path to the object name",
/// and the modelled refusal is about the FILE NAME — "the default root object file name is
/// too big or contains an invalid character" — so the bound is CloudFront's published
/// maximum of 255 characters for it. The model carries no `@length`, so nothing refuses a
/// longer one before a handler runs.
pub const MAX_DEFAULT_ROOT_OBJECT: usize = 255;

/// The lowest number that is an HTTP status code, and the highest: "the status code consists
/// of three digits", the first of which names one of the five classes (RFC 9110).
pub const HTTP_STATUS_CODES: std::ops::RangeInclusive<i32> = 100..=599;

/// The status codes a custom error response is stated FOR: `CustomErrorResponse` "controls
/// whether CloudFront replaces HTTP status codes in the 4xx and 5xx range with custom error
/// messages", so an `ErrorCode` outside that range names a response CloudFront replaces
/// nothing for.
pub const CUSTOM_ERROR_STATUS_CODES: std::ops::RangeInclusive<i32> = 400..=599;

/// The refusal `config` carries in its OWN MEMBERS, with nothing of the store or of another
/// service read, or `None` when every member is inside the domain documented for it.
///
/// The counts come first — a configuration past a published quota is refused on its size
/// before any of its values is read — then the member values, then the one reference a
/// configuration resolves against ITSELF (the origin a cache behaviour targets). The order is
/// what decides which refusal a configuration carrying several faults is answered with, and
/// it is stated once here so every handler answers the same one.
pub fn config_member_refusal(config: &DistributionConfig) -> Option<ConfigMemberRefusal> {
    if let Some(refusal) = config_count_refusal(config) {
        return Some(refusal);
    }
    if let Some(refusal) = default_root_object_refusal(config) {
        return Some(refusal);
    }
    if let Some(refusal) = custom_error_response_refusal(config) {
        return Some(refusal);
    }
    if let Some(refusal) = duration_member_refusal(config) {
        return Some(refusal);
    }
    if let Some(refusal) = geo_restriction_refusal(config) {
        return Some(refusal);
    }
    if let Some(refusal) = behavior_member_refusal(config) {
        return Some(refusal);
    }
    unresolved_target_origin(config).map(|_| {
        ConfigMemberRefusal::NoSuchOrigin(crate::ops::types::NoSuchOrigin {
            message: Some(NO_SUCH_ORIGIN_MESSAGE.to_string()),
        })
    })
}

/// The first count in `config` past the published quota for that list, or `None`.
fn config_count_refusal(config: &DistributionConfig) -> Option<ConfigMemberRefusal> {
    let aliases = config
        .aliases
        .as_ref()
        .map_or(0, |aliases| items_len(&aliases.items));
    if aliases > MAX_DISTRIBUTION_CNAMES {
        return Some(ConfigMemberRefusal::TooManyDistributionCNAMEs(
            crate::ops::types::TooManyDistributionCNAMEs {
                message: Some(TOO_MANY_DISTRIBUTION_CNAMES_MESSAGE.to_string()),
            },
        ));
    }
    if config.origins.items.len() > MAX_ORIGINS_PER_DISTRIBUTION {
        return Some(ConfigMemberRefusal::TooManyOrigins(
            crate::ops::types::TooManyOrigins {
                message: Some(TOO_MANY_ORIGINS_MESSAGE.to_string()),
            },
        ));
    }
    let origin_groups = config
        .origin_groups
        .as_ref()
        .map_or(0, |groups| items_len(&groups.items));
    if origin_groups > MAX_ORIGIN_GROUPS_PER_DISTRIBUTION {
        return Some(ConfigMemberRefusal::TooManyOriginGroupsPerDistribution(
            crate::ops::types::TooManyOriginGroupsPerDistribution {
                message: Some(TOO_MANY_ORIGIN_GROUPS_MESSAGE.to_string()),
            },
        ));
    }
    let custom_headers: usize = config
        .origins
        .items
        .iter()
        .filter_map(|origin| origin.custom_headers.as_ref())
        .map(|headers| items_len(&headers.items))
        .sum();
    if custom_headers > MAX_ORIGIN_CUSTOM_HEADERS {
        return Some(ConfigMemberRefusal::TooManyOriginCustomHeaders(
            crate::ops::types::TooManyOriginCustomHeaders {
                message: Some(TOO_MANY_ORIGIN_CUSTOM_HEADERS_MESSAGE.to_string()),
            },
        ));
    }

    // The legacy cache key's three lists, which are bounded PER CACHE BEHAVIOUR: each
    // behaviour states its own, so the count weighed is that behaviour's.
    for behavior in behavior_members(config) {
        let Some(forwarded) = behavior.forwarded_values else {
            continue;
        };
        let cookies = forwarded
            .cookies
            .whitelisted_names
            .as_ref()
            .map_or(0, |names| items_len(&names.items));
        if cookies > MAX_WHITELISTED_COOKIE_NAMES {
            return Some(ConfigMemberRefusal::TooManyCookieNamesInWhiteList(
                crate::ops::types::TooManyCookieNamesInWhiteList {
                    message: Some(TOO_MANY_COOKIE_NAMES_MESSAGE.to_string()),
                },
            ));
        }
        let headers = forwarded
            .headers
            .as_ref()
            .map_or(0, |headers| items_len(&headers.items));
        if headers > MAX_FORWARDED_HEADERS {
            return Some(ConfigMemberRefusal::TooManyHeadersInForwardedValues(
                crate::ops::types::TooManyHeadersInForwardedValues {
                    message: Some(TOO_MANY_HEADERS_IN_FORWARDED_VALUES_MESSAGE.to_string()),
                },
            ));
        }
        let keys = forwarded
            .query_string_cache_keys
            .as_ref()
            .map_or(0, |keys| items_len(&keys.items));
        if keys > MAX_QUERY_STRING_CACHE_KEYS {
            return Some(ConfigMemberRefusal::TooManyQueryStringParameters(
                crate::ops::types::TooManyQueryStringParameters {
                    message: Some(TOO_MANY_QUERY_STRING_PARAMETERS_MESSAGE.to_string()),
                },
            ));
        }
    }

    None
}

/// The refusal `config`'s `DefaultRootObject` carries, or `None` when it names an object —
/// or no object at all, which is what the empty element asks for ("if you don't want to
/// specify a default root object … include an empty `DefaultRootObject` element").
fn default_root_object_refusal(config: &DistributionConfig) -> Option<ConfigMemberRefusal> {
    let object = config.default_root_object.as_deref().unwrap_or_default();
    if object.is_empty() {
        return None;
    }
    let message = if object.chars().count() > MAX_DEFAULT_ROOT_OBJECT {
        format!(
            "The parameter DefaultRootObject cannot be longer than \
             {MAX_DEFAULT_ROOT_OBJECT} characters."
        )
    } else if !object.chars().all(|character| {
        character.is_ascii_alphanumeric() || DEFAULT_ROOT_OBJECT_CHARACTERS.contains(character)
    }) {
        INVALID_DEFAULT_ROOT_OBJECT_CHARACTERS_MESSAGE.to_string()
    } else {
        return None;
    };
    Some(ConfigMemberRefusal::InvalidDefaultRootObject(
        crate::ops::types::InvalidDefaultRootObject {
            message: Some(message),
        },
    ))
}

/// The refusal the first unusable member of `config`'s custom error responses carries, or
/// `None` when every one of them is usable.
///
/// The three members are read in the order the shape declares them — the status the response
/// is stated for, the page it returns and the status it returns — so a response carrying more
/// than one fault is refused for the first one.
fn custom_error_response_refusal(config: &DistributionConfig) -> Option<ConfigMemberRefusal> {
    let responses = config.custom_error_responses.as_ref()?;
    for response in responses.items.as_deref().unwrap_or_default() {
        if !CUSTOM_ERROR_STATUS_CODES.contains(&response.error_code) {
            return Some(invalid_parameter("ErrorCode"));
        }
        let page = response.response_page_path.as_deref().unwrap_or_default();
        if !page.is_empty() && (!page.starts_with('/') || !is_unencoded_path(page)) {
            return Some(invalid_parameter("ResponsePagePath"));
        }
        let status = response.response_code.as_deref().unwrap_or_default();
        let is_status_code = status
            .parse::<i32>()
            .is_ok_and(|code| HTTP_STATUS_CODES.contains(&code));
        if !status.is_empty() && !is_status_code {
            return Some(invalid_parameter("ResponseCode"));
        }
    }
    None
}

/// The refusal the first member of `config` that states an amount of time in SECONDS outside
/// the durations there are carries, or `None` when every one of them states a duration.
///
/// A member documented as "the … amount of time, in seconds" states a SPAN, and no span is
/// negative, so a negative value names no duration at all. The model puts no `@range` on the
/// integral types these members target, so nothing refuses such a value before a handler runs,
/// and CloudFront answers it against the PARAMETER that carried it
/// ([`super::parameter_out_of_range_message`]) rather than under a code of its own.
fn duration_member_refusal(config: &DistributionConfig) -> Option<ConfigMemberRefusal> {
    duration_members(config)
        .into_iter()
        .find(|(_, seconds)| *seconds < 0)
        .map(|(parameter, _)| parameter_out_of_range(parameter))
}

/// Every member of `config` that states an amount of time in seconds, each under the name the
/// WIRE reports it by, in the order the document carries them.
///
/// The members, each with the sentence that makes it a duration:
///
/// * `CustomErrorResponse$ErrorCachingMinTTL` — "the minimum amount of time, in seconds, that
///   you want CloudFront to cache the HTTP status code specified in `ErrorCode`";
/// * a cache behaviour's `MinTTL`, `DefaultTTL` and `MaxTTL` — "the {minimum,default,maximum}
///   amount of time that you want objects to stay in CloudFront caches", which the three of a
///   cache policy state the same way ([`super::cache_policy_config_invalid`] reads those);
/// * `CustomOriginConfig$OriginReadTimeout` and `$OriginKeepaliveTimeout` — "specifies how long,
///   in seconds, CloudFront waits for a response from the origin" and "how long, in seconds,
///   CloudFront persists its connection to the origin";
/// * `Origin$ConnectionTimeout` — "the number of seconds that CloudFront waits when trying to
///   establish a connection to the origin".
fn duration_members(config: &DistributionConfig) -> Vec<(&'static str, i64)> {
    let mut out = Vec::new();
    for response in config
        .custom_error_responses
        .as_ref()
        .map(|responses| responses.items.as_deref().unwrap_or_default())
        .unwrap_or_default()
    {
        if let Some(seconds) = response.error_caching_min_ttl {
            out.push(("ErrorCachingMinTTL", seconds));
        }
    }
    for behavior in behavior_members(config) {
        // Destructured rather than indexed, the way the ordering rule below reads the same
        // triple: the three members are named here, so a reordering of the shared view cannot
        // leave a refusal naming the wrong one.
        let [min, default, max] = behavior.ttls;
        for (parameter, stated) in [
            ("MinTTL", min),
            ("DefaultTTL", default),
            ("MaxTTL", max),
        ] {
            if let Some(seconds) = stated {
                out.push((parameter, seconds));
            }
        }
    }
    for origin in &config.origins.items {
        if let Some(seconds) = origin.connection_timeout {
            out.push(("ConnectionTimeout", i64::from(seconds)));
        }
        let Some(custom) = origin.custom_origin_config.as_ref() else {
            continue;
        };
        for (parameter, stated) in [
            ("OriginReadTimeout", custom.origin_read_timeout),
            ("OriginKeepaliveTimeout", custom.origin_keepalive_timeout),
        ] {
            if let Some(seconds) = stated {
                out.push((parameter, i64::from(seconds)));
            }
        }
    }
    out
}

/// The refusal `config`'s geographic restriction carries, or `None` when its members agree
/// and every location it names is a country code.
fn geo_restriction_refusal(config: &DistributionConfig) -> Option<ConfigMemberRefusal> {
    let geo = &config.restrictions.as_ref()?.geo_restriction;
    let locations = geo.items.as_deref().unwrap_or_default();
    let restricts = geo.restriction_type == GEO_RESTRICTION_WHITELIST
        || geo.restriction_type == GEO_RESTRICTION_BLACKLIST;
    if restricts == locations.is_empty() {
        return Some(ConfigMemberRefusal::InvalidGeoRestrictionParameter(
            crate::ops::types::InvalidGeoRestrictionParameter {
                message: Some(INVALID_GEO_RESTRICTION_PARAMETER_MESSAGE.to_string()),
            },
        ));
    }
    locations
        .iter()
        .any(|location| !is_country_code(location))
        .then(|| {
            ConfigMemberRefusal::InvalidLocationCode(crate::ops::types::InvalidLocationCode {
                message: Some(INVALID_LOCATION_CODE_MESSAGE.to_string()),
            })
        })
}

/// The refusal the first cache behaviour of `config` with an unusable cache key carries, or
/// `None` when every behaviour's is usable.
fn behavior_member_refusal(config: &DistributionConfig) -> Option<ConfigMemberRefusal> {
    for behavior in behavior_members(config) {
        if let Some(forwarded) = behavior.forwarded_values {
            let keys = forwarded
                .query_string_cache_keys
                .as_ref()
                .map_or(0, |keys| items_len(&keys.items));
            if keys > 0 && !forwarded.query_string {
                return Some(ConfigMemberRefusal::InvalidArgument(
                    crate::ops::types::InvalidArgument {
                        message: Some(
                            QUERY_STRING_CACHE_KEYS_WITHOUT_QUERY_STRING_MESSAGE.to_string(),
                        ),
                    },
                ));
            }
        }
        // The pairs the ordering rule constrains, each compared only where the configuration
        // supplies both members of it — the same reading [`super::cache_policy_config_invalid`]
        // gives the same three TTLs of a cache policy, since a member the request left out is
        // one the service resolves from the others. The refusal names the PAIR that broke
        // ([`ttl_order_message`]), which is what the caller has to fix.
        let [min, default, max] = behavior.ttls;
        for (lower, upper) in [
            (("MinTTL", min), ("DefaultTTL", default)),
            (("MinTTL", min), ("MaxTTL", max)),
            (("DefaultTTL", default), ("MaxTTL", max)),
        ] {
            let ((lower_member, Some(lower)), (upper_member, Some(upper))) = (lower, upper) else {
                continue;
            };
            if lower > upper {
                return Some(ConfigMemberRefusal::InvalidTTLOrder(
                    crate::ops::types::InvalidTTLOrder {
                        message: Some(ttl_order_message(lower_member, upper_member)),
                    },
                ));
            }
        }
    }
    None
}

/// The `TargetOriginId` of the first cache behaviour of `config` that names neither an origin
/// nor an origin group the SAME configuration carries, or `None` when every behaviour's
/// target resolves.
pub fn unresolved_target_origin(config: &DistributionConfig) -> Option<&str> {
    let origins: BTreeSet<&str> = config
        .origins
        .items
        .iter()
        .map(|origin| origin.id.as_str())
        .chain(
            config
                .origin_groups
                .as_ref()
                .map(|groups| groups.items.as_deref().unwrap_or_default())
                .unwrap_or_default()
                .iter()
                .map(|group| group.id.as_str()),
        )
        .collect();
    behavior_members(config)
        .into_iter()
        .map(|behavior| behavior.target_origin_id)
        .find(|target| !origins.contains(target))
}

/// The entry of a `TrustedKeyGroups` list of `config` that names no key group this account
/// holds, or `None` when every behaviour trusts groups that exist.
///
/// The question is answered against the key group RECORDS — the same state `GetKeyGroup`
/// answers from and the same direction `DeleteKeyGroup` reads when it refuses to release a
/// group a stored distribution still trusts — so a configuration is never stored trusting
/// public keys that are not there.
pub fn absent_trusted_key_group<'c>(
    store: &ComAmazonawsCloudfrontCloudfront20200531State,
    config: &'c DistributionConfig,
) -> Option<&'c str> {
    behavior_members(config)
        .into_iter()
        .filter_map(|behavior| behavior.trusted_key_groups)
        .flat_map(|groups| groups.items.as_deref().unwrap_or_default())
        .map(String::as_str)
        .find(|id| store.key_group(id).is_none())
}

/// The modelled `TrustedKeyGroupDoesNotExist` (HTTP 400) for a behaviour trusting a key group
/// the account does not hold.
pub fn trusted_key_group_does_not_exist() -> crate::ops::types::TrustedKeyGroupDoesNotExist {
    crate::ops::types::TrustedKeyGroupDoesNotExist {
        message: Some(TRUSTED_KEY_GROUP_DOES_NOT_EXIST_MESSAGE.to_string()),
    }
}

/// The members of ONE cache behaviour the rules above read.
///
/// `DefaultCacheBehavior` and `CacheBehavior` declare the same members, so both are walked
/// through this one view rather than through two copies of the same rule.
struct BehaviorMembers<'a> {
    /// `TargetOriginId` — the origin or origin group the behaviour routes requests to.
    target_origin_id: &'a str,
    /// The legacy cache key, where the behaviour states one.
    forwarded_values: Option<&'a ForwardedValues>,
    /// `MinTTL`, `DefaultTTL` and `MaxTTL`, in that order, each where the behaviour states
    /// it.
    ttls: [Option<i64>; 3],
    /// The key groups whose public keys the behaviour trusts, where it states any.
    trusted_key_groups: Option<&'a TrustedKeyGroups>,
}

impl<'a> From<&'a DefaultCacheBehavior> for BehaviorMembers<'a> {
    fn from(behavior: &'a DefaultCacheBehavior) -> Self {
        Self {
            target_origin_id: behavior.target_origin_id.as_str(),
            forwarded_values: behavior.forwarded_values.as_ref(),
            ttls: [behavior.min_ttl, behavior.default_ttl, behavior.max_ttl],
            trusted_key_groups: behavior.trusted_key_groups.as_ref(),
        }
    }
}

impl<'a> From<&'a CacheBehavior> for BehaviorMembers<'a> {
    fn from(behavior: &'a CacheBehavior) -> Self {
        Self {
            target_origin_id: behavior.target_origin_id.as_str(),
            forwarded_values: behavior.forwarded_values.as_ref(),
            ttls: [behavior.min_ttl, behavior.default_ttl, behavior.max_ttl],
            trusted_key_groups: behavior.trusted_key_groups.as_ref(),
        }
    }
}

/// Every cache behaviour of `config`: the default one first, then the ordered ones, which is
/// the order a refusal names them in.
fn behavior_members(config: &DistributionConfig) -> Vec<BehaviorMembers<'_>> {
    let mut out = vec![BehaviorMembers::from(&config.default_cache_behavior)];
    if let Some(behaviors) = &config.cache_behaviors {
        for behavior in behaviors.items.as_deref().unwrap_or_default() {
            out.push(BehaviorMembers::from(behavior));
        }
    }
    out
}

/// Whether `value` is a path that travels in a request line as written: every character is
/// one a URL path carries unencoded, so nothing in it would have to be percent-encoded first.
///
/// The set is RFC 3986's: the ASCII characters a path segment admits, the separators `/` and
/// `%` (which introduces an encoded octet) included. A space, a control character, a
/// delimiter of another part of the URL (`?`, `#`) and a byte outside ASCII all have to be
/// encoded, so a value carrying one of them is no path CloudFront could request.
fn is_unencoded_path(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || "/%-._~!$&'()*+,;=:@".contains(character)
    })
}

/// Whether `value` is a country code a geographic restriction may name: "a two-letter,
/// uppercase country code", read against `ISO 3166-1-alpha-2` — the standard
/// `GeoRestriction$Items` names ("CloudFront and `MaxMind` both use `ISO 3166` country
/// codes") — through the `rust_iso3166` crate's published table rather than a list restated
/// here.
fn is_country_code(value: &str) -> bool {
    value.len() == 2
        && value.bytes().all(|byte| byte.is_ascii_uppercase())
        && ::rust_iso3166::from_alpha2(value).is_some()
}

/// `TooManyDistributionCNAMEs`'s message, as the model documents the shape.
pub const TOO_MANY_DISTRIBUTION_CNAMES_MESSAGE: &str =
    "Your request contains more CNAMEs than are allowed per distribution.";

/// `TooManyOrigins`'s message, as the model documents the shape.
pub const TOO_MANY_ORIGINS_MESSAGE: &str = "You cannot create more origins for the distribution.";

/// `TooManyOriginGroupsPerDistribution`'s message, as the model documents the shape.
pub const TOO_MANY_ORIGIN_GROUPS_MESSAGE: &str =
    "Processing your request would cause you to exceed the maximum number of origin groups \
     allowed.";

/// `TooManyOriginCustomHeaders`'s message, on the wire.
///
/// It names the MEMBER the headers travel in and the scope the quota is published for, and
/// nothing of the request: the count a configuration states is no part of what the caller has
/// to fix. Recorded behaviour, which differs from the sentence the model's documentation of
/// the shape uses for the same condition ("Your request contains too many origin custom
/// headers") — do not "correct" this constant to the documentation sentence.
pub const TOO_MANY_ORIGIN_CUSTOM_HEADERS_MESSAGE: &str =
    "Your request contains more OriginCustomHeaders than allowed per distribution.";

/// `TooManyCookieNamesInWhiteList`'s message, as the model documents the shape.
pub const TOO_MANY_COOKIE_NAMES_MESSAGE: &str =
    "Your request contains more cookie names in the whitelist than are allowed per cache \
     behavior.";

/// `TooManyHeadersInForwardedValues`'s message, as the model documents the shape.
pub const TOO_MANY_HEADERS_IN_FORWARDED_VALUES_MESSAGE: &str =
    "Your request contains too many headers in forwarded values.";

/// `TooManyQueryStringParameters`'s message, as the model documents the shape.
pub const TOO_MANY_QUERY_STRING_PARAMETERS_MESSAGE: &str =
    "Your request contains too many query string parameters.";

/// The characters a default root object's name may carry.
///
/// `DistributionConfig$DefaultRootObject` names "the object name or a path to the object
/// name", and the service states the set itself in the sentence it refuses one outside it
/// with ([`INVALID_DEFAULT_ROOT_OBJECT_CHARACTERS_MESSAGE`]): the ASCII letters and digits,
/// plus the punctuation an object key carries — which is where this set comes from rather than
/// from a reading of what a URL path admits. Recorded behaviour.
pub const DEFAULT_ROOT_OBJECT_CHARACTERS: &str = "_-.*$/~\"'&";

/// The sentence CloudFront refuses a default root object carrying a character outside
/// [`DEFAULT_ROOT_OBJECT_CHARACTERS`] with: it names the parameter and ENUMERATES the set,
/// which is the whole of what the caller has to fix, and carries nothing from the request.
/// Recorded behaviour; the model's documentation of the shape describes the condition instead
/// ("The default root object file name is too big or contains an invalid character").
pub const INVALID_DEFAULT_ROOT_OBJECT_CHARACTERS_MESSAGE: &str =
    "The parameter DefaultRootObject can only contain these characters: \
     [a-zA-Z0-9_-.*$/~\"'&]";

/// The sentence a cache behaviour whose TTLs are not ordered is refused with: the PAIR that
/// broke the order, named by the two members and the relation between them that has to hold.
///
/// The variable parts are the two member names, read off the pair being compared; nothing
/// else of the request reaches the wire, so the values the configuration stated are no part of
/// it. Recorded behaviour. A cache POLICY's three TTLs are refused with the whole chain
/// instead ([`super::TTL_ORDER_MESSAGE`]), which is that surface's own sentence for the same
/// rule.
pub fn ttl_order_message(lower: &str, upper: &str) -> String {
    format!("{lower} should be less than or equal to {upper}")
}

/// The sentence a cache behaviour that keys its cache on query string parameters it forwards
/// none of is refused with: it names BOTH members whose values disagree — the one that is
/// `false` and the one that is set — which is what the caller has to fix, and carries no value
/// from the request. Recorded behaviour: the condition is served as the `InvalidArgument`
/// every distribution-writing operation models, not as the `InvalidQueryStringParameters` the
/// model also declares.
pub const QUERY_STRING_CACHE_KEYS_WITHOUT_QUERY_STRING_MESSAGE: &str =
    "The parameter QueryString cannot be false if query strings are set in \
     QueryStringCacheKeys.";

/// `InvalidGeoRestrictionParameter`'s message, on the wire: it names the SECTION whose
/// members disagree and nothing from the request, so every disagreement inside it reads the
/// same. Recorded behaviour; the model's documentation of the shape describes the condition
/// instead ("The specified geo restriction parameter is not valid").
pub const INVALID_GEO_RESTRICTION_PARAMETER_MESSAGE: &str =
    "Your request contains invalid Restriction parameters.";

/// `InvalidLocationCode`'s message, on the wire: it states the CONDITION over the whole list
/// — one or more of its entries is no country code — and carries nothing from the request, so
/// the entry that failed is no part of what the caller has to fix. Recorded behaviour; the
/// model's documentation of the shape states it in the singular ("The location code specified
/// is not valid").
pub const INVALID_LOCATION_CODE_MESSAGE: &str =
    "Your request contains one or more invalid location codes.";

/// `NoSuchOrigin`'s message, on the wire: it states the CONDITION over both of the lists a
/// cache behaviour's target is resolved against, and nothing from the request — the target
/// that failed to resolve is no part of what the caller has to fix. Recorded behaviour.
pub const NO_SUCH_ORIGIN_MESSAGE: &str =
    "One or more of your cache behavior target origins or origin groups do not exist.";

/// `TrustedKeyGroupDoesNotExist`'s message, on the wire: it states the CONDITION over the
/// whole list — one or more of the groups a behaviour trusts is not there — and carries
/// nothing from the request, so the entry that failed to resolve is no part of what the caller
/// has to fix. Recorded behaviour; the model's documentation of the shape states it in the
/// singular ("The specified key group does not exist").
pub const TRUSTED_KEY_GROUP_DOES_NOT_EXIST_MESSAGE: &str =
    "One or more of your trusted keygroups do not exist.";

/// Lift a [`ConfigMemberRefusal`] into the modelled error enum `$error` names.
///
/// Every operation that writes a whole `DistributionConfig` declares its own error enum, with
/// one variant per modelled error, so the lift is per operation — but the MAPPING is the same
/// for all of them: each refusal names the modelled shape it carries, and the variant of that
/// name takes it. Written once here so a handler cannot answer one of these conditions with
/// another operation's code, and so a refusal added above is added to every handler at once.
#[macro_export]
macro_rules! config_member_error {
    ($refusal:expr, $error:ident) => {
        match $refusal {
            $crate::state::ConfigMemberRefusal::TooManyDistributionCNAMEs(error) => {
                $error::TooManyDistributionCNAMEs(error)
            }
            $crate::state::ConfigMemberRefusal::TooManyOrigins(error) => {
                $error::TooManyOrigins(error)
            }
            $crate::state::ConfigMemberRefusal::TooManyOriginGroupsPerDistribution(error) => {
                $error::TooManyOriginGroupsPerDistribution(error)
            }
            $crate::state::ConfigMemberRefusal::TooManyOriginCustomHeaders(error) => {
                $error::TooManyOriginCustomHeaders(error)
            }
            $crate::state::ConfigMemberRefusal::TooManyCookieNamesInWhiteList(error) => {
                $error::TooManyCookieNamesInWhiteList(error)
            }
            $crate::state::ConfigMemberRefusal::TooManyHeadersInForwardedValues(error) => {
                $error::TooManyHeadersInForwardedValues(error)
            }
            $crate::state::ConfigMemberRefusal::TooManyQueryStringParameters(error) => {
                $error::TooManyQueryStringParameters(error)
            }
            $crate::state::ConfigMemberRefusal::InvalidDefaultRootObject(error) => {
                $error::InvalidDefaultRootObject(error)
            }
            $crate::state::ConfigMemberRefusal::InvalidGeoRestrictionParameter(error) => {
                $error::InvalidGeoRestrictionParameter(error)
            }
            $crate::state::ConfigMemberRefusal::InvalidLocationCode(error) => {
                $error::InvalidLocationCode(error)
            }
            $crate::state::ConfigMemberRefusal::InvalidTTLOrder(error) => {
                $error::InvalidTTLOrder(error)
            }
            $crate::state::ConfigMemberRefusal::NoSuchOrigin(error) => {
                $error::NoSuchOrigin(error)
            }
            $crate::state::ConfigMemberRefusal::TrustedKeyGroupDoesNotExist(error) => {
                $error::TrustedKeyGroupDoesNotExist(error)
            }
            $crate::state::ConfigMemberRefusal::InvalidArgument(error) => {
                $error::InvalidArgument(error)
            }
        }
    };
}

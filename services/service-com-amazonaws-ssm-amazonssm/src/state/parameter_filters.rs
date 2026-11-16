// SPDX-License-Identifier: CC0-1.0

//! The `ParameterStringFilter` vocabulary the two filter-taking parameter reads publish,
//! how a filter MATCHES a stored parameter, and the sentences a rejected filter is refused
//! with.
//!
//! One concern, read by both `DescribeParameters` and `GetParametersByPath`, so the keys,
//! the options, the enum-valued attributes and the comparison cannot drift between them.
//! Every name stays reachable as `crate::state::<name>` through [`super`]'s re-export.

use super::*;

// ---------------------------------------------------------------------------
// Reading a hierarchy of parameters
// ---------------------------------------------------------------------------

/// A `ParameterStringFilter` key that filters on a parameter's `ParameterType`.
pub const FILTER_KEY_TYPE: &str = "Type";

/// A `ParameterStringFilter` key that filters on the KMS key a `SecureString` names.
pub const FILTER_KEY_KEY_ID: &str = "KeyId";

/// A `ParameterStringFilter` key that filters on a version's labels.
pub const FILTER_KEY_LABEL: &str = "Label";

/// The `Key` values a hierarchy read NARROWS by, and the ones it names when it refuses a
/// key outside its vocabulary ([`invalid_filter_key_message`]).
///
/// The modelled key pattern is `^(tag:.+|Name|Type|KeyId|Path|Label|Tier|DataType)$`,
/// and "For `GetParametersByPath`, the following patterns listed for `Key` aren't valid:
/// `tag`, `DataType`, `Name`, `Path`, and `Tier`" (the `ParameterStringFilter$Key`
/// doc) — which leaves these three. The tag form is the one key of that pattern a
/// hierarchy read still ACCEPTS, narrowing nothing by it
/// ([`is_parameters_by_path_filter_key`]).
pub const PARAMETERS_BY_PATH_FILTER_KEYS: &[&str] =
    &[FILTER_KEY_TYPE, FILTER_KEY_KEY_ID, FILTER_KEY_LABEL];

/// Compare a filter's value against the whole held value.
pub const FILTER_OPTION_EQUALS: &str = "Equals";

/// Compare a filter's value against the start of the held value.
pub const FILTER_OPTION_BEGINS_WITH: &str = "BeginsWith";

/// The comparison options a hierarchy read's filters accept: "For filters used with
/// `GetParametersByPath`, valid options include `Equals` and `BeginsWith`" (the
/// `ParameterStringFilter$Option` doc).
pub const PARAMETERS_BY_PATH_FILTER_OPTIONS: &[&str] =
    &[FILTER_OPTION_EQUALS, FILTER_OPTION_BEGINS_WITH];

/// The options a `Label` filter accepts: "(Exception: For filters using `Label` as the
/// Key name, the only valid option is `Equals`.)" (the same doc).
pub const LABEL_FILTER_OPTIONS: &[&str] = &[FILTER_OPTION_EQUALS];

/// Whether `key` is a filter key a hierarchy read accepts (see
/// [`PARAMETERS_BY_PATH_FILTER_KEYS`]), the tag form included.
///
/// A `tag:<key>` entry is ACCEPTED here and narrows nothing: the service takes it on a
/// hierarchy read and serves the whole subtree whatever value it carries, so the keys it
/// refuses are the other keys the modelled pattern allows rather than every key outside
/// its own three. Where the filter does narrow is a describe
/// ([`is_describe_parameters_filter_key`]).
pub fn is_parameters_by_path_filter_key(key: &str) -> bool {
    PARAMETERS_BY_PATH_FILTER_KEYS.contains(&key) || tag_filter_key(key).is_some()
}

/// The tag a `tag:<key>` filter key names, or `None` when `key` names no tag — the one
/// reading of the modelled `tag:.+` key form, so every filter that takes a tag key reads
/// it alike.
///
/// `tag:` alone names no tag, the modelled pattern requiring at least one character after
/// the prefix.
pub fn tag_filter_key(key: &str) -> Option<&str> {
    key.strip_prefix(TAG_FILTER_KEY_PREFIX)
        .filter(|tag| !tag.is_empty())
}

/// The comparison options a hierarchy read accepts for `key`.
pub fn parameters_by_path_filter_options(key: &str) -> &'static [&'static str] {
    if key == FILTER_KEY_LABEL {
        LABEL_FILTER_OPTIONS
    } else {
        PARAMETERS_BY_PATH_FILTER_OPTIONS
    }
}

/// Whether `held` carries a value matching one of `wanted` under `option`.
///
/// `held` is every value the stored record carries for the filter's key (a parameter
/// has one type, at most one key id, one tier, one data type, at most one value for a
/// given tag, and any number of labels), and a filter matches when any held value
/// matches any wanted one. The comparison is the one `option` names, and a filter
/// naming none compares whole values.
pub fn string_filter_matches(held: &[&str], option: &str, wanted: &[String]) -> bool {
    held.iter().any(|value| {
        wanted.iter().any(|want| match option {
            FILTER_OPTION_BEGINS_WITH => value.starts_with(want.as_str()),
            FILTER_OPTION_CONTAINS => value.contains(want.as_str()),
            _ => *value == want.as_str(),
        })
    })
}

// ---------------------------------------------------------------------------
// The sentences a filter refusal is worded with
// ---------------------------------------------------------------------------

/// What a `ParameterFilters` entry naming a key the operation does not accept is
/// refused with.
///
/// `valid` is the accepting operation's OWN key vocabulary, so the caller is told what
/// it may ask for here rather than what Parameter Store accepts somewhere else — the
/// two operations that take these filters accept different keys
/// ([`DESCRIBE_PARAMETERS_FILTER_KEYS`], [`PARAMETERS_BY_PATH_FILTER_KEYS`]).
pub fn invalid_filter_key_message(key: &str, valid: &[&str]) -> String {
    format!(
        "The following filter key is not valid: {key}. Valid filter keys include: [{}].",
        valid.join(", ")
    )
}

/// What a `ParameterFilters` entry naming an option its key does not accept is refused
/// with, where `valid` is the option vocabulary of THAT key.
///
/// The options are listed in ascending order rather than in the order the vocabulary is
/// declared in: the declaration order here follows the documentation's prose, and the
/// service lists the options it accepts sorted.
pub fn invalid_filter_option_message(option: &str, valid: &[&str]) -> String {
    let mut listed: Vec<&str> = valid.to_vec();
    listed.sort_unstable();
    format!(
        "The following filter option is not valid: {option}. Valid options include: [{}].",
        listed.join(", ")
    )
}

/// What a `ParameterFilters` entry carrying a value the attribute its key names cannot
/// hold is refused with, where `valid` is that attribute's whole value set.
///
/// `valid` is listed in the order the modelled enum declares its values, which is the
/// order the service lists them in: the sentence tells the caller what the attribute can
/// be, so the vocabulary is quoted as the model states it rather than re-sorted.
pub fn invalid_filter_value_message(value: &str, valid: &[&str]) -> String {
    format!(
        "The following filter value is not valid: {value}. Valid values include: [{}].",
        valid.join(", ")
    )
}

// ---------------------------------------------------------------------------
// The filters a parameter describe accepts
// ---------------------------------------------------------------------------

/// A `ParameterStringFilter` key that filters on a parameter's name.
pub const FILTER_KEY_NAME: &str = "Name";

/// A `ParameterStringFilter` key that filters on the hierarchy a parameter lies in.
pub const FILTER_KEY_PATH: &str = "Path";

/// A `ParameterStringFilter` key that filters on a version's `ParameterTier`.
pub const FILTER_KEY_TIER: &str = "Tier";

/// A `ParameterStringFilter` key that filters on a version's `ParameterDataType`.
pub const FILTER_KEY_DATA_TYPE: &str = "DataType";

/// Compare a filter's value against any part of the held value.
pub const FILTER_OPTION_CONTAINS: &str = "Contains";

/// A `Path` filter option that reaches every level under the path.
pub const FILTER_OPTION_RECURSIVE: &str = "Recursive";

/// A `Path` filter option that reaches only the level immediately below the path.
pub const FILTER_OPTION_ONE_LEVEL: &str = "OneLevel";

/// The non-tag `Key` values a `ParameterStringFilter` may carry on a parameter
/// describe.
///
/// The modelled key pattern is `^(tag:.+|Name|Type|KeyId|Path|Label|Tier|DataType)$`,
/// and "for `DescribeParameters`, all of the listed patterns are valid except `Label`"
/// (the `ParameterStringFilter$Key` doc) — which leaves these five plus the tag form
/// ([`TAG_FILTER_KEY_PREFIX`]). They are held in the order the service LISTS them in
/// when it refuses a key outside the set ([`invalid_filter_key_message`]).
pub const DESCRIBE_PARAMETERS_FILTER_KEYS: &[&str] = &[
    FILTER_KEY_PATH,
    FILTER_KEY_NAME,
    FILTER_KEY_TYPE,
    FILTER_KEY_KEY_ID,
    FILTER_KEY_TIER,
    FILTER_KEY_DATA_TYPE,
];

/// The `Key` values the deprecated `ParametersFilter` carries, from the modelled
/// `ParametersFilterKey` enum (`Name`, `Type`, `KeyId`). "This data type is deprecated.
/// Instead, use `ParameterStringFilter`", so each one filters on the attribute the
/// string filter of the same key does.
pub const LEGACY_PARAMETERS_FILTER_KEYS: &[&str] =
    &[FILTER_KEY_NAME, FILTER_KEY_TYPE, FILTER_KEY_KEY_ID];

/// The comparison options a parameter describe's filters accept: "For all filters used
/// with `DescribeParameters`, valid options include `Equals` and `BeginsWith`" (the
/// `ParameterStringFilter$Option` doc).
pub const DESCRIBE_PARAMETERS_FILTER_OPTIONS: &[&str] =
    &[FILTER_OPTION_EQUALS, FILTER_OPTION_BEGINS_WITH];

/// The options a `Name` filter accepts: "The `Name` filter additionally supports the
/// `Contains` option" (the same doc).
pub const NAME_FILTER_OPTIONS: &[&str] = &[
    FILTER_OPTION_EQUALS,
    FILTER_OPTION_BEGINS_WITH,
    FILTER_OPTION_CONTAINS,
];

/// The options a `Path` filter accepts: "(Exception: For filters using the key `Path`,
/// valid options include `Recursive` and `OneLevel`.)" (the same doc).
pub const PATH_FILTER_OPTIONS: &[&str] = &[FILTER_OPTION_RECURSIVE, FILTER_OPTION_ONE_LEVEL];

/// How many filters one parameter describe carries, across both the deprecated
/// `Filters` member and `ParameterFilters`.
///
/// Every filter is compared against every parameter of the Region, so the work one
/// request asks for is the number of filters times the number of parameters held times
/// the values per filter. The last two are already bounded — by
/// [`MAX_PARAMETERS_PER_REGION`] and by the modelled value-list length (1..50) — and
/// neither filter list carries a modelled length, so the count is bounded here. Ten
/// holds a describe's whole published key vocabulary (`Name`, `Type`, `KeyId`, `Path`,
/// `Tier`, `DataType`) with room for tag keys beside it, and is the order this service's
/// own filter lists are modelled at (a session describe's is 1..6, a patch describe's
/// 0..5).
pub const MAX_DESCRIBE_PARAMETERS_FILTERS: usize = 10;

/// Whether `key` is a filter key a parameter describe accepts (see
/// [`DESCRIBE_PARAMETERS_FILTER_KEYS`]).
pub fn is_describe_parameters_filter_key(key: &str) -> bool {
    DESCRIBE_PARAMETERS_FILTER_KEYS.contains(&key) || tag_filter_key(key).is_some()
}

/// The comparison options a parameter describe accepts for `key`.
pub fn describe_parameters_filter_options(key: &str) -> &'static [&'static str] {
    match key {
        FILTER_KEY_NAME => NAME_FILTER_OPTIONS,
        FILTER_KEY_PATH => PATH_FILTER_OPTIONS,
        _ => DESCRIBE_PARAMETERS_FILTER_OPTIONS,
    }
}

/// The comparison a filter on `key` that names no option performs.
///
/// A filter naming no option compares whole values, as every other string filter in
/// this service does — except a `Path` filter, whose options are the two reaches of a
/// hierarchy and whose narrower one is the reach a hierarchy read defaults to
/// (`GetParametersByPath`'s `Recursive` is "default false").
pub fn describe_parameters_default_filter_option(key: &str) -> &'static str {
    if key == FILTER_KEY_PATH {
        FILTER_OPTION_ONE_LEVEL
    } else {
        FILTER_OPTION_EQUALS
    }
}

/// The values a filter on `key` can be satisfied by, when the attribute it names is a
/// modelled enum, or `None` when it is not.
///
/// A parameter's type and tier are modelled enums (`ParameterType`, `ParameterTier`), so
/// a filter value outside one asks for an attribute value no parameter can hold — whatever
/// comparison the filter names, because a closed enum holds no value that merely begins
/// with one of its members either. Such a value is `InvalidFilterValue`
/// ([`invalid_filter_value_message`]) rather than a narrowing that matches nothing.
pub fn filter_enum_values(key: &str) -> Option<&'static [&'static str]> {
    match key {
        FILTER_KEY_TYPE => Some(PARAMETER_TYPES_AS_MODELLED),
        FILTER_KEY_TIER => Some(PARAMETER_TIERS_AS_MODELLED),
        _ => None,
    }
}

/// The values of `key` the stored parameter `record` carries at `version`, for a
/// `ParameterStringFilter` on a parameter describe to compare against.
///
/// The version-held attributes come from [`ParameterVersionRecord::filter_values`], the
/// accessor a hierarchy read already compares against, so the two reads cannot drift;
/// the keys only a describe publishes (`Tier`, `DataType`, `Name` and a tag) are added
/// here. A key naming an attribute this parameter does not carry (a tag nobody set, a
/// key id on a parameter that is not a `SecureString`) yields an EMPTY list, which
/// matches no filter value rather than refusing the filter.
pub fn describe_filter_values<'a>(
    record: &'a ParameterRecord,
    version: &'a ParameterVersionRecord,
    key: &str,
) -> Vec<&'a str> {
    // The keys a hierarchy read and a describe share name attributes of the version, so
    // they are read through the one accessor both reads use rather than a second copy.
    if let Some(held) = version.filter_values(key) {
        return held;
    }
    match key {
        FILTER_KEY_TIER => vec![version.tier.as_str()],
        FILTER_KEY_DATA_TYPE => vec![version.data_type.as_str()],
        FILTER_KEY_NAME => vec![record.name.as_str()],
        _ => key
            .strip_prefix(TAG_FILTER_KEY_PREFIX)
            .and_then(|tag| record.tags.get(tag))
            .map(String::as_str)
            .into_iter()
            .collect(),
    }
}

/// Whether `name` is a name one of `values` asks for under `option`.
///
/// The leading separator is not part of a name's identity ([`canonical_name`]), so a
/// filter value spelled with one asks for the same parameter as one spelled without —
/// except under `Contains`, where the separator is one of the characters being searched
/// for, so the name is searched in its hierarchical spelling.
pub fn name_filter_matches(name: &str, option: &str, values: &[String]) -> bool {
    if option == FILTER_OPTION_CONTAINS {
        let hierarchical = format!("/{}", canonical_name(name));
        return values.iter().any(|want| hierarchical.contains(want));
    }
    let canonical: Vec<String> = values
        .iter()
        .map(|want| canonical_name(want).to_string())
        .collect();
    string_filter_matches(&[canonical_name(name)], option, &canonical)
}

/// Whether the parameter `record` at `version` carries what one describe filter asks
/// for: the key names an attribute of the parameter, and `option` is how its held value
/// is compared against `values`.
///
/// `Path` is the one key that does not compare a held value: it names a hierarchy, and a
/// parameter is in it when its name lies under the path ([`is_under_path`]) — the whole
/// hierarchy under `Recursive`, the level immediately below it under `OneLevel`.
///
/// A TAG's value is compared in FULL whatever option the filter names: a tag is matched by
/// its value, which `BeginsWith` does not turn into a prefix search, so a filter value that
/// is a strict prefix of the stored tag value matches nothing. The option vocabulary bounds
/// what such a filter may carry ([`describe_parameters_filter_options`]); it does not widen
/// what it reaches.
pub fn describe_parameter_matches(
    record: &ParameterRecord,
    version: &ParameterVersionRecord,
    key: &str,
    option: &str,
    values: &[String],
) -> bool {
    match key {
        FILTER_KEY_NAME => name_filter_matches(&record.name, option, values),
        FILTER_KEY_PATH => values.iter().any(|want| {
            parameter_path(want).is_ok_and(|path| {
                is_under_path(
                    canonical_name(&record.name),
                    path,
                    option == FILTER_OPTION_RECURSIVE,
                )
            })
        }),
        _ => {
            let compared = if tag_filter_key(key).is_some() {
                FILTER_OPTION_EQUALS
            } else {
                option
            };
            string_filter_matches(
                &describe_filter_values(record, version, key),
                compared,
                values,
            )
        }
    }
}

// SPDX-License-Identifier: CC0-1.0

//! The authentication context class reference (ACR) levels a user pool reports:
//! the four fixed levels every pool has, and the name each one carries.

use super::*;

/// How many ACR levels a user pool has. `AcrConfigurationType` "Defines the
/// custom names that Amazon Cognito reports for the four fixed authentication
/// context class reference (ACR) levels in a user pool. The map is keyed by
/// level, from `Level1` through `Level4`" and is `@length 0..4` keyed by
/// `AcrLevelKeyType`, `@pattern ^Level[1-4]$` (this service's model), so the set
/// of levels is fixed and only their names are configurable.
const ACR_LEVELS: u32 = 4;

/// The key ACR level `level` is reported under: the `Level<n>` form
/// `AcrLevelKeyType` admits.
#[must_use]
pub fn acr_level_key(level: u32) -> String {
    format!("Level{level}")
}

/// The name ACR level `level` carries for a pool that does not override it: "By
/// default, the levels are named `urn:cognito:loa:1` through
/// `urn:cognito:loa:4`, and Amazon Cognito applies the default name to any level
/// that you don't specify" (`CreateUserPool.AcrConfiguration`, this service's
/// model) — fixed text and the level's own number, with nothing from the
/// request in it.
#[must_use]
pub fn default_acr_value(level: u32) -> String {
    format!("urn:cognito:loa:{level}")
}

/// The `AcrConfiguration` a pool is STORED with: all four fixed levels, each
/// named as `requested` names it where it names one and by the service default
/// where it does not.
///
/// A pool always reports a name for every level — the levels are fixed, and a
/// request "can override a subset of the levels"
/// (`CreateUserPool.AcrConfiguration`) — so a request that overrides some of
/// them, or none at all, is stored with the defaults filled in beside what it
/// named, and every read reports the names the pool's `acr` token claim
/// actually carries.
pub fn effective_acr_configuration(
    requested: Option<HashMap<String, AcrLevelConfigType>>,
) -> HashMap<String, AcrLevelConfigType> {
    let mut configuration = requested.unwrap_or_default();
    for level in 1..=ACR_LEVELS {
        configuration
            .entry(acr_level_key(level))
            .or_insert_with(|| AcrLevelConfigType {
                acr_value: default_acr_value(level),
            });
    }
    configuration
}

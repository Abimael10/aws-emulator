// SPDX-License-Identifier: CC0-1.0

//! Which connections `TestConnection` can test at all, what it refuses an incomplete
//! authentication configuration with, and why no test of an external data source succeeds
//! here.
//!
//! A test is asked of ONE connection: either the definition the request carries inline
//! (`TestConnectionInput`) or the stored one its `ConnectionName` addresses. Both are read
//! by the rules below, so the restriction a client meets is the same whichever form it
//! sends.

use super::*;

/// The modelled `ConnectionType` values that are NOT unified connections: the legacy
/// connection kinds the Data Catalog has always carried.
///
/// Source: the `ConnectionType` enum of this service's model
/// (`smithy_query shape=ConnectionType`), whose values fall into these — a generic JDBC
/// endpoint, the file, document, stream and network kinds, a Marketplace or customer-supplied
/// connector, and the two view-validation kinds — and, beside them, the per-source connectors
/// of the UNIFIED connection surface (`SALESFORCE`, `SERVICENOW`, `BIGQUERY`, `POSTGRESQL`,
/// one per source). RECORDED behaviour: a test of a connection of a legacy kind is refused
/// with the unified-only `ValidationException`, which is a restriction on the KIND and so is
/// read off this one list wherever the definition under test came from — while the SENTENCE
/// it is reported with is the one the service words for that request form
/// ([`messages::TEST_CONNECTION_INPUT_UNIFIED_ONLY`] for an inline definition,
/// [`messages::TEST_CONNECTION_STORED_UNIFIED_ONLY`] for a stored connection).
pub const LEGACY_CONNECTION_TYPES: [&str; 9] = [
    "JDBC",
    "SFTP",
    "MONGODB",
    "KAFKA",
    "NETWORK",
    "MARKETPLACE",
    "CUSTOM",
    "VIEW_VALIDATION_REDSHIFT",
    "VIEW_VALIDATION_ATHENA",
];

/// Whether `connection_type` is a UNIFIED connection, i.e. one of the per-source connector
/// kinds rather than one of the legacy kinds above ([`LEGACY_CONNECTION_TYPES`]).
///
/// A value outside the modelled enum cannot reach here: `TestConnectionInput$ConnectionType`
/// is an enum the request layer validates, and a stored connection's type was validated the
/// same way when it was written.
pub fn is_unified_connection_type(connection_type: &str) -> bool {
    !LEGACY_CONNECTION_TYPES.contains(&connection_type)
}

/// The `AuthenticationType` whose credentials live in an OAuth2 properties structure: one of
/// the modelled `AuthenticationType` values (`BASIC`, `OAUTH2`, `CUSTOM`, `IAM`).
pub const AUTHENTICATION_TYPE_OAUTH2: &str = "OAUTH2";
/// See [`AUTHENTICATION_TYPE_OAUTH2`]: the type whose credentials are a user name and a
/// password.
pub const AUTHENTICATION_TYPE_BASIC: &str = "BASIC";
/// See [`AUTHENTICATION_TYPE_OAUTH2`]: the type whose credentials are the connector's own
/// map of values.
pub const AUTHENTICATION_TYPE_CUSTOM: &str = "CUSTOM";

/// The member of `AuthenticationConfiguration` an inline test configuration is MISSING, or
/// `None` when it carries the credentials its `AuthenticationType` needs.
///
/// Each authentication type is configured through the one member its credentials live in, and
/// the member path is what the refusal names
/// ([`messages::authentication_member_required_for_type`]): RECORDED behaviour is that an
/// `OAUTH2` configuration with no `OAuth2Properties` is refused naming
/// `AuthenticationConfiguration.OAuth2Properties`, and the same reading answers for the other
/// types the model declares. The two credential types that may instead be held in Secrets
/// Manager are satisfied by a `SecretArn` as well, which is what their own documentation
/// states: `BasicAuthenticationCredentials` is "for supplying basic auth credentials when not
/// providing a `SecretArn` value". `IAM` carries no credentials of its own, and a
/// configuration naming no type at all states no requirement to check.
pub fn missing_authentication_member(
    configuration: &AuthenticationConfigurationInput,
) -> Option<&'static str> {
    let authentication_type = configuration.authentication_type.as_deref()?;
    let held_in_secrets_manager = configuration.secret_arn.is_some();
    match authentication_type {
        AUTHENTICATION_TYPE_OAUTH2 if configuration.o_auth2_properties.is_none() => {
            Some("OAuth2Properties")
        }
        AUTHENTICATION_TYPE_BASIC
            if configuration.basic_authentication_credentials.is_none()
                && !held_in_secrets_manager =>
        {
            Some("BasicAuthenticationCredentials")
        }
        AUTHENTICATION_TYPE_CUSTOM
            if configuration.custom_authentication_credentials.is_none()
                && !held_in_secrets_manager =>
        {
            Some("CustomAuthenticationCredentials")
        }
        _ => None,
    }
}

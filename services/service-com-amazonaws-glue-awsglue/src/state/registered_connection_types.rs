// SPDX-License-Identifier: CC0-1.0

//! The CUSTOM connection types this ACCOUNT registered, and what a read of one answers.
//!
//! `RegisterConnectionType` "registers a custom connection type in Glue based on the
//! configuration provided", so the record it writes is this account's own — unlike the
//! connectors the service itself ships, which every caller is served the same description of
//! ([`connection_type_entry`]). The record carries everything the registration
//! stated: the properties a connection of the type is created with, the authentication the
//! connector accepts, and the REST configuration its requests and responses are interpreted
//! by, which is the one thing `DescribeConnectionType` reports for "custom REST-based
//! connection types registered via RegisterConnectionType" and for nothing else.
//!
//! The namespace is per ACCOUNT and keyed by the type's name, like the crawlers and the
//! custom entity types that live beside the catalog tree: `DescribeConnectionType` and
//! `DeleteConnectionType` address a connection type by `ConnectionType` alone, with no
//! `CatalogId` member, so the name a registration sends is the whole address. The key is the
//! name exactly as sent — the model documents no case fold for `NameString`.
//!
//! What a registration may call its type is stated by the member itself: "must be prefixed
//! with "REST-" to indicate it is a REST-based connector"
//! (`RegisterConnectionTypeRequest$ConnectionType`), which is [`CUSTOM_CONNECTION_TYPE_PREFIX`]
//! — and is also why a custom name can never collide with one of the shipped connectors,
//! none of whose modelled values carries that prefix.

use std::collections::HashMap;

use super::*;
use crate::ops::types::{
    AllowedValue, AuthConfiguration, BasicAuthenticationProperties, Capabilities,
    ConnectionPropertiesConfiguration, ConnectorAuthenticationConfiguration,
    ConnectorOAuth2Properties, ConnectorProperty, CustomAuthenticationProperties, Property,
    RegisterConnectionTypeInput, RestConfiguration,
};

/// The prefix a custom connection type's name must carry: "must be prefixed with "REST-" to
/// indicate it is a REST-based connector"
/// (`RegisterConnectionTypeRequest$ConnectionType`). A registration whose name does not
/// carry it is refused rather than stored.
pub const CUSTOM_CONNECTION_TYPE_PREFIX: &str = "REST-";

/// The ARN resource-type segment of a registered connection type
/// (`connectionType/<name>`), spelled as the camel-cased resource type AWS names this kind
/// of Glue resource by — the convention this crate's other segments with no modelled
/// spelling follow (`customEntityType`, `mlTransform`, `usageProfile`; see
/// [`TAGGED_RESOURCE_TYPES`]).
pub const CONNECTION_TYPE_ARN_SEGMENT: &str = "connectionType";

/// Ceiling on the custom connection types this emulator will hold, for the same reason as
/// [`MAX_CRAWLERS`]: `RegisterConnectionType` writes a record any caller can reach, each
/// one carrying a whole REST configuration, so the number of records one process will hold
/// is bounded here. A registration past it is refused with that operation's own modelled
/// `ResourceNumberLimitExceededException`.
pub const MAX_REGISTERED_CONNECTION_TYPES: usize = 1_000;

/// The way a CUSTOM connection type reaches its data source: a REST source read through its
/// own API, which is what "Currently only "REST" protocol is supported"
/// (`RegisterConnectionTypeRequest$IntegrationType`) leaves. The family is what decides the
/// descriptors that follow from the way the source is reached — the Spark connection
/// options, the compute environments and the physical placement — exactly as it does for a
/// shipped connector; what the REGISTRATION states is the type's own data.
const CUSTOM_CONNECTION_FAMILY: ConnectionFamily = ConnectionFamily::RestSource;

/// One custom connection type this account registered: the whole configuration the
/// registration stated, so a later read answers what was registered and not a description
/// composed for it.
#[derive(Debug, Clone)]
pub struct RegisteredConnectionTypeEntry {
    /// The name the type was registered under, which is the address every operation over it
    /// names it by.
    pub connection_type: String,
    /// The protocol the connector speaks, as the registration stated it (the modelled
    /// `IntegrationType`).
    pub integration_type: String,
    /// What the registration says the type is for, or `None` where it stated nothing.
    pub description: Option<String>,
    /// The base URL and the additional request parameters a connection of this type is
    /// created with.
    pub connection_properties: ConnectionPropertiesConfiguration,
    /// The authentication types the connector accepts, and the properties each of them
    /// calls for.
    pub connector_authentication_configuration: ConnectorAuthenticationConfiguration,
    /// How the connector's requests and responses are interpreted: the one member a read of
    /// a custom type reports that a shipped connector has none of.
    pub rest_configuration: RestConfiguration,
    /// The tags the registration assigned, which the tagging surface reads and writes
    /// through this record ([`TaggedResourceKind::ConnectionType`]).
    pub tags: HashMap<String, String>,
}

impl RegisteredConnectionTypeEntry {
    /// The record a registration writes: everything the request stated, stored as sent, so
    /// a read reports the registration and nothing else.
    pub fn from_register_input(input: &RegisterConnectionTypeInput) -> Self {
        Self {
            connection_type: input.connection_type.clone(),
            integration_type: input.integration_type.clone(),
            description: input.description.clone(),
            connection_properties: input.connection_properties.clone(),
            connector_authentication_configuration: input
                .connector_authentication_configuration
                .clone(),
            rest_configuration: input.rest_configuration.clone(),
            tags: input.tags.clone().unwrap_or_default(),
        }
    }

    /// Everything `DescribeConnectionType` reports about this type: the registration's own
    /// data — what it is for, the credentials it takes, the properties a connection of it is
    /// created with and the REST configuration its traffic is interpreted by — beside the
    /// descriptors that follow from the way a REST source is reached
    /// ([`CUSTOM_CONNECTION_FAMILY`]).
    pub fn details(&self) -> ConnectionTypeDetails {
        let family = CUSTOM_CONNECTION_FAMILY;
        let capabilities = Capabilities {
            supported_authentication_types: self
                .connector_authentication_configuration
                .authentication_types
                .clone(),
            supported_data_operations: family.supported_data_operations(),
            supported_compute_environments: family.supported_compute_environments(),
        };
        let connection_options = family.connection_options();
        let compute_environment_configurations =
            family.compute_environment_configurations(&capabilities, &connection_options);
        ConnectionTypeDetails {
            connection_type: self.connection_type.clone(),
            description: self.description.clone(),
            capabilities,
            connection_properties: self.connection_property_descriptors(),
            connection_options,
            authentication_configuration: self.authentication_descriptors(),
            compute_environment_configurations,
            physical_connection_requirements: family.physical_connection_requirements(),
            rest_configuration: Some(self.rest_configuration.clone()),
        }
    }

    /// The properties a create of a connection of this type sets: the base URL and the
    /// additional request parameters the registration declared ("defines the base URL and
    /// additional request parameters needed during connection creation for this connection
    /// type" — `RegisterConnectionTypeRequest$ConnectionProperties`).
    ///
    /// Each parameter is keyed by its own name, as every properties map of this surface is
    /// keyed (`PropertiesMap`'s key targets `PropertyName`, "the name of the property"). The
    /// BASE URL is keyed by [`BASE_URL_PROPERTY`] instead — the key a create of such a
    /// connection states the base URL under, which is one key whatever display name the
    /// registration gave that declaration, because the declaration is the member
    /// `ConnectionPropertiesConfiguration$Url`: "the base instance URL for the endpoint that
    /// this connection type will connect to". The declaration's own name stays on the
    /// descriptor's `Name`, which is the display name of the property and not the key a
    /// create sends it under.
    fn connection_property_descriptors(&self) -> HashMap<String, Property> {
        let mut descriptors = descriptor_map(
            self.connection_properties
                .additional_request_parameters
                .iter()
                .flatten()
                .collect(),
        );
        if let Some(url) = &self.connection_properties.url {
            descriptors.insert(BASE_URL_PROPERTY.to_string(), descriptor_of(url));
        }
        descriptors
    }

    /// How a connection of this type supplies its credentials: the authentication types the
    /// registration declared, each described with the properties THAT REGISTRATION declared
    /// for it rather than with the generic descriptors a shipped connector reports.
    ///
    /// A type the registration declared no authentication type for takes no credentials, and
    /// reports no configuration at all, exactly as a shipped connector that takes none does.
    /// Nothing is reported for a secret either: a custom connector's credentials are the
    /// properties its own registration declared, which states no Secrets Manager secret to
    /// hold them in.
    fn authentication_descriptors(&self) -> Option<AuthConfiguration> {
        let configuration = &self.connector_authentication_configuration;
        let types: Vec<&str> = configuration
            .authentication_types
            .iter()
            .map(String::as_str)
            .collect();
        let mut descriptors = authentication_configuration(&types)?;
        descriptors.secret_arn = None;
        descriptors.basic_authentication_properties = configuration
            .basic_authentication_properties
            .as_ref()
            .map(basic_descriptors);
        descriptors.o_auth2_properties = configuration
            .o_auth2_properties
            .as_ref()
            .map(oauth2_descriptors);
        descriptors.custom_authentication_properties = configuration
            .custom_authentication_properties
            .as_ref()
            .map(custom_descriptors);
        Some(descriptors)
    }
}

/// The user name and the password a BASIC connector takes, as its registration declared
/// them (`BasicAuthenticationProperties`).
fn basic_descriptors(properties: &BasicAuthenticationProperties) -> HashMap<String, Property> {
    descriptor_map(
        properties
            .username
            .iter()
            .chain(properties.password.iter())
            .collect::<Vec<_>>(),
    )
}

/// The credentials a CUSTOM connector takes: the authentication parameters its registration
/// declared, which are the keys the connector itself defines
/// (`CustomAuthenticationProperties$AuthenticationParameters`).
fn custom_descriptors(properties: &CustomAuthenticationProperties) -> HashMap<String, Property> {
    descriptor_map(properties.authentication_parameters.iter().collect::<Vec<_>>())
}

/// The OAuth2 properties a connector takes: the grant its registration declared, and the
/// properties of THAT grant's own leg — the connector declares one grant
/// (`ConnectorOAuth2Properties$OAuth2GrantType` is required), so the properties reported are
/// the ones that grant calls for and not every grant's.
fn oauth2_descriptors(properties: &ConnectorOAuth2Properties) -> HashMap<String, Property> {
    let mut declared: Vec<&ConnectorProperty> = Vec::new();
    if let Some(grant) = &properties.client_credentials_properties {
        declared.extend(grant.token_url.iter());
        declared.extend(grant.client_id.iter());
        declared.extend(grant.client_secret.iter());
        declared.extend(grant.scope.iter());
        declared.extend(grant.token_url_parameters.iter().flatten());
    }
    if let Some(grant) = &properties.jwt_bearer_properties {
        declared.extend(grant.token_url.iter());
        declared.extend(grant.jwt_token.iter());
        declared.extend(grant.token_url_parameters.iter().flatten());
    }
    if let Some(grant) = &properties.authorization_code_properties {
        declared.extend(grant.authorization_code_url.iter());
        declared.extend(grant.authorization_code.iter());
        declared.extend(grant.redirect_uri.iter());
        declared.extend(grant.token_url.iter());
        declared.extend(grant.client_id.iter());
        declared.extend(grant.client_secret.iter());
        declared.extend(grant.scope.iter());
        declared.extend(grant.prompt.iter());
        declared.extend(grant.token_url_parameters.iter().flatten());
    }

    let mut descriptors = descriptor_map(declared);
    // The grant itself is a property of the create, admitting the one grant the
    // registration declared, as a shipped connector's OAuth2 grant property admits the
    // grants that connector supports.
    descriptors.insert(
        OAUTH2_GRANT_TYPE_PROPERTY.to_string(),
        Property {
            name: OAUTH2_GRANT_TYPE_PROPERTY.to_string(),
            description: String::new(),
            required: true,
            default_value: None,
            property_types: vec![PROPERTY_TYPE_USER_INPUT.to_string()],
            allowed_values: Some(vec![AllowedValue {
                description: None,
                value: properties.o_auth2_grant_type.clone(),
            }]),
            data_operation_scopes: None,
            key_override: None,
            property_location: None,
        },
    );
    descriptors
}

/// The member of `AuthenticationConfigurationInput` that carries the OAuth2 grant an
/// access token is obtained by, which is the key the grant property is reported under.
const OAUTH2_GRANT_TYPE_PROPERTY: &str = "OAuth2GrantType";

/// The key a create of a connection of a custom type states its BASE URL under: the member
/// of `ConnectionPropertiesConfiguration` that declares it (`Url`), which is one key for
/// every custom type, since "the base instance URL for the endpoint that this connection
/// type will connect to" is one property of the create whatever display name the
/// registration gave its declaration.
const BASE_URL_PROPERTY: &str = "URL";

/// A property the caller supplies, one of the modelled `PropertyType` values — the type a
/// registered property carries when its own declaration states none.
const PROPERTY_TYPE_USER_INPUT: &str = "USER_INPUT";

/// The properties a registration declared, as the descriptors a read reports them as, keyed
/// by each property's own name.
fn descriptor_map(declared: Vec<&ConnectorProperty>) -> HashMap<String, Property> {
    declared
        .into_iter()
        .map(|property| (property.name.clone(), descriptor_of(property)))
        .collect()
}

/// One declared `ConnectorProperty` as the `Property` a read reports: every member the two
/// shapes share, carried across unchanged — the key the connector sends the value under, the
/// values it admits, where in the request it goes and what kind of value it is.
///
/// A declaration states no DESCRIPTION of its property (`ConnectorProperty` has no such
/// member), so the descriptor carries none: the registration is what a read of a custom type
/// reports, and prose for it would be this emulator's invention rather than the account's
/// registration.
fn descriptor_of(property: &ConnectorProperty) -> Property {
    Property {
        name: property.name.clone(),
        description: String::new(),
        required: property.required,
        default_value: property.default_value.clone(),
        property_types: vec![property.property_type.clone()],
        allowed_values: property.allowed_values.as_ref().map(|values| {
            values
                .iter()
                .map(|value| AllowedValue {
                    description: None,
                    value: value.clone(),
                })
                .collect()
        }),
        data_operation_scopes: None,
        key_override: property.key_override.clone(),
        property_location: property.property_location.clone(),
    }
}

/// The ARN of the connection type registered under `connection_type`, as served in
/// `region`: the identity a registration answers with, and the value the tagging surface
/// addresses the record by.
pub fn registered_connection_type_arn(region: &str, connection_type: &str) -> String {
    ::engine::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{CONNECTION_TYPE_ARN_SEGMENT}/{connection_type}"),
    )
}

impl ComAmazonawsGlueAwsglueState {
    /// The custom connection type registered under `connection_type`, or `None` when this
    /// account registered none — the ONE lookup every operation over a registered type goes
    /// through, so a read and a delete address the same record.
    pub fn registered_connection_type(
        &self,
        connection_type: &str,
    ) -> Option<&RegisteredConnectionTypeEntry> {
        self.registered_connection_types.get(connection_type)
    }

    /// Whether one more custom connection type may be registered, i.e. whether this store
    /// holds fewer than [`MAX_REGISTERED_CONNECTION_TYPES`].
    pub fn can_register_connection_type(&self) -> bool {
        self.registered_connection_types.len() < MAX_REGISTERED_CONNECTION_TYPES
    }
}

// SPDX-License-Identifier: CC0-1.0

//! The CONNECTION TYPES this service serves, and what each one is configured with.
//!
//! `DescribeConnectionType` answers "full details of the supported options for a given
//! connection type in Glue", so the types it serves are the types a connection can be
//! created with: the values of the model's `ConnectionType` enum
//! ([`crate::ops::types::CONNECTION_TYPE_VALUES`]), which is the key space of
//! `ConnectionInput$ConnectionType`. A name outside that set names no connection type at
//! all, and a read of it is refused rather than answered.
//!
//! What a type is configured with is the CATALOG ENTRY recorded for that type
//! ([`ConnectionTypeEntry`], in [`entries`]): the entry carries the type's own data — the
//! data source it reaches and the authentication types a connection of it may be created
//! with — so two types reached the same way still answer differently where the service
//! publishes different things about them, and a name with no entry is no connection type at
//! all.
//!
//! The DESCRIPTORS of an entry's properties follow the connectivity FAMILY the entry names
//! ([`ConnectionFamily`]), because the model's own key spaces state those per way of
//! reaching a source rather than per product: a connection's properties are keyed by
//! `ConnectionPropertyKey` (the key of the `ConnectionProperties` map `CreateConnection`
//! takes), its physical requirements by the members of `PhysicalConnectionRequirements`,
//! and its authentication by the members of `AuthenticationConfigurationInput` and the
//! structures beneath it. The per-compute-environment configuration is derived from the
//! same family, one entry per environment that family supports.
//!
//! None of this is per-account state: these are the connectors the service itself ships, so
//! every caller is served the same description of them. A CUSTOM connection type is the
//! other kind — an account-level record written by `RegisterConnectionType`, carrying its
//! own REST configuration — and is described from that record, not from here.

use std::collections::HashMap;

use super::{AUTHENTICATION_TYPE_BASIC, AUTHENTICATION_TYPE_CUSTOM, AUTHENTICATION_TYPE_OAUTH2};
use crate::ops::types::{
    AllowedValue, AuthConfiguration, Capabilities, ComputeEnvironmentConfiguration, Property,
    RestConfiguration,
};

mod entries;

pub use entries::*;

/// The `AuthenticationType` whose credentials are the caller's own IAM identity, so the
/// connection carries none: the fourth of the modelled `AuthenticationType` values, beside
/// the three the connection tests already name ([`AUTHENTICATION_TYPE_BASIC`]).
pub const AUTHENTICATION_TYPE_IAM: &str = "IAM";

/// The Glue Spark runtime, one of the modelled `ComputeEnvironment` values
/// ([`crate::ops::types::COMPUTE_ENVIRONMENT_VALUES`]).
const COMPUTE_ENVIRONMENT_SPARK: &str = "SPARK";
/// The Glue Python shell runtime; see [`COMPUTE_ENVIRONMENT_SPARK`].
const COMPUTE_ENVIRONMENT_PYTHON: &str = "PYTHON";
/// The Athena runtime; see [`COMPUTE_ENVIRONMENT_SPARK`].
const COMPUTE_ENVIRONMENT_ATHENA: &str = "ATHENA";

/// A read of the data source, one of the modelled `DataOperation` values
/// ([`crate::ops::types::DATA_OPERATION_VALUES`]).
const DATA_OPERATION_READ: &str = "READ";
/// A write to the data source; see [`DATA_OPERATION_READ`].
const DATA_OPERATION_WRITE: &str = "WRITE";

/// A property the caller supplies, one of the modelled `PropertyType` values
/// ([`crate::ops::types::PROPERTY_TYPE_VALUES`]).
const PROPERTY_TYPE_USER_INPUT: &str = "USER_INPUT";
/// A property whose value is a credential; see [`PROPERTY_TYPE_USER_INPUT`].
const PROPERTY_TYPE_SECRET: &str = "SECRET";
/// A property that is either a credential or a plain value; see
/// [`PROPERTY_TYPE_USER_INPUT`].
const PROPERTY_TYPE_SECRET_OR_USER_INPUT: &str = "SECRET_OR_USER_INPUT";
/// A property the service itself supplies, which the caller reads rather than sets; see
/// [`PROPERTY_TYPE_USER_INPUT`].
const PROPERTY_TYPE_READ_ONLY: &str = "READ_ONLY";

/// How a connection of some type REACHES its data source. Every connection type the
/// service serves belongs to exactly one family, and the family decides the property,
/// option and physical-placement descriptors the type is described with, together with the
/// compute environments that can use it — two engines reached over JDBC are configured with
/// the same keys. What the family does NOT decide is the type's own catalog data: the
/// source it reaches and the credentials it takes come from the type's
/// [`ConnectionTypeEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionFamily {
    /// A data store reached over JDBC by a JDBC URL the caller writes out: the legacy
    /// `JDBC` kind the Data Catalog has always carried.
    GenericJdbc,
    /// A relational engine reached over JDBC by host, port and database name.
    Relational,
    /// A document store reached by one connection URL.
    DocumentStore,
    /// A record stream reached by its bootstrap servers.
    Stream,
    /// A file transfer endpoint reached by host and port.
    FileTransfer,
    /// An Amazon Web Services service reached with the caller's own IAM identity, which
    /// carries no credentials of its own.
    AwsService,
    /// A path into a VPC that carries no data source of its own, so what it is configured
    /// with is its physical placement alone.
    Network,
    /// A connector the customer supplies or subscribes to in Amazon Web Services
    /// Marketplace, which brings its own driver.
    Connector,
    /// A REST data source — a SaaS application or an HTTPS-reached store — read through its
    /// own API.
    RestSource,
    /// The engine a catalog view's Amazon Redshift dialect is validated against.
    ViewValidationRedshift,
    /// The engine a catalog view's Athena dialect is validated against.
    ViewValidationAthena,
}

/// The family `connection_type` is reached by, or `None` when it is no connection type this
/// service serves — read off that type's catalog entry
/// ([`connection_type_entry`]), which is the only place a type is recorded.
pub fn connection_type_family(connection_type: &str) -> Option<ConnectionFamily> {
    connection_type_entry(connection_type).map(|entry| entry.family)
}

/// Everything `DescribeConnectionType` reports about one connection type — a connector the
/// service itself ships ([`describe_connection_type`]) or one this account registered
/// ([`RegisteredConnectionTypeEntry::details`]).
#[derive(Debug, Clone)]
pub struct ConnectionTypeDetails {
    /// The type described, as the request named it.
    pub connection_type: String,
    /// What a connection of this type connects to, or `None` where nothing states it — a
    /// custom type whose registration carried no `Description`.
    pub description: Option<String>,
    /// The authentication types, data operations and compute environments it supports.
    pub capabilities: Capabilities,
    /// The properties a create of this type sets in `ConnectionInput$ConnectionProperties`.
    pub connection_properties: HashMap<String, Property>,
    /// The options a Spark ETL script passes this connection's data source.
    pub connection_options: HashMap<String, Property>,
    /// How credentials are supplied, or `None` for a type that takes none.
    pub authentication_configuration: Option<AuthConfiguration>,
    /// One entry per compute environment the type supports.
    pub compute_environment_configurations: HashMap<String, ComputeEnvironmentConfiguration>,
    /// Where in a VPC the connection sits: the placement every connection may state.
    pub physical_connection_requirements: HashMap<String, Property>,
    /// How the connector's requests and responses are interpreted, which a type carries
    /// only where it was REGISTERED with one (`RegisterConnectionType`): `None` for every
    /// connector the service itself ships.
    pub rest_configuration: Option<RestConfiguration>,
}

/// Everything the service knows about `connection_type`, or `None` when it serves no such
/// connection type: the catalog entry recorded for that type, with the descriptors the way
/// it reaches its source carries.
pub fn describe_connection_type(connection_type: &str) -> Option<ConnectionTypeDetails> {
    let entry = connection_type_entry(connection_type)?;
    let family = entry.family;
    let capabilities = entry.capabilities();
    let connection_options = family.connection_options();
    let compute_environment_configurations =
        family.compute_environment_configurations(&capabilities, &connection_options);
    Some(ConnectionTypeDetails {
        connection_type: entry.connection_type.to_string(),
        description: Some(family.describe(entry.source)),
        capabilities,
        connection_properties: family
            .connection_properties(takes_stored_credentials(entry.authentication_types)),
        connection_options,
        authentication_configuration: authentication_configuration(entry.authentication_types),
        compute_environment_configurations,
        physical_connection_requirements: family.physical_connection_requirements(),
        // A connector the service ships is reached by the driver it ships with, not by a
        // REST configuration an account registered for it.
        rest_configuration: None,
    })
}

impl ConnectionTypeEntry {
    /// The capabilities of a connection of this type, as `DescribeConnectionType` and
    /// `ListConnectionTypes` both report them: the credentials this type takes come from its
    /// own entry, and what a job may do with it and which runtimes can use it from the way it
    /// reaches its source.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            supported_authentication_types: self
                .authentication_types
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            supported_data_operations: self.family.supported_data_operations(),
            supported_compute_environments: self.family.supported_compute_environments(),
        }
    }
}

impl ConnectionFamily {
    /// What a connection of this family connects to, naming the data source the catalog
    /// entry records: the family states the shape of the source and the entry states which
    /// source it is.
    pub fn describe(self, source: &str) -> String {
        match self {
            Self::GenericJdbc => format!(
                "A connection to a data store reached over {source}, configured with the \
                 {source} connection URL of that store."
            ),
            Self::Relational => format!(
                "A connection to {source}, a database reached over JDBC by host, port and \
                 database name."
            ),
            Self::DocumentStore => {
                format!("A connection to {source}, a document store reached by its connection URL.")
            }
            Self::Stream => format!(
                "A connection to {source}, a record stream reached by its bootstrap servers."
            ),
            Self::FileTransfer => format!(
                "A connection to {source}, a file transfer endpoint reached by host and port."
            ),
            Self::AwsService => format!(
                "A connection to {source}, reached with the identity the caller's credentials \
                 speak for."
            ),
            Self::Network => format!(
                "A connection to {source}, carrying no data source of its own: what it \
                 configures is the subnet and the security groups a job reaches them through."
            ),
            Self::Connector => format!(
                "A connection to a data store served by {source}, which brings the driver the \
                 store is read and written through."
            ),
            Self::RestSource => {
                format!("A connection to the {source} data source, read through its own REST API.")
            }
            Self::ViewValidationRedshift => format!(
                "A connection used to validate the {source} dialect of a catalog view's SQL \
                 against the cluster or workgroup that runs it."
            ),
            Self::ViewValidationAthena => format!(
                "A connection used to validate the {source} dialect of a catalog view's SQL \
                 against the workgroup that runs it."
            ),
        }
    }

    /// The data operations a job may run against a source of this family. A REST source is
    /// read; a store Glue has a driver for is read and written; a VPC path carries no data
    /// of its own, so neither.
    pub fn supported_data_operations(self) -> Vec<String> {
        let operations: &[&str] = match self {
            Self::GenericJdbc
            | Self::Relational
            | Self::DocumentStore
            | Self::Stream
            | Self::FileTransfer
            | Self::AwsService
            | Self::Connector => &[DATA_OPERATION_READ, DATA_OPERATION_WRITE],
            Self::RestSource | Self::ViewValidationRedshift | Self::ViewValidationAthena => {
                &[DATA_OPERATION_READ]
            }
            Self::Network => &[],
        };
        operations.iter().map(|value| value.to_string()).collect()
    }

    /// The compute environments that can use a connection of this family: the Spark runtime
    /// wherever Glue has a driver for the source, the Python shell where the source is
    /// reached by an API or a JDBC engine, and Athena where the source is queryable through
    /// a federated catalog.
    pub fn supported_compute_environments(self) -> Vec<String> {
        let environments: &[&str] = match self {
            Self::Relational | Self::AwsService | Self::Connector => &[
                COMPUTE_ENVIRONMENT_SPARK,
                COMPUTE_ENVIRONMENT_PYTHON,
                COMPUTE_ENVIRONMENT_ATHENA,
            ],
            Self::GenericJdbc | Self::RestSource | Self::Network => {
                &[COMPUTE_ENVIRONMENT_SPARK, COMPUTE_ENVIRONMENT_PYTHON]
            }
            Self::DocumentStore | Self::Stream | Self::FileTransfer => {
                &[COMPUTE_ENVIRONMENT_SPARK]
            }
            Self::ViewValidationAthena => &[COMPUTE_ENVIRONMENT_ATHENA],
            // A Redshift view validation is run by Redshift itself, which is no Glue
            // compute environment.
            Self::ViewValidationRedshift => &[],
        };
        environments.iter().map(|value| value.to_string()).collect()
    }

    /// The properties a create of a connection of this family sets, keyed as the create
    /// keys them: by a value of the modelled `ConnectionPropertyKey` enum, which is the key
    /// of `ConnectionInput$ConnectionProperties`.
    ///
    /// `takes_stored_credentials` says whether the credentials the TYPE takes may be held
    /// in a Secrets Manager secret instead of on the connection, which is one more property
    /// of the create wherever that is so.
    pub fn connection_properties(self, takes_stored_credentials: bool) -> HashMap<String, Property> {
        let mut properties = match self {
            Self::GenericJdbc => vec![
                required_input(
                    "JDBC_CONNECTION_URL",
                    "The JDBC URL of the data store this connection reaches.",
                ),
                optional_input(
                    "JDBC_DRIVER_JAR_URI",
                    "The Amazon S3 location of the JDBC driver this connection loads.",
                ),
                optional_input(
                    "JDBC_DRIVER_CLASS_NAME",
                    "The class name of the JDBC driver this connection loads.",
                ),
                enforce_ssl_property(),
                optional_input(
                    "CUSTOM_JDBC_CERT",
                    "The Amazon S3 location of the certificate the data store presents.",
                ),
            ],
            Self::Relational => vec![
                required_input("HOST", "The host name of the database server."),
                required_input("PORT", "The port the database server listens on."),
                optional_input("DATABASE", "The name of the database to connect to."),
                enforce_ssl_property(),
            ],
            Self::DocumentStore => vec![
                required_input(
                    "CONNECTION_URL",
                    "The URL of the document store this connection reaches.",
                ),
                optional_input("DATABASE", "The name of the database to connect to."),
            ],
            Self::Stream => vec![
                required_input(
                    "KAFKA_BOOTSTRAP_SERVERS",
                    "The comma-separated host and port pairs of the stream's bootstrap servers.",
                ),
                allowing(
                    defaulting(
                        optional_input(
                            "KAFKA_SSL_ENABLED",
                            "Whether the connection to the bootstrap servers is encrypted.",
                        ),
                        "true",
                    ),
                    &["true", "false"],
                ),
                optional_input(
                    "KAFKA_CUSTOM_CERT",
                    "The Amazon S3 location of the certificate the bootstrap servers present.",
                ),
                optional_input(
                    "KAFKA_SASL_MECHANISM",
                    "The SASL mechanism the client authenticates to the stream with.",
                ),
            ],
            Self::FileTransfer => vec![
                required_input("HOST", "The host name of the file transfer endpoint."),
                defaulting(
                    optional_input("PORT", "The port the file transfer endpoint listens on."),
                    "22",
                ),
            ],
            Self::AwsService => vec![
                optional_input(
                    "REGION",
                    "The Region the service is reached in, when it is not the Region the job \
                     runs in.",
                ),
                optional_input(
                    "ROLE_ARN",
                    "The role a job assumes to reach the service, when it is not the job's own \
                     role.",
                ),
            ],
            Self::Network => Vec::new(),
            Self::Connector => vec![
                required_input(
                    "CONNECTOR_URL",
                    "The location of the connector this connection loads.",
                ),
                allowing(
                    required_input(
                        "CONNECTOR_TYPE",
                        "The interface the connector implements.",
                    ),
                    &["JDBC", "SPARK", "MARKETPLACE_ATHENA", "CUSTOM_ATHENA"],
                ),
                required_input(
                    "CONNECTOR_CLASS_NAME",
                    "The entry-point class of the connector this connection loads.",
                ),
            ],
            Self::RestSource => vec![
                required_input(
                    "ENDPOINT",
                    "The base URL of the data source's API.",
                ),
                optional_input(
                    "ENDPOINT_TYPE",
                    "The kind of endpoint the base URL addresses.",
                ),
                optional_input(
                    "INSTANCE_ID",
                    "The tenant of the data source this connection reaches, where the source \
                     serves more than one.",
                ),
            ],
            Self::ViewValidationRedshift => vec![
                optional_input(
                    "CLUSTER_IDENTIFIER",
                    "The provisioned cluster that runs the view's SQL.",
                ),
                optional_input(
                    "WORKGROUP_NAME",
                    "The serverless workgroup that runs the view's SQL.",
                ),
                optional_input("DATABASE", "The database the view's SQL is run against."),
            ],
            Self::ViewValidationAthena => vec![
                optional_input(
                    "WORKGROUP_NAME",
                    "The workgroup that runs the view's SQL.",
                ),
                optional_input(
                    "REGION",
                    "The Region the workgroup is reached in, when it is not the Region the \
                     validation runs in.",
                ),
            ],
        };
        if takes_stored_credentials {
            properties.push(optional_input(
                "SECRET_ID",
                "The name or ARN of the Secrets Manager secret holding this connection's \
                 credentials.",
            ));
        }
        property_map(properties)
    }

    /// The options a Spark ETL script passes a data source of this family in the connection
    /// options map, which is what `ConnectionOptions` reports. These are the script's own
    /// keys, not the create's: the model keys them with no enum.
    pub fn connection_options(self) -> HashMap<String, Property> {
        let options = match self {
            Self::GenericJdbc | Self::Relational => vec![
                scoped(
                    optional_input("dbtable", "The table in the database to read or write."),
                    &[DATA_OPERATION_READ, DATA_OPERATION_WRITE],
                ),
                scoped(
                    optional_input("query", "The query whose result set is read."),
                    &[DATA_OPERATION_READ],
                ),
            ],
            Self::DocumentStore => vec![
                required_input("database", "The database to read or write."),
                required_input("collection", "The collection to read or write."),
            ],
            Self::Stream => vec![
                required_input("topicName", "The topic to read or write."),
                allowing(
                    defaulting(
                        optional_input(
                            "startingOffsets",
                            "Where in the topic a read starts.",
                        ),
                        "latest",
                    ),
                    &["earliest", "latest"],
                ),
            ],
            Self::FileTransfer => vec![required_input(
                "path",
                "The path on the endpoint that is read or written.",
            )],
            Self::AwsService | Self::RestSource => vec![
                required_input(
                    "ENTITY_NAME",
                    "The entity of the data source that is read or written.",
                ),
                optional_input(
                    "API_VERSION",
                    "The version of the data source's API the entity is read through.",
                ),
                scoped(
                    optional_input(
                        "SELECTED_FIELDS",
                        "The fields of the entity that are read.",
                    ),
                    &[DATA_OPERATION_READ],
                ),
                scoped(
                    optional_input(
                        "FILTER_PREDICATE",
                        "The predicate rows are filtered by before they are read.",
                    ),
                    &[DATA_OPERATION_READ],
                ),
            ],
            Self::Connector => vec![required_input(
                "className",
                "The class of the connector the source is read and written through.",
            )],
            Self::Network | Self::ViewValidationRedshift | Self::ViewValidationAthena => {
                Vec::new()
            }
        };
        property_map(options)
    }

    /// One configuration per compute environment the family supports, each carrying that
    /// environment's own options. The families here override no property or option name per
    /// environment, so the override maps are empty rather than absent: an empty override is
    /// what "nothing is renamed in this environment" says.
    pub fn compute_environment_configurations(
        self,
        capabilities: &Capabilities,
        connection_options: &HashMap<String, Property>,
    ) -> HashMap<String, ComputeEnvironmentConfiguration> {
        capabilities
            .supported_compute_environments
            .iter()
            .map(|environment| {
                (
                    environment.clone(),
                    ComputeEnvironmentConfiguration {
                        name: environment.clone(),
                        description: compute_environment_description(environment).to_string(),
                        compute_environment: environment.clone(),
                        supported_authentication_types: capabilities
                            .supported_authentication_types
                            .clone(),
                        connection_options: connection_options.clone(),
                        connection_property_name_overrides: HashMap::new(),
                        connection_option_name_overrides: HashMap::new(),
                        connection_properties_required_overrides: Vec::new(),
                        physical_connection_properties_required: Some(
                            self.physical_placement_required(),
                        ),
                    },
                )
            })
            .collect()
    }

    /// Whether a create of this family MUST state where in a VPC the connection sits. A VPC
    /// path is nothing BUT its placement, where a source that can also be reached over the
    /// public API of its own endpoint leaves it optional.
    fn physical_placement_required(self) -> bool {
        matches!(self, Self::Network)
    }

    /// Where in a VPC a connection of this family sits, keyed by the members of the modelled
    /// `PhysicalConnectionRequirements` structure — the placement every connection may state,
    /// required only where the connection is nothing but that placement
    /// ([`Self::physical_placement_required`]).
    pub fn physical_connection_requirements(self) -> HashMap<String, Property> {
        let required = self.physical_placement_required();
        let describe = |name: &str, description: &str| {
            if required {
                required_input(name, description)
            } else {
                optional_input(name, description)
            }
        };
        property_map(vec![
            describe("SubnetId", "The subnet the connection is reached through."),
            describe(
                "SecurityGroupIdList",
                "The security groups the connection is reached through.",
            ),
            describe(
                "AvailabilityZone",
                "The Availability Zone the connection is reached in.",
            ),
        ])
    }
}

/// Whether credentials of one of these authentication types may be held in a Secrets
/// Manager secret: the two credential kinds the model documents a `SecretArn` as an
/// alternative to ("for supplying basic auth credentials when NOT providing a `SecretArn`
/// value" — `BasicAuthenticationCredentials`), and the OAuth2 credentials stored the same
/// way. A connection type authenticated by IAM alone holds no credential to store.
pub fn takes_stored_credentials(authentication_types: &[&str]) -> bool {
    authentication_types.iter().any(|kind| {
        [
            AUTHENTICATION_TYPE_BASIC,
            AUTHENTICATION_TYPE_OAUTH2,
            AUTHENTICATION_TYPE_CUSTOM,
        ]
        .contains(kind)
    })
}

/// How a connection that supports these authentication types supplies its credentials,
/// keyed by the members of the modelled `AuthenticationConfigurationInput` and the
/// structures beneath it, or `None` for a type that takes no credentials at all.
///
/// The descriptors reported are exactly the ones the supported types call for: the
/// authentication type itself admits those types and nothing else, and each kind's own
/// properties are reported only where that kind is one of them.
pub fn authentication_configuration(authentication_types: &[&str]) -> Option<AuthConfiguration> {
    if authentication_types.is_empty() {
        return None;
    }
    let supports = |kind: &str| authentication_types.contains(&kind);

    // Each admitted authentication type describes itself, as the modelled
    // `AllowedValue$Description` is for.
    let mut authentication_type = required_input(
        "AuthenticationType",
        "The kind of credentials this connection authenticates with.",
    );
    authentication_type.allowed_values = Some(
        authentication_types
            .iter()
            .map(|kind| AllowedValue {
                description: Some(authentication_type_description(kind).to_string()),
                value: (*kind).to_string(),
            })
            .collect(),
    );

    Some(AuthConfiguration {
        authentication_type,
        secret_arn: takes_stored_credentials(authentication_types).then(|| {
            optional_input(
                "SecretArn",
                "The ARN of the Secrets Manager secret holding this connection's credentials.",
            )
        }),
        o_auth2_properties: supports(AUTHENTICATION_TYPE_OAUTH2).then(oauth2_properties),
        basic_authentication_properties: supports(AUTHENTICATION_TYPE_BASIC)
            .then(basic_authentication_properties),
        custom_authentication_properties: supports(AUTHENTICATION_TYPE_CUSTOM)
            .then(custom_authentication_properties),
    })
}

/// What the authentication type named here is, as the value describes itself where a
/// connection type admits it.
fn authentication_type_description(kind: &str) -> &'static str {
    match kind {
        AUTHENTICATION_TYPE_BASIC => {
            "Authentication with the user name and password the data source is reached as."
        }
        AUTHENTICATION_TYPE_OAUTH2 => {
            "Authentication with an access token obtained from the provider's OAuth2 \
             authorization server."
        }
        AUTHENTICATION_TYPE_CUSTOM => {
            "Authentication with the credentials the connector itself defines the keys of."
        }
        AUTHENTICATION_TYPE_IAM => {
            "Authentication with the identity the caller's own credentials speak for, so the \
             connection holds none."
        }
        // Every value here comes from a catalog entry's own list, so no other kind can reach
        // this; a value the model adds later describes itself as authentication in general
        // rather than as one of the four above.
        _ => "Authentication with the credentials this kind calls for.",
    }
}

/// What the compute environment named here is, as its configuration describes it.
fn compute_environment_description(environment: &str) -> &'static str {
    match environment {
        COMPUTE_ENVIRONMENT_SPARK => {
            "The Glue Spark runtime, which reads and writes this connection's data source from a \
             Spark ETL job."
        }
        COMPUTE_ENVIRONMENT_PYTHON => {
            "The Glue Python shell runtime, which reads and writes this connection's data source \
             from a Python script."
        }
        COMPUTE_ENVIRONMENT_ATHENA => {
            "The Athena runtime, which queries this connection's data source through a federated \
             catalog."
        }
        // Every value here comes from a family's own list, so no other environment can
        // reach this; a value the model adds later is described as nothing in particular
        // rather than as one of the three above.
        _ => "A compute environment that reads and writes this connection's data source.",
    }
}

/// The OAuth2 configuration of a connection type that authenticates that way, keyed by the
/// members of the modelled `OAuth2PropertiesInput`, `OAuth2ClientApplication`,
/// `AuthorizationCodeProperties` and `OAuth2Credentials` — the properties a create of an
/// OAuth2 connection sets — beside the two the service itself supplies for the
/// authorization-code leg: the URL the user is sent to, and the scope the access token is
/// asked for.
///
/// A property only the AUTHORIZATION_CODE grant calls for is reported as one the caller may
/// supply, naming the grant it belongs to, because requiredness here is conditional on the
/// grant rather than on the type.
fn oauth2_properties() -> HashMap<String, Property> {
    property_map(vec![
        allowing(
            required_input(
                "OAuth2GrantType",
                "The OAuth2 grant the access token is obtained by.",
            ),
            // The modelled `OAuth2GrantType` values.
            &[
                "AUTHORIZATION_CODE",
                "CLIENT_CREDENTIALS",
                "JWT_BEARER",
                "REFRESH_TOKEN",
            ],
        ),
        required_input(
            "TokenUrl",
            "The URL of the provider's authentication server, where a token is obtained.",
        ),
        read_only_property(
            "AuthorizationCodeUrl",
            "The URL of the provider's authorization server the user is sent to, to issue an \
             authorization code for the AUTHORIZATION_CODE grant.",
        ),
        optional_secret(
            "AuthorizationCode",
            "The single-use authorization code the authorization server issued, exchanged for \
             an access token by the AUTHORIZATION_CODE grant.",
        ),
        optional_input(
            "RedirectUri",
            "The URI the authorization server returns the user to with the authorization code, \
             used again when that code is exchanged for an access token.",
        ),
        optional_input(
            "Scope",
            "The scopes the access token is requested with, as the provider names them.",
        ),
        optional_input(
            "UserManagedClientApplicationClientId",
            "The client id of the client application, when the caller manages it.",
        ),
        optional_secret(
            "UserManagedClientApplicationClientSecret",
            "The client secret of the client application, when the caller manages it.",
        ),
        read_only_property(
            "AWSManagedClientApplicationReference",
            "The reference to the client application registered with the provider on this \
             service's side, used when the caller manages none of its own.",
        ),
    ])
}

/// The basic-authentication configuration of a family that authenticates that way, keyed by
/// the members of the modelled `BasicAuthenticationCredentials`.
fn basic_authentication_properties() -> HashMap<String, Property> {
    property_map(vec![
        required_input("Username", "The user name the data source is reached as."),
        secret_property("Password", "The password of that user."),
    ])
}

/// The custom-authentication configuration of a family that authenticates that way: the
/// credential map the connector itself defines the keys of
/// (`AuthenticationConfigurationInput$CustomAuthenticationCredentials`), so the one
/// property reported is that map.
fn custom_authentication_properties() -> HashMap<String, Property> {
    property_map(vec![secret_property(
        "CustomAuthenticationCredentials",
        "The credentials the connector itself defines the keys of.",
    )])
}

/// A property the caller must supply.
fn required_input(name: &str, description: &str) -> Property {
    property(name, description, true, PROPERTY_TYPE_USER_INPUT)
}

/// A property the caller may supply.
fn optional_input(name: &str, description: &str) -> Property {
    property(name, description, false, PROPERTY_TYPE_USER_INPUT)
}

/// A credential: required, and either held on the connection or in the secret the
/// connection names, which is the pair of places the model documents for one.
fn secret_property(name: &str, description: &str) -> Property {
    let mut property = property(name, description, true, PROPERTY_TYPE_SECRET);
    property
        .property_types
        .push(PROPERTY_TYPE_SECRET_OR_USER_INPUT.to_string());
    property
}

/// The same credential, where supplying it is conditional rather than required: the caller
/// supplies it only for the grant or the client application it belongs to.
fn optional_secret(name: &str, description: &str) -> Property {
    let mut property = secret_property(name, description);
    property.required = false;
    property
}

/// A property the SERVICE supplies and the caller only reads
/// ([`PROPERTY_TYPE_READ_ONLY`]).
fn read_only_property(name: &str, description: &str) -> Property {
    property(name, description, false, PROPERTY_TYPE_READ_ONLY)
}

/// The `JDBC_ENFORCE_SSL` property, which both JDBC-reached families carry.
fn enforce_ssl_property() -> Property {
    allowing(
        defaulting(
            optional_input(
                "JDBC_ENFORCE_SSL",
                "Whether the JDBC connection to the data store is encrypted.",
            ),
            "false",
        ),
        &["true", "false"],
    )
}

/// One property of a connection type.
fn property(name: &str, description: &str, required: bool, property_type: &str) -> Property {
    Property {
        name: name.to_string(),
        description: description.to_string(),
        required,
        default_value: None,
        property_types: vec![property_type.to_string()],
        allowed_values: None,
        data_operation_scopes: None,
        key_override: None,
        property_location: None,
    }
}

/// The same property, with the values it admits.
fn allowing(mut property: Property, values: &[&str]) -> Property {
    property.allowed_values = Some(
        values
            .iter()
            .map(|value| AllowedValue {
                description: None,
                value: value.to_string(),
            })
            .collect(),
    );
    property
}

/// The same property, with the value it takes when the caller sets none.
fn defaulting(mut property: Property, value: &str) -> Property {
    property.default_value = Some(value.to_string());
    property
}

/// The same property, narrowed to the data operations it applies to.
fn scoped(mut property: Property, operations: &[&str]) -> Property {
    property.data_operation_scopes =
        Some(operations.iter().map(|value| value.to_string()).collect());
    property
}

/// A properties map, keyed by each property's own name — which is how the create and the
/// script that read these maps address a property.
fn property_map(properties: Vec<Property>) -> HashMap<String, Property> {
    properties
        .into_iter()
        .map(|property| (property.name.clone(), property))
        .collect()
}

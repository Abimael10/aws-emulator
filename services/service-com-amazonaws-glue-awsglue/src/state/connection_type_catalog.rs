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
//! What a type is configured WITH is read off that type's connectivity FAMILY
//! ([`ConnectionFamily`]) instead of being recorded type by type: the properties a
//! relational engine is reached by are the same properties whichever engine it is, and the
//! model's own key spaces state which keys they are — a connection's properties are keyed
//! by `ConnectionPropertyKey` (the key of the `ConnectionProperties` map `CreateConnection`
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
    CONNECTION_TYPE_VALUES,
};

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

/// How a connection of some type REACHES its data source. Every connection type the
/// service serves belongs to exactly one family, and the family decides the properties,
/// the options, the authentication and the compute environments the type is described
/// with — the per-source connectors differ in which source they talk to, not in how a
/// caller configures them.
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

/// The family `connection_type` belongs to, or `None` when it is no connection type this
/// service serves.
///
/// The exceptions are named one by one and every other value of the modelled enum is a
/// REST source, because that is what the bulk of the per-source connectors are: a SaaS
/// application Glue reads through its own API.
pub fn connection_type_family(connection_type: &str) -> Option<ConnectionFamily> {
    if !CONNECTION_TYPE_VALUES.contains(&connection_type) {
        return None;
    }
    Some(match connection_type {
        "JDBC" => ConnectionFamily::GenericJdbc,
        "MYSQL" | "POSTGRESQL" | "ORACLE" | "SQLSERVER" | "SAPHANA" | "TERADATA"
        | "TERADATANOS" | "VERTICA" | "DB2" | "DB2AS400" | "SYNAPSE" | "AZURESQL"
        | "CLOUDERAHIVE" | "CLOUDERAIMPALA" | "HBASE" | "TPCDS" => ConnectionFamily::Relational,
        "MONGODB" | "DOCUMENTDB" | "AZURECOSMOS" => ConnectionFamily::DocumentStore,
        "KAFKA" => ConnectionFamily::Stream,
        "SFTP" => ConnectionFamily::FileTransfer,
        "DYNAMODB" | "CLOUDWATCH" | "CLOUDWATCHMETRICS" | "TIMESTREAM" => {
            ConnectionFamily::AwsService
        }
        "NETWORK" => ConnectionFamily::Network,
        "MARKETPLACE" | "CUSTOM" => ConnectionFamily::Connector,
        "VIEW_VALIDATION_REDSHIFT" => ConnectionFamily::ViewValidationRedshift,
        "VIEW_VALIDATION_ATHENA" => ConnectionFamily::ViewValidationAthena,
        _ => ConnectionFamily::RestSource,
    })
}

/// Everything `DescribeConnectionType` reports about one connection type.
#[derive(Debug, Clone)]
pub struct ConnectionTypeDetails {
    /// The type described, as the request named it.
    pub connection_type: String,
    /// What a connection of this type connects to.
    pub description: String,
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
    /// Where in a VPC the connection sits, for a type that is reached inside one.
    pub physical_connection_requirements: Option<HashMap<String, Property>>,
}

/// Everything the service knows about `connection_type`, or `None` when it serves no such
/// connection type.
pub fn describe_connection_type(connection_type: &str) -> Option<ConnectionTypeDetails> {
    let family = connection_type_family(connection_type)?;
    let capabilities = family.capabilities();
    let connection_options = family.connection_options();
    let compute_environment_configurations =
        family.compute_environment_configurations(&capabilities, &connection_options);
    Some(ConnectionTypeDetails {
        connection_type: connection_type.to_string(),
        description: family.describe(connection_type),
        capabilities,
        connection_properties: family.connection_properties(),
        connection_options,
        authentication_configuration: family.authentication_configuration(),
        compute_environment_configurations,
        physical_connection_requirements: family.physical_connection_requirements(),
    })
}

impl ConnectionFamily {
    /// What a connection of this family connects to, naming the type it was asked about:
    /// the family states the shape of the source and the type states which source it is.
    pub fn describe(self, connection_type: &str) -> String {
        match self {
            Self::GenericJdbc => "A connection to a data store reached over JDBC, configured \
                 with the JDBC connection URL of that store."
                .to_string(),
            Self::Relational => format!(
                "A connection to a {connection_type} database, reached over JDBC by host, port \
                 and database name."
            ),
            Self::DocumentStore => format!(
                "A connection to a {connection_type} document store, reached by its connection \
                 URL."
            ),
            Self::Stream => format!(
                "A connection to a {connection_type} record stream, reached by its bootstrap \
                 servers."
            ),
            Self::FileTransfer => format!(
                "A connection to a {connection_type} file transfer endpoint, reached by host and \
                 port."
            ),
            Self::AwsService => format!(
                "A connection to {connection_type}, reached with the identity the caller's \
                 credentials speak for."
            ),
            Self::Network => "A connection to the resources of a VPC, carrying no data source of \
                 its own: what it configures is the subnet and the security groups a job reaches \
                 them through."
                .to_string(),
            Self::Connector => format!(
                "A connection to a data store served by a {connection_type} connector, which \
                 brings the driver the store is read and written through."
            ),
            Self::RestSource => format!(
                "A connection to the {connection_type} data source, read through its own REST \
                 API."
            ),
            Self::ViewValidationRedshift => {
                "A connection used to validate the Amazon Redshift dialect of a catalog view's \
                 SQL against the cluster or workgroup that runs it."
                    .to_string()
            }
            Self::ViewValidationAthena => {
                "A connection used to validate the Athena dialect of a catalog view's SQL against \
                 the workgroup that runs it."
                    .to_string()
            }
        }
    }

    /// The authentication types a connection of this family may be configured with, in the
    /// order the model declares them. A family whose source carries no credentials at all
    /// supports none.
    pub fn supported_authentication_types(self) -> Vec<String> {
        let types: &[&str] = match self {
            Self::GenericJdbc | Self::Relational | Self::DocumentStore | Self::FileTransfer => {
                &[AUTHENTICATION_TYPE_BASIC]
            }
            Self::Stream | Self::Connector => &[
                AUTHENTICATION_TYPE_BASIC,
                AUTHENTICATION_TYPE_CUSTOM,
                AUTHENTICATION_TYPE_IAM,
            ],
            Self::AwsService | Self::ViewValidationAthena => &[AUTHENTICATION_TYPE_IAM],
            Self::ViewValidationRedshift => &[AUTHENTICATION_TYPE_BASIC, AUTHENTICATION_TYPE_IAM],
            Self::RestSource => &[
                AUTHENTICATION_TYPE_BASIC,
                AUTHENTICATION_TYPE_OAUTH2,
                AUTHENTICATION_TYPE_CUSTOM,
            ],
            Self::Network => &[],
        };
        types.iter().map(|value| value.to_string()).collect()
    }

    /// The data operations a job may run against a source of this family. A REST source is
    /// read; a store Glue has a driver for is read and written; a VPC path carries no data
    /// of its own, so neither.
    fn supported_data_operations(self) -> Vec<String> {
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
    fn supported_compute_environments(self) -> Vec<String> {
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

    /// The capabilities of a connection of this family, as `DescribeConnectionType` and
    /// `ListConnectionTypes` both report them.
    pub fn capabilities(self) -> Capabilities {
        Capabilities {
            supported_authentication_types: self.supported_authentication_types(),
            supported_data_operations: self.supported_data_operations(),
            supported_compute_environments: self.supported_compute_environments(),
        }
    }

    /// The properties a create of a connection of this family sets, keyed as the create
    /// keys them: by a value of the modelled `ConnectionPropertyKey` enum, which is the key
    /// of `ConnectionInput$ConnectionProperties`.
    pub fn connection_properties(self) -> HashMap<String, Property> {
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
        // Credentials this family takes may be held in Secrets Manager instead of on the
        // connection, which is one more property of the create wherever that is so.
        if self.takes_stored_credentials() {
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

    /// Whether credentials of a family this supports may be held in a Secrets Manager
    /// secret: the two credential kinds the model documents a `SecretArn` as an alternative
    /// to ("for supplying basic auth credentials when NOT providing a `SecretArn` value" —
    /// `BasicAuthenticationCredentials`), and the OAuth2 credentials stored the same way. A
    /// family authenticated by IAM alone holds no credential to store.
    fn takes_stored_credentials(self) -> bool {
        let supported = self.supported_authentication_types();
        [
            AUTHENTICATION_TYPE_BASIC,
            AUTHENTICATION_TYPE_OAUTH2,
            AUTHENTICATION_TYPE_CUSTOM,
        ]
        .iter()
        .any(|kind| supported.iter().any(|value| value == kind))
    }

    /// How a connection of this family supplies credentials, keyed by the members of the
    /// modelled `AuthenticationConfigurationInput` and the structures beneath it, or `None`
    /// for a family that takes no credentials at all.
    pub fn authentication_configuration(self) -> Option<AuthConfiguration> {
        let supported = self.supported_authentication_types();
        if supported.is_empty() {
            return None;
        }
        let supports = |kind: &str| supported.iter().any(|value| value == kind);

        let authentication_type = allowing(
            required_input(
                "AuthenticationType",
                "The kind of credentials this connection authenticates with.",
            ),
            &supported
                .iter()
                .map(String::as_str)
                .collect::<Vec<&str>>(),
        );

        Some(AuthConfiguration {
            authentication_type,
            secret_arn: self.takes_stored_credentials().then(|| {
                optional_input(
                    "SecretArn",
                    "The ARN of the Secrets Manager secret holding this connection's \
                     credentials.",
                )
            }),
            o_auth2_properties: supports(AUTHENTICATION_TYPE_OAUTH2).then(oauth2_properties),
            basic_authentication_properties: supports(AUTHENTICATION_TYPE_BASIC)
                .then(basic_authentication_properties),
            custom_authentication_properties: supports(AUTHENTICATION_TYPE_CUSTOM)
                .then(custom_authentication_properties),
        })
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
                        physical_connection_properties_required: self
                            .physical_placement_required(),
                    },
                )
            })
            .collect()
    }

    /// Whether a create of this family MUST state where in a VPC the connection sits, or
    /// `None` for a family reached over the public API of its source, which sits in no VPC
    /// of the caller's and so states no placement at all. A VPC path is nothing BUT its
    /// placement, where a source that can also be reached without one leaves it optional.
    fn physical_placement_required(self) -> Option<bool> {
        match self {
            Self::Network => Some(true),
            Self::GenericJdbc
            | Self::Relational
            | Self::DocumentStore
            | Self::Stream
            | Self::FileTransfer
            | Self::Connector => Some(false),
            Self::AwsService
            | Self::RestSource
            | Self::ViewValidationRedshift
            | Self::ViewValidationAthena => None,
        }
    }

    /// Where in a VPC a connection of this family sits, keyed by the members of the
    /// modelled `PhysicalConnectionRequirements` structure, or `None` for a family that
    /// states no placement ([`Self::physical_placement_required`]).
    pub fn physical_connection_requirements(self) -> Option<HashMap<String, Property>> {
        let required = self.physical_placement_required()?;
        let describe = |name: &str, description: &str| {
            if required {
                required_input(name, description)
            } else {
                optional_input(name, description)
            }
        };
        Some(property_map(vec![
            describe("SubnetId", "The subnet the connection is reached through."),
            describe(
                "SecurityGroupIdList",
                "The security groups the connection is reached through.",
            ),
            describe(
                "AvailabilityZone",
                "The Availability Zone the connection is reached in.",
            ),
        ]))
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

/// The OAuth2 configuration of a family that authenticates that way, keyed by the members
/// of the modelled `OAuth2PropertiesInput`, `OAuth2ClientApplication` and
/// `OAuth2Credentials`.
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
        optional_input(
            "UserManagedClientApplicationClientId",
            "The client id of the client application, when the caller manages it.",
        ),
        secret_property(
            "UserManagedClientApplicationClientSecret",
            "The client secret of the client application, when the caller manages it.",
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

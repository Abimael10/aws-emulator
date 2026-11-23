// SPDX-License-Identifier: CC0-1.0

//! The catalog ENTRIES themselves: one row per connection type the service serves.
//!
//! `DescribeConnectionType` is a read of this catalog, so what it answers for a type is
//! the row recorded here for that type and nothing else — two types reached the same way
//! still differ in what the service publishes about them, and a name with no row is no
//! connection type at all.
//!
//! A row carries the type's own data: the data source it reaches (as the catalog names the
//! product, which is what the type's description says it connects to), the authentication
//! types a connection of it may be created with (the values of the modelled
//! `AuthenticationType` enum, which is the key space of
//! `AuthenticationConfigurationInput$AuthenticationType`), and the connectivity
//! [`ConnectionFamily`] the type is reached by — the family is what decides the property,
//! option and physical-placement descriptors, because the model's own key spaces
//! (`ConnectionPropertyKey`, the members of `PhysicalConnectionRequirements`) state those
//! per way of reaching a source rather than per product.
//!
//! The rows are the values of the modelled `ConnectionType` enum
//! ([`crate::ops::types::CONNECTION_TYPE_VALUES`]), which is the key space of
//! `ConnectionInput$ConnectionType`: a type a connection can be created with is a type this
//! read describes, and the two sets are held equal by the crate's tests. The authentication
//! types of a row are the ones the connector that reaches that product authenticates with:
//! a SaaS source whose vendor runs an OAuth2 authorization server is reached with `OAUTH2`,
//! one reached with an account's user name and password or API key pair with `BASIC`, one
//! reached with a vendor API token with `CUSTOM`, and an Amazon Web Services service with
//! the caller's own `IAM` identity.

use super::*;

/// A connector reached with the OAuth2 authorization server the vendor runs.
const OAUTH2_ONLY: &[&str] = &[AUTHENTICATION_TYPE_OAUTH2];
/// A connector reached either with an account's own credentials or through the vendor's
/// OAuth2 authorization server.
const BASIC_OR_OAUTH2: &[&str] = &[AUTHENTICATION_TYPE_BASIC, AUTHENTICATION_TYPE_OAUTH2];
/// A connector reached with an account's user name and password, or with an API key pair
/// supplied the same way.
const BASIC_ONLY: &[&str] = &[AUTHENTICATION_TYPE_BASIC];
/// A connector reached with a vendor API token, whose keys the connector itself defines.
const API_TOKEN: &[&str] = &[AUTHENTICATION_TYPE_CUSTOM];
/// A store Glue reaches with a driver of its own, which may be configured with an account's
/// credentials, with credentials the connector defines, or with the caller's identity.
const DRIVER_SUPPLIED: &[&str] = &[
    AUTHENTICATION_TYPE_BASIC,
    AUTHENTICATION_TYPE_CUSTOM,
    AUTHENTICATION_TYPE_IAM,
];
/// An Amazon Web Services service, reached with the identity the caller's credentials speak
/// for, so the connection carries no credentials of its own.
const IAM_ONLY: &[&str] = &[AUTHENTICATION_TYPE_IAM];
/// Amazon Redshift, which runs a view's SQL either as a database user or as the caller.
const BASIC_OR_IAM: &[&str] = &[AUTHENTICATION_TYPE_BASIC, AUTHENTICATION_TYPE_IAM];
/// A path into a VPC, which reaches no data source of its own and so takes no credentials.
const NO_CREDENTIALS: &[&str] = &[];

/// One row of the service's connection-type catalog: everything the service publishes about
/// ONE connection type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionTypeEntry {
    /// The type this row is the catalog entry of — a value of the modelled `ConnectionType`
    /// enum.
    pub connection_type: &'static str,
    /// The data source a connection of this type reaches, as the catalog names the product.
    pub source: &'static str,
    /// How a connection of this type reaches that source, which decides its properties,
    /// options and physical placement.
    pub family: ConnectionFamily,
    /// The authentication types a connection of this type may be created with, in the order
    /// the modelled `AuthenticationType` enum declares them.
    pub authentication_types: &'static [&'static str],
}

/// The catalog entry of `connection_type`, or `None` when the service serves no such
/// connection type.
pub fn connection_type_entry(connection_type: &str) -> Option<&'static ConnectionTypeEntry> {
    CONNECTION_TYPE_CATALOG
        .iter()
        .find(|entry| entry.connection_type == connection_type)
}

/// One row per connection type the service serves, in the order the modelled
/// `ConnectionType` enum declares them.
pub const CONNECTION_TYPE_CATALOG: &[ConnectionTypeEntry] = &[
    entry("JDBC", "JDBC", ConnectionFamily::GenericJdbc, BASIC_ONLY),
    entry("SFTP", "SFTP", ConnectionFamily::FileTransfer, BASIC_ONLY),
    entry(
        "MONGODB",
        "MongoDB",
        ConnectionFamily::DocumentStore,
        BASIC_ONLY,
    ),
    entry(
        "KAFKA",
        "Apache Kafka",
        ConnectionFamily::Stream,
        DRIVER_SUPPLIED,
    ),
    entry(
        "NETWORK",
        "the resources of a VPC",
        ConnectionFamily::Network,
        NO_CREDENTIALS,
    ),
    entry(
        "MARKETPLACE",
        "a connector from Amazon Web Services Marketplace",
        ConnectionFamily::Connector,
        DRIVER_SUPPLIED,
    ),
    entry(
        "CUSTOM",
        "a connector the caller supplied",
        ConnectionFamily::Connector,
        DRIVER_SUPPLIED,
    ),
    entry(
        "SALESFORCE",
        "Salesforce",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "VIEW_VALIDATION_REDSHIFT",
        "Amazon Redshift",
        ConnectionFamily::ViewValidationRedshift,
        BASIC_OR_IAM,
    ),
    entry(
        "VIEW_VALIDATION_ATHENA",
        "Amazon Athena",
        ConnectionFamily::ViewValidationAthena,
        IAM_ONLY,
    ),
    entry(
        "GOOGLEADS",
        "Google Ads",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "GOOGLESHEETS",
        "Google Sheets",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "GOOGLEANALYTICS4",
        "Google Analytics 4",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "SERVICENOW",
        "ServiceNow",
        ConnectionFamily::RestSource,
        BASIC_OR_OAUTH2,
    ),
    entry(
        "MARKETO",
        "Adobe Marketo Engage",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "SAPODATA",
        "SAP OData",
        ConnectionFamily::RestSource,
        BASIC_OR_OAUTH2,
    ),
    entry(
        "ZENDESK",
        "Zendesk",
        ConnectionFamily::RestSource,
        BASIC_OR_OAUTH2,
    ),
    entry(
        "JIRACLOUD",
        "Jira Cloud",
        ConnectionFamily::RestSource,
        BASIC_OR_OAUTH2,
    ),
    entry(
        "NETSUITEERP",
        "NetSuite ERP",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "HUBSPOT",
        "HubSpot",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "FACEBOOKADS",
        "Facebook Ads",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "INSTAGRAMADS",
        "Instagram Ads",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "ZOHOCRM",
        "Zoho CRM",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "SALESFORCEPARDOT",
        "Salesforce Pardot",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "SALESFORCEMARKETINGCLOUD",
        "Salesforce Marketing Cloud",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "ADOBEANALYTICS",
        "Adobe Analytics",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry("SLACK", "Slack", ConnectionFamily::RestSource, OAUTH2_ONLY),
    entry(
        "LINKEDIN",
        "LinkedIn",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "MIXPANEL",
        "Mixpanel",
        ConnectionFamily::RestSource,
        BASIC_ONLY,
    ),
    entry("ASANA", "Asana", ConnectionFamily::RestSource, OAUTH2_ONLY),
    entry("STRIPE", "Stripe", ConnectionFamily::RestSource, BASIC_ONLY),
    entry(
        "SMARTSHEET",
        "Smartsheet",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry("DATADOG", "Datadog", ConnectionFamily::RestSource, API_TOKEN),
    entry(
        "WOOCOMMERCE",
        "WooCommerce",
        ConnectionFamily::RestSource,
        BASIC_ONLY,
    ),
    entry(
        "INTERCOM",
        "Intercom",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "SNAPCHATADS",
        "Snapchat Ads",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "PAYPAL",
        "PayPal",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "QUICKBOOKS",
        "QuickBooks",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "FACEBOOKPAGEINSIGHTS",
        "Facebook Page Insights",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "FRESHDESK",
        "Freshdesk",
        ConnectionFamily::RestSource,
        BASIC_ONLY,
    ),
    entry("TWILIO", "Twilio", ConnectionFamily::RestSource, BASIC_ONLY),
    entry(
        "DOCUSIGNMONITOR",
        "Docusign Monitor",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "FRESHSALES",
        "Freshsales",
        ConnectionFamily::RestSource,
        API_TOKEN,
    ),
    entry("ZOOM", "Zoom", ConnectionFamily::RestSource, OAUTH2_ONLY),
    entry(
        "GOOGLESEARCHCONSOLE",
        "Google Search Console",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "SALESFORCECOMMERCECLOUD",
        "Salesforce Commerce Cloud",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "SAPCONCUR",
        "SAP Concur",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "DYNATRACE",
        "Dynatrace",
        ConnectionFamily::RestSource,
        API_TOKEN,
    ),
    entry(
        "MICROSOFTDYNAMIC365FINANCEANDOPS",
        "Microsoft Dynamics 365 Finance and Operations",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "MICROSOFTTEAMS",
        "Microsoft Teams",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "BLACKBAUDRAISEREDGENXT",
        "Blackbaud Raiser's Edge NXT",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "MAILCHIMP",
        "Mailchimp",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "GITLAB",
        "GitLab",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry("PENDO", "Pendo", ConnectionFamily::RestSource, API_TOKEN),
    entry(
        "PRODUCTBOARD",
        "Productboard",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "CIRCLECI",
        "CircleCI",
        ConnectionFamily::RestSource,
        API_TOKEN,
    ),
    entry(
        // The modelled `ConnectionType` enum declares this spelling BESIDE `PIPEDRIVE`, so the
        // catalog records both: the key space of `ConnectionInput$ConnectionType` is what a
        // connection may be created with, and a type a create admits is a type this read
        // describes.
        "PIPEDIVE",
        "Pipedrive",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "SENDGRID",
        "Twilio SendGrid",
        ConnectionFamily::RestSource,
        API_TOKEN,
    ),
    entry(
        "AZURECOSMOS",
        "Azure Cosmos DB",
        ConnectionFamily::DocumentStore,
        BASIC_ONLY,
    ),
    entry(
        "AZURESQL",
        "Azure SQL",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "BIGQUERY",
        "Google BigQuery",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "BLACKBAUD",
        "Blackbaud",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "CLOUDERAHIVE",
        "Cloudera Hive",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "CLOUDERAIMPALA",
        "Cloudera Impala",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "CLOUDWATCH",
        "Amazon CloudWatch Logs",
        ConnectionFamily::AwsService,
        IAM_ONLY,
    ),
    entry(
        "CLOUDWATCHMETRICS",
        "Amazon CloudWatch metrics",
        ConnectionFamily::AwsService,
        IAM_ONLY,
    ),
    entry(
        "CMDB",
        "ServiceNow CMDB",
        ConnectionFamily::RestSource,
        BASIC_OR_OAUTH2,
    ),
    entry(
        "DATALAKEGEN2",
        "Azure Data Lake Storage Gen2",
        ConnectionFamily::RestSource,
        API_TOKEN,
    ),
    entry("DB2", "IBM Db2", ConnectionFamily::Relational, BASIC_ONLY),
    entry(
        "DB2AS400",
        "IBM Db2 for i",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "DOCUMENTDB",
        "Amazon DocumentDB",
        ConnectionFamily::DocumentStore,
        BASIC_ONLY,
    ),
    entry("DOMO", "Domo", ConnectionFamily::RestSource, OAUTH2_ONLY),
    entry(
        "DYNAMODB",
        "Amazon DynamoDB",
        ConnectionFamily::AwsService,
        IAM_ONLY,
    ),
    entry(
        "GOOGLECLOUDSTORAGE",
        "Google Cloud Storage",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "HBASE",
        "Apache HBase",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "KUSTOMER",
        "Kustomer",
        ConnectionFamily::RestSource,
        API_TOKEN,
    ),
    entry(
        "MICROSOFTDYNAMICS365CRM",
        "Microsoft Dynamics 365 CRM",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "MONDAY",
        "monday.com",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry("MYSQL", "MySQL", ConnectionFamily::Relational, BASIC_ONLY),
    entry("OKTA", "Okta", ConnectionFamily::RestSource, API_TOKEN),
    entry(
        "OPENSEARCH",
        "OpenSearch",
        ConnectionFamily::RestSource,
        BASIC_ONLY,
    ),
    entry(
        "ORACLE",
        "Oracle Database",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "PIPEDRIVE",
        "Pipedrive",
        ConnectionFamily::RestSource,
        OAUTH2_ONLY,
    ),
    entry(
        "POSTGRESQL",
        "PostgreSQL",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "SAPHANA",
        "SAP HANA",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "SQLSERVER",
        "Microsoft SQL Server",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "SYNAPSE",
        "Azure Synapse Analytics",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "TERADATA",
        "Teradata",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "TERADATANOS",
        "Teradata Native Object Store",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry(
        "TIMESTREAM",
        "Amazon Timestream",
        ConnectionFamily::AwsService,
        IAM_ONLY,
    ),
    entry(
        "TPCDS",
        "the TPC-DS benchmark data set",
        ConnectionFamily::Relational,
        BASIC_ONLY,
    ),
    entry("VERTICA", "Vertica", ConnectionFamily::Relational, BASIC_ONLY),
];

/// One catalog row, written out so the table above reads as data.
const fn entry(
    connection_type: &'static str,
    source: &'static str,
    family: ConnectionFamily,
    authentication_types: &'static [&'static str],
) -> ConnectionTypeEntry {
    ConnectionTypeEntry {
        connection_type,
        source,
        family,
        authentication_types,
    }
}

// SPDX-License-Identifier: CC0-1.0

//! The SOURCE KIND a data source's `DataSourceParameters` describe, and the refusal a
//! write whose parameters do not describe the kind the data source IS.
//!
//! `DataSourceParameters` is a variant type: each member is the parameter set of ONE
//! kind of source, and `DataSourceType` names that kind. The two state the same thing
//! about one data source, so QuickSight does not register a data source whose parameter
//! set belongs to a kind other than its type — a create or update that submits one is
//! refused synchronously with `InvalidParameterValueException` naming the member that was
//! supplied and the kind it belongs to, and nothing is written.
//!
//! The pairing below is the model's own: every `DataSourceParameters` member whose
//! parameter set is the one of a member of the `DataSourceType` enum
//! (`smithy_query(query="shape", shape="DataSourceParameters,DataSourceType",
//! model_path="services/service-com-amazonaws-quicksight-quicksight-20180401/smithy-model/com-amazonaws-quicksight-quicksight-20180401.json")`).
//! Four members of the union name a kind the enum has no member for at all —
//! `RdsParameters`, `ImpalaParameters`, `CustomConnectionParameters` and
//! `FMKBParameters` — so there is no type for such a parameter set to disagree with and
//! a request carrying one is not refused here. A write that submits no parameters at all
//! describes no connection, so there is nothing to cross-check either.

use crate::ops::types::DataSourceParameters;

/// The union member name a parameter set is submitted under, and the `DataSourceType`
/// member whose sources it configures — `None` for a parameter set the enum names no
/// kind for.
///
/// The member name is the model's, which is also the name the wire document carries, so
/// a refusal names the member the request itself used.
fn parameter_set(parameters: &DataSourceParameters) -> (&'static str, Option<&'static str>) {
    match parameters {
        DataSourceParameters::AmazonElasticsearchParameters(_) => (
            "AmazonElasticsearchParameters",
            Some("AMAZON_ELASTICSEARCH"),
        ),
        DataSourceParameters::AthenaParameters(_) => ("AthenaParameters", Some("ATHENA")),
        DataSourceParameters::AuroraParameters(_) => ("AuroraParameters", Some("AURORA")),
        DataSourceParameters::AuroraPostgreSqlParameters(_) => {
            ("AuroraPostgreSqlParameters", Some("AURORA_POSTGRESQL"))
        }
        DataSourceParameters::AwsIotAnalyticsParameters(_) => {
            ("AwsIotAnalyticsParameters", Some("AWS_IOT_ANALYTICS"))
        }
        DataSourceParameters::JiraParameters(_) => ("JiraParameters", Some("JIRA")),
        DataSourceParameters::MariaDbParameters(_) => ("MariaDbParameters", Some("MARIADB")),
        DataSourceParameters::MySqlParameters(_) => ("MySqlParameters", Some("MYSQL")),
        DataSourceParameters::OracleParameters(_) => ("OracleParameters", Some("ORACLE")),
        DataSourceParameters::PostgreSqlParameters(_) => {
            ("PostgreSqlParameters", Some("POSTGRESQL"))
        }
        DataSourceParameters::PrestoParameters(_) => ("PrestoParameters", Some("PRESTO")),
        // The enum names no RDS kind: an RDS instance is reached as the engine it runs
        // (`AURORA`, `MARIADB`, `MYSQL`, `POSTGRESQL`, `SQLSERVER`), so this parameter
        // set belongs to no one type.
        DataSourceParameters::RdsParameters(_) => ("RdsParameters", None),
        DataSourceParameters::RedshiftParameters(_) => ("RedshiftParameters", Some("REDSHIFT")),
        DataSourceParameters::S3Parameters(_) => ("S3Parameters", Some("S3")),
        DataSourceParameters::S3TablesParameters(_) => ("S3TablesParameters", Some("S3_TABLES")),
        DataSourceParameters::S3KnowledgeBaseParameters(_) => {
            ("S3KnowledgeBaseParameters", Some("S3_KNOWLEDGE_BASE"))
        }
        DataSourceParameters::ServiceNowParameters(_) => {
            ("ServiceNowParameters", Some("SERVICENOW"))
        }
        DataSourceParameters::SnowflakeParameters(_) => ("SnowflakeParameters", Some("SNOWFLAKE")),
        DataSourceParameters::SparkParameters(_) => ("SparkParameters", Some("SPARK")),
        DataSourceParameters::SqlServerParameters(_) => ("SqlServerParameters", Some("SQLSERVER")),
        DataSourceParameters::TeradataParameters(_) => ("TeradataParameters", Some("TERADATA")),
        DataSourceParameters::TwitterParameters(_) => ("TwitterParameters", Some("TWITTER")),
        DataSourceParameters::AmazonOpenSearchParameters(_) => {
            ("AmazonOpenSearchParameters", Some("AMAZON_OPENSEARCH"))
        }
        DataSourceParameters::ExasolParameters(_) => ("ExasolParameters", Some("EXASOL")),
        DataSourceParameters::DatabricksParameters(_) => {
            ("DatabricksParameters", Some("DATABRICKS"))
        }
        DataSourceParameters::StarburstParameters(_) => ("StarburstParameters", Some("STARBURST")),
        DataSourceParameters::TrinoParameters(_) => ("TrinoParameters", Some("TRINO")),
        DataSourceParameters::BigQueryParameters(_) => ("BigQueryParameters", Some("BIGQUERY")),
        // Three more parameter sets the enum names no kind for.
        DataSourceParameters::ImpalaParameters(_) => ("ImpalaParameters", None),
        DataSourceParameters::CustomConnectionParameters(_) => {
            ("CustomConnectionParameters", None)
        }
        DataSourceParameters::FMKBParameters(_) => ("FMKBParameters", None),
        DataSourceParameters::WebCrawlerParameters(_) => {
            ("WebCrawlerParameters", Some("WEB_CRAWLER"))
        }
        DataSourceParameters::ConfluenceParameters(_) => {
            ("ConfluenceParameters", Some("CONFLUENCE"))
        }
        DataSourceParameters::QBusinessParameters(_) => ("QBusinessParameters", Some("QBUSINESS")),
        DataSourceParameters::SharePointParameters(_) => {
            ("SharePointParameters", Some("SHAREPOINT"))
        }
        DataSourceParameters::GoogleDriveParameters(_) => {
            ("GoogleDriveParameters", Some("GOOGLE_DRIVE"))
        }
        DataSourceParameters::OneDriveParameters(_) => ("OneDriveParameters", Some("ONE_DRIVE")),
    }
}

/// The refusal a write whose parameter set belongs to a kind other than
/// `data_source_type` reports: fixed text around the union member the request supplied
/// and the kind that member configures, with no trailing period.
pub fn mismatched_parameters_message(member: &str, owning_type: &str) -> String {
    format!("{member} should only be set for {owning_type} source")
}

/// The message refusing `parameters` as a parameter set a data source of
/// `data_source_type` cannot have, or `None` when the write may go on.
///
/// `None` as well when the write submits no parameters, and when the parameter set
/// belongs to no member of the type enum, so nothing states which kind it configures.
pub fn mismatched_parameters(
    parameters: Option<&DataSourceParameters>,
    data_source_type: &str,
) -> Option<String> {
    let (member, owning_type) = parameter_set(parameters?);
    let owning_type = owning_type?;
    (owning_type != data_source_type)
        .then(|| mismatched_parameters_message(member, owning_type))
}

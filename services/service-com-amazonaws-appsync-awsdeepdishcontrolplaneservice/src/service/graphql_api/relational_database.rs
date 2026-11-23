// SPDX-License-Identifier: CC0-1.0

//! What a resolver step whose data source is of the `RELATIONAL_DATABASE` type DOES: it RUNS
//! the statements its request half returned against the database that data source was
//! configured with, through the HTTP endpoint of the cluster holding it — the Amazon RDS Data
//! API — and the result its response half is handed is what that API answered.
//!
//! "The relational database source type is an Amazon RDS HTTP endpoint"
//! (`RelationalDatabaseDataSourceConfig$relationalDatabaseSourceType`), and the endpoint
//! settings beside it carry exactly what a Data API call carries: the cluster's own ARN
//! (`rdsHttpEndpointConfig.dbClusterIdentifier`), the secret holding the login
//! (`awsSecretStoreArn`) and the database to run against (`databaseName`). So a step of such
//! a data source makes the call any client of that cluster makes.
//!
//! The request half returns AppSync's relational-database request object — the `statements`
//! to run, the `variableMap` of the values their placeholders stand for and the
//! `variableTypeHintMap` of the types those values are sent as — which an APPSYNC_JS resolver
//! builds with the statement builders of the `@aws-appsync/utils/rds` module
//! ([`utilities`]). Every value travels in `variableMap`, as a parameter of the statement,
//! rather than in the statement's text: what a resolver was CALLED with can therefore never
//! change what its statement DOES.
//!
//! Three things make this a real call rather than a reported one:
//!
//! * the statements are RUN, over the emulator's own in-process transport (the callback handle
//!   this plane holds), so the Data API this emulator mounts serves them against the real
//!   database of the addressed cluster and what the field resolves to are that database's own
//!   rows — no connection is opened, no name is resolved and nothing leaves this process;
//! * the call is weighed against the role the data source names
//!   (`DataSource$serviceRoleArn`: "The system assumes this role when accessing the data
//!   source"), so a role whose documents withhold the Data API action on that cluster runs
//!   nothing ([`super::data_source_role`]);
//! * whatever that API REFUSES the call with — a cluster that is not there, an HTTP endpoint
//!   that is off, a secret it cannot read, a statement the database raised on — is the step's
//!   own failure, in that service's words and under its own error class, so a resolver reading
//!   `ctx.error` sees what the owner of the database said.
//!
//! Nothing of the stored record is changed here: this module reads the data source's
//! configuration and makes the calls the request names.

use ::serde_json::{json, Map as JsonMap, Value as Json};

use super::PlaneContext;

pub mod records;
pub mod utilities;

/// The `RELATIONAL_DATABASE` data source a step resolves against, as the step's own record
/// carries it.
#[derive(Debug, Clone, Copy)]
pub struct Configured<'a> {
    /// The data source's name, which an error of the step names it by.
    pub name: &'a str,
    /// `relationalDatabaseConfig.relationalDatabaseSourceType` — the one value the model
    /// declares is `RDS_HTTP_ENDPOINT` ([`RDS_HTTP_ENDPOINT_SOURCE`]), the source type this
    /// endpoint reaches a database through.
    pub source_type: Option<&'a str>,
    /// `rdsHttpEndpointConfig.dbClusterIdentifier` — "Amazon RDS cluster Amazon Resource Name
    /// (ARN)", the cluster whose endpoint every statement of this data source is run through.
    pub cluster_arn: Option<&'a str>,
    /// `rdsHttpEndpointConfig.awsSecretStoreArn` — "Amazon Web Services secret store Amazon
    /// Resource Name (ARN) for database credentials", the login the call authenticates with.
    pub secret_arn: Option<&'a str>,
    /// `rdsHttpEndpointConfig.databaseName` — "Logical database name", the database the
    /// statements run against.
    pub database_name: Option<&'a str>,
    /// `rdsHttpEndpointConfig.awsRegion` — "Amazon Web Services Region for Amazon RDS HTTP
    /// endpoint", or `None` for a configuration that names none, where the cluster's own ARN
    /// says which Region serves it.
    pub aws_region: Option<&'a str>,
    /// `serviceRoleArn`, the role AppSync makes the call under, or `None` for a data source
    /// that names none.
    pub service_role_arn: Option<&'a str>,
}

/// Run the statements `requested` carries against `source`'s database and answer the result
/// the step's response half is handed, or what the step did not resolve with — a [`Refused`],
/// whose type is what the step's own `ctx.error.type` carries.
pub async fn answered(
    context: &PlaneContext,
    source: &Configured<'_>,
    requested: &Json,
) -> Result<Json, Refused> {
    let configured = Endpoint::of(source)?;
    let call = Call::read(requested)?;
    let region = configured.region(context);
    // The permissions are weighed BEFORE anything runs: a call the role's documents withhold
    // runs no statement at all, so nothing it would have written is written.
    if let Some(role_arn) = source
        .service_role_arn
        .map(str::trim)
        .filter(|role_arn| !role_arn.is_empty())
    {
        if let Some(refusal) = super::data_source_role::withheld(
            context,
            role_arn,
            EXECUTE_STATEMENT_ACTION,
            configured.cluster_arn,
        )
        .await
        {
            return Err(Refused {
                message: refusal,
                error_type: answered_error_type(ACCESS_DENIED_EXCEPTION),
            });
        }
    }
    let parameters = call.parameters()?;
    let mut results = Vec::with_capacity(call.statements.len());
    for statement in &call.statements {
        let body = json!({
            RESOURCE_ARN_MEMBER: configured.cluster_arn,
            SECRET_ARN_MEMBER: configured.secret_arn,
            DATABASE_MEMBER: configured.database_name,
            SQL_MEMBER: statement,
            PARAMETERS_MEMBER: Json::Array(parameters.clone()),
            // The column metadata is what names the values of a row, which is what
            // `toJsonObject` reads a row by ([`records`]), so every result carries it.
            INCLUDE_RESULT_METADATA_MEMBER: true,
        });
        results.push(called(context, &region, &body).await?);
    }
    Ok(json!({RESULTS_MEMBER: results}))
}

/// The member the result of a relational-database call carries one entry per statement
/// under, each the answer of the Data API to that statement.
pub const RESULTS_MEMBER: &str = "sqlStatementResults";

/// The endpoint settings a call of this data source needs, all present.
#[derive(Debug, Clone, Copy)]
struct Endpoint<'a> {
    cluster_arn: &'a str,
    secret_arn: &'a str,
    database_name: &'a str,
    aws_region: Option<&'a str>,
}

impl<'a> Endpoint<'a> {
    /// The settings `source` was configured with, or what the step is refused with — which
    /// names the member of the data source's configuration a client has to correct.
    fn of(source: &Configured<'a>) -> Result<Self, Refused> {
        let stated = |value: Option<&'a str>| {
            value
                .map(str::trim)
                .filter(|stated: &&str| !stated.is_empty())
        };
        // The model declares exactly one source type, and a data source naming another — or
        // naming none — configures no endpoint this step can reach.
        let source_type = stated(source.source_type).unwrap_or_default();
        if source_type != RDS_HTTP_ENDPOINT_SOURCE {
            return Err(Refused::request(format!(
                "{ONE_SOURCE_TYPE} {RDS_HTTP_ENDPOINT_SOURCE}, and the data source {} names \
                 {source_type:?}.",
                source.name
            )));
        }
        let required = |value: Option<&'a str>, member: &str| {
            stated(value).ok_or_else(|| {
                Refused::request(format!(
                    "{NO_ENDPOINT_SETTING} {member}, and the data source {} states none.",
                    source.name
                ))
            })
        };
        Ok(Self {
            cluster_arn: required(source.cluster_arn, CLUSTER_SETTING)?,
            secret_arn: required(source.secret_arn, SECRET_SETTING)?,
            database_name: required(source.database_name, DATABASE_SETTING)?,
            aws_region: stated(source.aws_region),
        })
    }

    /// The Region whose Data API serves this cluster: the one the endpoint settings name,
    /// else the one the cluster's own ARN is in, else the Region the API is served in.
    fn region(&self, context: &PlaneContext) -> String {
        self.aws_region
            .or_else(|| crate::state::arn_region(self.cluster_arn))
            .unwrap_or(&context.region)
            .to_string()
    }
}

/// The ONE call a step of this data source makes, as its request half described it.
#[derive(Debug, Clone)]
struct Call {
    /// The statements to run, in the order the request states them.
    statements: Vec<String>,
    /// `variableMap` — the value each placeholder of those statements stands for.
    variables: JsonMap<String, Json>,
    /// `variableTypeHintMap` — the type a value is sent to the database as, for every value
    /// the request states one for.
    hints: JsonMap<String, Json>,
}

impl Call {
    /// The call `requested` describes, or the sentence the step is refused with — which
    /// names the member of the request the step's own definition produced, because that is
    /// the value a client has to correct.
    fn read(requested: &Json) -> Result<Self, Refused> {
        let Some(members) = requested.as_object() else {
            return Err(Refused::request(format!(
                "{A_REQUEST_IS_AN_OBJECT} {requested}."
            )));
        };
        // The version the request states, which has to be the one AppSync's
        // relational-database request carries; a request stating none is read as that one.
        let version = members
            .get(utilities::VERSION_MEMBER)
            .and_then(Json::as_str)
            .map(str::trim)
            .unwrap_or(utilities::STATEMENT_VERSION);
        if version != utilities::STATEMENT_VERSION {
            return Err(Refused::request(format!(
                "{ONE_REQUEST_VERSION} {}, and this request states {version:?}.",
                utilities::STATEMENT_VERSION
            )));
        }
        let stated = members
            .get(utilities::STATEMENTS_MEMBER)
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        if stated.is_empty() {
            return Err(Refused::request(format!(
                "{NO_STATEMENTS} {}.",
                utilities::STATEMENTS_MEMBER
            )));
        }
        if stated.len() > utilities::MAX_STATEMENTS {
            return Err(Refused::request(format!(
                "{TOO_MANY_STATEMENTS} {}, and this request carries {}.",
                utilities::MAX_STATEMENTS,
                stated.len()
            )));
        }
        let mut statements = Vec::with_capacity(stated.len());
        for statement in &stated {
            let text = statement
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    Refused::request(format!(
                        "{A_STATEMENT_IS_TEXT} {}, and this request carries {statement}.",
                        utilities::STATEMENTS_MEMBER
                    ))
                })?;
            statements.push(text.to_string());
        }
        Ok(Self {
            statements,
            variables: map_member(members, utilities::VARIABLE_MAP_MEMBER)?,
            hints: map_member(members, utilities::VARIABLE_TYPE_HINT_MAP_MEMBER)?,
        })
    }

    /// The parameters every statement of this call is run with: one per value the request's
    /// `variableMap` states, named as the placeholder names it and typed as the value's own
    /// JSON type — with the type HINT the request states for it where it states one.
    ///
    /// A placeholder is written `:name` in a statement and the parameter carries the name
    /// itself, so the two spellings of one placeholder name the same parameter.
    fn parameters(&self) -> Result<Vec<Json>, Refused> {
        let mut parameters = Vec::with_capacity(self.variables.len());
        for (placeholder, value) in &self.variables {
            let name = placeholder
                .trim()
                .trim_start_matches(PLACEHOLDER_MARKER)
                .to_string();
            if name.is_empty() {
                return Err(Refused::request(format!(
                    "{A_PLACEHOLDER_IS_NAMED} {}.",
                    utilities::VARIABLE_MAP_MEMBER
                )));
            }
            let mut parameter = JsonMap::new();
            parameter.insert(NAME_MEMBER.to_string(), Json::String(name));
            parameter.insert(VALUE_MEMBER.to_string(), field(value));
            if let Some(hint) = self.hints.get(placeholder) {
                parameter.insert(TYPE_HINT_MEMBER.to_string(), hinted(hint, placeholder)?);
            }
            parameters.push(Json::Object(parameter));
        }
        Ok(parameters)
    }
}

/// One map member of the request, and the sentence the step is refused with for a value that
/// is not one.
fn map_member(
    members: &JsonMap<String, Json>,
    name: &str,
) -> Result<JsonMap<String, Json>, Refused> {
    match members.get(name) {
        None | Some(Json::Null) => Ok(JsonMap::new()),
        Some(Json::Object(stated)) => Ok(stated.clone()),
        Some(other) => Err(Refused::request(format!(
            "{A_MAP_IS_AN_OBJECT} {name}, and this request carries {other}."
        ))),
    }
}

/// `value` as the Data API's own field of it: a field states its type by the member it
/// carries, so the member is chosen by the value's JSON type.
///
/// A composite — a list or an object — is sent as its JSON TEXT, which is what a column
/// holding a document takes, and is the form a `JSON` type hint names.
fn field(value: &Json) -> Json {
    match value {
        Json::Null => json!({"isNull": true}),
        Json::Bool(flag) => json!({"booleanValue": flag}),
        Json::Number(number) => match number.as_i64() {
            Some(whole) => json!({"longValue": whole}),
            None => json!({"doubleValue": number.as_f64().unwrap_or_default()}),
        },
        Json::String(text) => json!({"stringValue": text}),
        composite => json!({"stringValue": composite.to_string()}),
    }
}

/// The type hint the request states for one value, or the sentence the step is refused with
/// for a value that is not one of the hints the Data API declares.
fn hinted(hint: &Json, placeholder: &str) -> Result<Json, Refused> {
    let stated = hint.as_str().map(str::trim).unwrap_or_default();
    if !TYPE_HINTS.contains(&stated) {
        return Err(Refused::request(format!(
            "{ONE_OF_THE_TYPE_HINTS} {}, and this request states {stated:?} for {placeholder}.",
            TYPE_HINTS.join(", ")
        )));
    }
    Ok(Json::String(stated.to_string()))
}

/// Run one statement against the Amazon RDS Data API in `region`, in process, and answer the
/// result it answered with.
///
/// The hop is the emulator's own in-process transport, signed for the called service by the
/// tree's one in-process signer with the credential this endpoint holds — which is never
/// reported or logged — so the call is served by the Data API this emulator mounts and no
/// connection is opened. A refusal that service ANSWERED is read off the answer and reported
/// as that service's own error class; an answer from no service at all (an emulator with no
/// Data API mounted) is reported as a call that could not be made.
async fn called(context: &PlaneContext, region: &str, body: &Json) -> Result<Json, Refused> {
    let host = format!("{ENDPOINT_PREFIX}.{region}.amazonaws.com");
    let mut request = ::http::Request::builder()
        .method(::http::Method::POST)
        .uri(format!("https://{host}{EXECUTE_PATH}"))
        .header(::http::header::HOST, host.clone())
        .header(::http::header::CONTENT_TYPE, WIRE_MEDIA_TYPE)
        .body(body.to_string().into_bytes())
        .map_err(|error| Refused::unmade(&format!("{NOT_CALLED} {error}")))?;
    ::service_common::in_process_sigv4::sign_callback_request(
        &mut request,
        SIGNING_NAME,
        region,
        &context.callback,
    )
    .await
    .map_err(|error| Refused::unmade(&format!("{NOT_SIGNED} {error}")))?;
    let (parts, payload) = request.into_parts();
    let request = ::aws_smithy_runtime_api::http::Request::try_from(::http::Request::from_parts(
        parts,
        ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(payload)),
    ))
    .map_err(|error| Refused::unmade(&format!("{NOT_CALLED} {error}")))?;
    let answer = ::aws_smithy_runtime_api::client::http::HttpConnector::call(
        &context.callback.http_connector(),
        request,
    )
    .await
    .map_err(|error| {
        Refused::unmade(&format!(
            "{NOT_CALLED} {}",
            ::aws_smithy_types::error::display::DisplayErrorContext(&error)
        ))
    })?;
    let status = answer.status().as_u16();
    let class = answer
        .headers()
        .get(::service_common::error_type_locator::ERROR_TYPE_HEADER)
        .map(|named| ::service_common::error_type_locator::shape_named(named).to_string());
    let payload = answer.body().bytes().unwrap_or_default().to_vec();
    if ::service_common::in_process_refusal::is_transport_refusal(&payload) {
        return Err(Refused::unmade(NO_SUCH_SERVICE));
    }
    let document: Json = ::serde_json::from_slice(&payload).unwrap_or(Json::Null);
    if status == SERVED_STATUS {
        return Ok(document);
    }
    // What the OWNING service answered about the call, which is what a client of this field
    // has to act on: the error class and the sentence the Data API stated.
    Err(Refused {
        message: answered_message(&document),
        error_type: answered_error_type(&answered_code(&document, class.as_deref())),
    })
}

/// The error class the called service answered under: the shape its REST-JSON refusal names
/// in the field a client resolves it from, else the discriminator its body carries.
fn answered_code(document: &Json, class: Option<&str>) -> String {
    class
        .map(str::to_string)
        .or_else(|| {
            document
                .get(::service_common::error_type_locator::SHAPE_DISCRIMINATOR)
                .and_then(Json::as_str)
                .map(|named| {
                    ::service_common::error_type_locator::shape_named(named).to_string()
                })
        })
        .filter(|code| !code.is_empty())
        .unwrap_or_else(|| UNKNOWN_ERROR_CODE.to_string())
}

/// The sentence it answered with, under either spelling a JSON refusal carries it in.
fn answered_message(document: &Json) -> String {
    [ERROR_MESSAGE_MEMBER, ERROR_MESSAGE_MEMBER_CAPITALISED]
        .iter()
        .filter_map(|member| document.get(*member).and_then(Json::as_str))
        .find(|message| !message.is_empty())
        .unwrap_or(NOT_ANSWERED)
        .to_string()
}

/// Why a step resolving against a `RELATIONAL_DATABASE` data source did not resolve: the
/// sentence it states, and the `errorType` it is of.
///
/// AppSync reports both to the step's own `ctx.error` (`message` and `type`), which is what a
/// definition reads to decide the field — and what it re-raises with where it calls
/// `util.error(ctx.error.message, ctx.error.type)`.
#[derive(Debug, Clone)]
pub struct Refused {
    pub message: String,
    pub error_type: String,
}

impl Refused {
    /// A request this data source cannot act on: the step's own definition produced it, or
    /// the data source is configured for no endpoint at all, so it is an error of that CODE
    /// rather than anything the owning service answered.
    fn request(message: String) -> Self {
        Self {
            message,
            error_type: REQUEST_ERROR_TYPE.to_string(),
        }
    }

    /// A call that was never MADE: there is no answer from the owning service to report, so
    /// the step is reported as one whose call could not be placed, under the class a client
    /// raises for a call it could not complete.
    fn unmade(reason: &str) -> Self {
        Self {
            message: reason.to_string(),
            error_type: answered_error_type(BASE_EXCEPTION),
        }
    }
}

/// The `errorType` of what the OWNING service answered about the call: AppSync reports a
/// downstream refusal as the service it came from and the class of the exception its client
/// raised.
///
/// The service a step of this data source calls is the cluster's HTTP endpoint — the one the
/// `RDS_HTTP_ENDPOINT` source type addresses — so the class is named for that endpoint
/// ([`ANSWERED_ERROR_TYPE_PREFIX`]). Which class it is follows the called service's own
/// MODEL: an error the Data API declares for `ExecuteStatement` is raised as that error's own
/// class, and anything else it answers is raised as that service's BASE exception, because a
/// client has no modelled class to raise for it ([`MODELLED_ERRORS`]).
fn answered_error_type(code: &str) -> String {
    let class = if MODELLED_ERRORS.contains(&code) {
        code
    } else {
        BASE_EXCEPTION
    };
    format!("{ANSWERED_ERROR_TYPE_PREFIX}{class}")
}

/// The errors the Amazon RDS Data API DECLARES for the operation a statement of this data
/// source is run as, each of which a client raises under its own name.
///
/// Source: the `errors` of `ExecuteStatement` in that service's model
/// (`smithy_query(query="operation",operation="ExecuteStatement",
/// model_path="services/service-com-amazonaws-rdsdata-rdsdataservice/smithy-model/com-amazonaws-rdsdata-rdsdataservice.json")`).
const MODELLED_ERRORS: &[&str] = &[
    "AccessDeniedException",
    "BadRequestException",
    "DatabaseErrorException",
    "DatabaseNotFoundException",
    "DatabaseResumingException",
    "DatabaseUnavailableException",
    "ForbiddenException",
    "HttpEndpointNotEnabledException",
    "InternalServerErrorException",
    "InvalidResourceStateException",
    "InvalidSecretException",
    "SecretsErrorException",
    "ServiceUnavailableError",
    "StatementTimeoutException",
    "TransactionNotFoundException",
    "UnsupportedResultException",
];

/// The class a client raises for an answer the called service's model declares no error for.
const BASE_EXCEPTION: &str = "RdsDataServiceException";

/// The prefix the class of an answered refusal carries.
const ANSWERED_ERROR_TYPE_PREFIX: &str = "RDSHttp:";

/// The class the Data API refuses a call its caller's permissions do not admit with, which is
/// also what this endpoint reports for a call the data source's own role withholds.
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// The `errorType` a step reports for a request its own data source cannot act on, whatever
/// the data source's type ([`super::data_source_role::REQUEST_FAULT_ERROR_TYPE`]).
const REQUEST_ERROR_TYPE: &str = super::data_source_role::REQUEST_FAULT_ERROR_TYPE;

/// The one source type `RelationalDatabaseSourceType` declares: "the relational database
/// source type is an Amazon Relational Database Service (Amazon RDS) HTTP endpoint".
pub const RDS_HTTP_ENDPOINT_SOURCE: &str = crate::state::RDS_HTTP_ENDPOINT_SOURCE;

/// The IAM action running one statement performs: "Actions defined by RDS Data API" names an
/// action `rds-data:<OperationName>`, and the operation a statement is run as is
/// `ExecuteStatement`.
const EXECUTE_STATEMENT_ACTION: &str = "rds-data:ExecuteStatement";

/// The endpoint prefix, the signing name and the path of the called service: one value for
/// the Data API's prefix and signing name (`smithy_query(query="service_info")` of its
/// model), and `ExecuteStatement`'s own `http` URI.
const ENDPOINT_PREFIX: &str = "rds-data";
const SIGNING_NAME: &str = "rds-data";
const EXECUTE_PATH: &str = "/Execute";

/// The media type the called service's protocol carries a request in (`restJson1`).
const WIRE_MEDIA_TYPE: &str = "application/json";

/// The status that service answers a call it SERVED with; every other status carries a
/// refusal.
const SERVED_STATUS: u16 = 200;

/// The members one `ExecuteStatement` call carries.
const RESOURCE_ARN_MEMBER: &str = "resourceArn";
const SECRET_ARN_MEMBER: &str = "secretArn";
const DATABASE_MEMBER: &str = "database";
const SQL_MEMBER: &str = "sql";
const PARAMETERS_MEMBER: &str = "parameters";
const INCLUDE_RESULT_METADATA_MEMBER: &str = "includeResultMetadata";

/// The members one parameter of such a call carries (`SqlParameter`'s own).
const NAME_MEMBER: &str = "name";
const VALUE_MEMBER: &str = "value";
const TYPE_HINT_MEMBER: &str = "typeHint";

/// The marker a placeholder is written with in a statement, which its parameter's name does
/// not carry.
const PLACEHOLDER_MARKER: char = ':';

/// The type hints a value may be sent as. Source: `TypeHint`'s own values
/// (`smithy_query(query="shape",shape="TypeHint",
/// model_path="services/service-com-amazonaws-rdsdata-rdsdataservice/smithy-model/com-amazonaws-rdsdata-rdsdataservice.json")`).
const TYPE_HINTS: &[&str] = &["JSON", "UUID", "TIMESTAMP", "DATE", "TIME", "DECIMAL"];

/// The members a JSON refusal carries its sentence in.
const ERROR_MESSAGE_MEMBER: &str = "message";
const ERROR_MESSAGE_MEMBER_CAPITALISED: &str = "Message";

/// The class an answer naming none is reported under.
const UNKNOWN_ERROR_CODE: &str = "Unknown";

/// The sentences a refused step states. Each names what a client has to correct and
/// interpolates only the request's own values and the data source's own configuration.
const ONE_SOURCE_TYPE: &str =
    "A relational database data source is reached through the one source type its \
     configuration declares,";
const NO_ENDPOINT_SETTING: &str =
    "A relational database call needs the endpoint setting";
const A_REQUEST_IS_AN_OBJECT: &str =
    "A relational database request is an object stating the statements to run, and this \
     request is";
const ONE_REQUEST_VERSION: &str =
    "A relational database request states the version";
const NO_STATEMENTS: &str =
    "A relational database request carries at least one statement, in";
const TOO_MANY_STATEMENTS: &str =
    "A relational database request carries at most";
const A_STATEMENT_IS_TEXT: &str = "A statement is text, in";
const A_MAP_IS_AN_OBJECT: &str = "An object is expected at";
const ONE_OF_THE_TYPE_HINTS: &str =
    "A value is sent to the database as one of the types";
const A_PLACEHOLDER_IS_NAMED: &str = "Every placeholder of a statement carries a name, in";
const NOT_SIGNED: &str =
    "This field's relational database call could not be signed, so none was made:";
const NOT_CALLED: &str = "This field's relational database call could not be made:";
const NO_SUCH_SERVICE: &str =
    "This emulator holds no Amazon RDS Data API, so no relational database call could be \
     made.";
const NOT_ANSWERED: &str = "This field's relational database call did not succeed.";

/// The settings of the endpoint one call is made through, as the data source's own
/// configuration carries them, for the readings above.
const CLUSTER_SETTING: &str = "dbClusterIdentifier";
const SECRET_SETTING: &str = "awsSecretStoreArn";
const DATABASE_SETTING: &str = "databaseName";

#[cfg(test)]
mod tests {
    use super::*;

    /// A data source configured for the endpoint of one cluster.
    fn configured<'a>(source_type: Option<&'a str>, cluster: Option<&'a str>) -> Configured<'a> {
        Configured {
            name: "widgets",
            source_type,
            cluster_arn: cluster,
            secret_arn: Some("arn:aws:secretsmanager:us-east-1:123456789012:secret:login"),
            database_name: Some("appdb"),
            aws_region: None,
            service_role_arn: None,
        }
    }

    /// RULE: a data source that does not state the endpoint a call is made through refuses
    /// the step, naming the setting to correct — and nothing is called.
    ///
    /// The rows span the source type and each endpoint setting a call needs.
    #[test]
    fn a_data_source_with_no_endpoint_refuses_the_step() {
        let cluster = Some("arn:aws:rds:us-east-1:123456789012:cluster:widgets");
        let rows: &[(&str, Configured<'_>, &str)] = &[
            (
                "a source type the model does not declare",
                configured(Some("RDS_WIRE"), cluster),
                RDS_HTTP_ENDPOINT_SOURCE,
            ),
            (
                "no source type at all",
                configured(None, cluster),
                RDS_HTTP_ENDPOINT_SOURCE,
            ),
            (
                "no cluster",
                configured(Some(RDS_HTTP_ENDPOINT_SOURCE), None),
                CLUSTER_SETTING,
            ),
            (
                "no secret",
                Configured {
                    secret_arn: None,
                    ..configured(Some(RDS_HTTP_ENDPOINT_SOURCE), cluster)
                },
                SECRET_SETTING,
            ),
            (
                "no database",
                Configured {
                    database_name: Some("  "),
                    ..configured(Some(RDS_HTTP_ENDPOINT_SOURCE), cluster)
                },
                DATABASE_SETTING,
            ),
        ];
        for (case, source, named) in rows {
            let refused = Endpoint::of(source)
                .err()
                .unwrap_or_else(|| panic!("case {case:?}: expected the step to be refused"));
            assert!(
                refused.message.contains(named),
                "case {case:?}: expected the refusal to name {named:?}, got {:?}",
                refused.message
            );
            assert_eq!(
                refused.error_type, REQUEST_ERROR_TYPE,
                "case {case:?}: expected the class of a request this data source cannot act \
                 on, got {:?}",
                refused.error_type
            );
        }
    }

    /// RULE: every value of the request's `variableMap` becomes one parameter of every
    /// statement, named as the placeholder names it and typed by the value's own JSON type.
    ///
    /// The rows span the JSON types a value is stated in, including a composite, and a value
    /// the request states a type hint for.
    #[test]
    fn every_value_of_the_request_is_sent_as_a_parameter() {
        let call = Call::read(&json!({
            "version": utilities::STATEMENT_VERSION,
            "statements": ["SELECT 1"],
            "variableMap": {
                ":text": "widget-alpha",
                ":whole": 7,
                ":fraction": 1.5,
                ":flag": true,
                ":nothing": Json::Null,
                ":document": {"a": 1},
                "bare": "no marker",
            },
            "variableTypeHintMap": {":document": "JSON"},
        }))
        .expect("the request is one this data source acts on");
        let parameters = call
            .parameters()
            .expect("the parameters are ones this data source sends");
        let sent = |name: &str| {
            parameters
                .iter()
                .find(|parameter| parameter[NAME_MEMBER] == json!(name))
                .cloned()
                .unwrap_or(Json::Null)
        };
        let rows: &[(&str, &str, Json)] = &[
            ("text", "text", json!({"stringValue": "widget-alpha"})),
            ("a whole number", "whole", json!({"longValue": 7})),
            ("a fraction", "fraction", json!({"doubleValue": 1.5})),
            ("a flag", "flag", json!({"booleanValue": true})),
            ("nothing", "nothing", json!({"isNull": true})),
            ("a document", "document", json!({"stringValue": "{\"a\":1}"})),
            ("a placeholder written bare", "bare", json!({"stringValue": "no marker"})),
        ];
        for (case, name, value) in rows {
            let parameter = sent(name);
            assert_eq!(
                &parameter[VALUE_MEMBER], value,
                "case {case:?}: expected {value}, got {parameter}"
            );
        }
        assert_eq!(
            sent("document")[TYPE_HINT_MEMBER],
            json!("JSON"),
            "a value the request hints is sent as that type, got {:?}",
            sent("document")
        );
        assert_eq!(
            sent("text").get(TYPE_HINT_MEMBER),
            None,
            "a value the request does not hint is sent with no hint, got {:?}",
            sent("text")
        );
    }

    /// RULE: a request this data source cannot act on is refused, naming the member to
    /// correct, and no statement is run.
    ///
    /// The rows span the request's version, how many statements it carries, what a statement
    /// is, what the maps are, and the type hints a value may be sent as.
    #[test]
    fn a_request_this_data_source_cannot_act_on_is_refused() {
        let rows: &[(&str, Json)] = &[
            ("a request that is no object", json!("SELECT 1")),
            (
                "a version this request is not written to",
                json!({"version": "2017-02-28", "statements": ["SELECT 1"]}),
            ),
            ("no statements at all", json!({"statements": []})),
            (
                "one statement more than a request carries",
                json!({"statements": ["SELECT 1", "SELECT 2", "SELECT 3"]}),
            ),
            (
                "a statement that is not text",
                json!({"statements": [{"sql": "SELECT 1"}]}),
            ),
            (
                "a statement with no text at all",
                json!({"statements": ["   "]}),
            ),
            (
                "a variable map that is no object",
                json!({"statements": ["SELECT 1"], "variableMap": [":id", 1]}),
            ),
        ];
        for (case, requested) in rows {
            let refused = Call::read(requested)
                .err()
                .unwrap_or_else(|| panic!("case {case:?}: expected the step to be refused"));
            assert_eq!(
                refused.error_type, REQUEST_ERROR_TYPE,
                "case {case:?}: expected the class of a request this data source cannot act \
                 on, got {:?}",
                refused.error_type
            );
        }
        let hinted = Call::read(&json!({
            "statements": ["SELECT 1"],
            "variableMap": {":id": "7"},
            "variableTypeHintMap": {":id": "BIGINT"},
        }))
        .expect("the request is one this data source acts on")
        .parameters();
        assert!(
            hinted.is_err(),
            "a type hint the Data API does not declare is refused, got {hinted:?}"
        );
    }

    /// RULE: the class of a refusal the called service answered is that service's own error
    /// class, and an answer naming a class its model does not declare is reported as its base
    /// exception.
    #[test]
    fn the_class_of_a_refusal_is_the_one_the_called_service_answered() {
        for (case, answered, expected) in [
            (
                "an error the Data API declares",
                "BadRequestException",
                "RDSHttp:BadRequestException",
            ),
            (
                "an error named with its namespace",
                "com.amazonaws.rdsdata#DatabaseErrorException",
                "RDSHttp:DatabaseErrorException",
            ),
            (
                "an answer its model declares no error for",
                "SomethingElse",
                "RDSHttp:RdsDataServiceException",
            ),
        ] {
            let class = ::service_common::error_type_locator::shape_named(answered);
            let reported = answered_error_type(class);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }
}

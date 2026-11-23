// SPDX-License-Identifier: CC0-1.0

//! The `aws_s3` extension's functions on a PostgreSQL endpoint: the Amazon S3 import and
//! export an RDS for PostgreSQL or Aurora PostgreSQL DB instance serves.
//!
//! Neither function is a statement the embedded SQL engine has a grammar for — they are the
//! functions the `aws_s3` extension Amazon RDS ships adds to the engine
//! (`aws_s3.table_import_from_s3`, which reads an object into a table, and
//! `aws_s3.query_export_to_s3`, which writes a query's rows out as one) — so this module
//! owns the three parts of each: what the call NAMES, whether the resource has the S3
//! capability the call needs, and how an object's bytes become rows of a table (and rows of
//! a query become an object's bytes).
//!
//! The rows really move: an import reads the object out of Amazon S3's OWN store on this
//! engine and loads each record through the same bulk load the wire's `COPY` uses
//! ([`super::pg_copy`]), and an export runs the query on this session and writes the bytes it
//! rendered back into that store, where Amazon S3's own `GetObject` then serves them.
//! Nothing is answered from the shape of the call's text.
//!
//! WHETHER the engine may reach the object at all is the resource's own state, never the
//! call's: the engine acts under the IAM role the instance has ASSOCIATED for the feature the
//! function needs ([`super::engine_s3_role`]), and the object operation is performed only
//! where that role's own policies allow it. With no such association the function still RUNS
//! and fails where the extension would ask for credentials, and with a role whose policies
//! refuse the object the call fails with the Amazon S3 error — so a client can tell a missing
//! association from a missing permission, and either from a function the engine does not
//! have.
//!
//! The functions live in the extension, so they answer at all only where the extension is
//! INSTALLED in the session's database ([`super::server_statements`]): a database that has
//! not run `CREATE EXTENSION aws_s3` has no `aws_s3` schema, which is what PostgreSQL says
//! about such a call.

use ::sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, FunctionArguments, ObjectName,
    ObjectNamePart, SelectItem, SetExpr, Statement, TableFactor, Value as SqlValue,
};
use ::sqlparser::dialect::PostgreSqlDialect;
use ::sqlparser::parser::Parser;

use super::engine_s3_role::{self, S3Capability};
use super::mysql_s3_load::{MAX_LOAD_BYTES, MAX_LOAD_ROWS};
use super::pg_copy::{CopyTarget, Records};
use super::sql_engine::{Cell, ColumnInfo, EngineError, EngineErrorKind, InstanceDatabase, ResultSet};
use super::wire_session::{self, SessionContext, SessionFacts, StatementAnswer};
use super::EngineWire;

/// The schema the extension's own functions live in, which is also the name it is installed
/// under.
const AWS_S3_SCHEMA: &str = "aws_s3";

/// The schema of the helper functions the `aws_s3` calls take their composite arguments from:
/// `aws_commons.create_s3_uri` builds the `s3_info` value and
/// `aws_commons.create_aws_credentials` the credentials one, which is why `aws_s3` requires
/// `aws_commons` ([`super::server_statements`]).
const AWS_COMMONS_SCHEMA: &str = "aws_commons";

/// `aws_s3.table_import_from_s3(table_name, column_list, options, …)` — the import.
const IMPORT_FUNCTION: &str = "table_import_from_s3";

/// `aws_s3.query_export_to_s3(query, …)` — the export.
const EXPORT_FUNCTION: &str = "query_export_to_s3";

/// `aws_commons.create_s3_uri(bucket, file_path, region)`.
const CREATE_S3_URI: &str = "create_s3_uri";

/// `aws_commons.create_aws_credentials(access_key, secret_key, session_token)`.
const CREATE_AWS_CREDENTIALS: &str = "create_aws_credentials";

/// The object one call names: the bucket, the key and the Region the call stated for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Object {
    pub bucket: String,
    pub key: String,
    /// The Region the call named, which the extension takes as an argument of its own. Kept
    /// because it is part of what the call said, not because the object is reached through
    /// it: the store this endpoint reads and writes is the one on this engine.
    pub region: Option<String>,
}

/// One call to a function of the `aws_s3` extension, as the server read it.
#[derive(Debug, Clone)]
pub enum AwsS3Call {
    /// `aws_s3.table_import_from_s3(...)`: the object's records become rows of the table.
    Import {
        /// The relation the rows are loaded into, as the call named it.
        table: String,
        /// The columns the call named, in its own order; empty where it named none ("an
        /// optional list of columns … an empty string means all the columns").
        columns: Vec<String>,
        /// The `COPY` options the call named, which say how the object's bytes are read.
        options: CopyFormat,
        object: S3Object,
    },
    /// `aws_s3.query_export_to_s3(...)`: the query's rows become the object's bytes.
    Export {
        /// The query whose rows are exported, run on this session.
        query: String,
        /// The `COPY` options the call named, which say how the bytes are written.
        options: CopyFormat,
        object: S3Object,
    },
}

impl AwsS3Call {
    /// Which S3 integration of the engine the call performs, which decides the feature the
    /// instance must have associated a role for and the action that role must allow.
    #[must_use]
    pub fn capability(&self) -> S3Capability {
        match self {
            Self::Import { .. } => S3Capability::Import,
            Self::Export { .. } => S3Capability::Export,
        }
    }

    /// The object the call names.
    #[must_use]
    pub fn object(&self) -> &S3Object {
        match self {
            Self::Import { object, .. } | Self::Export { object, .. } => object,
        }
    }

    /// The function's own name, which is the column a call in a select list is reported
    /// under.
    #[must_use]
    fn function(&self) -> &'static str {
        match self {
            Self::Import { .. } => IMPORT_FUNCTION,
            Self::Export { .. } => EXPORT_FUNCTION,
        }
    }
}

/// The `COPY` options an `aws_s3` call carries: "arguments to the PostgreSQL `COPY` command"
/// (`aws_s3.table_import_from_s3`), which is what says how the object's bytes and the
/// table's rows correspond.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFormat {
    /// Whether the object is in the `csv` format rather than `COPY`'s default text format.
    pub csv: bool,
    /// Whether it is in `COPY`'s BINARY format, in which every value carries the bytes its
    /// own type's binary form has rather than its text.
    pub binary: bool,
    /// The field separator: a tab in the text format and a comma in `csv`, which are
    /// `COPY`'s own defaults for each.
    pub delimiter: u8,
    /// Whether the first record is a header line of column names rather than data.
    pub header: bool,
    /// The quoting character of the `csv` format.
    pub quote: u8,
    /// Its escape character, which defaults to the quoting character.
    pub escape: u8,
    /// The string that stands for a NULL: `\N` in the text format and the empty field in
    /// `csv`, which are `COPY`'s own defaults.
    pub null: String,
}

impl CopyFormat {
    /// The options a call that named none carries: `COPY`'s own defaults for the text
    /// format.
    #[must_use]
    pub fn text() -> Self {
        Self {
            csv: false,
            binary: false,
            delimiter: b'\t',
            header: false,
            quote: b'"',
            escape: b'"',
            null: "\\N".to_string(),
        }
    }
}

/// The call `sql` makes to a function of the `aws_s3` extension, `None` for a statement that
/// makes none, and the refusal for one that names such a function and cannot be read as a
/// call to it.
///
/// Only the PostgreSQL wire has these functions: they come from an extension of that engine,
/// and the other wire's Amazon S3 import is a statement of its own
/// ([`super::mysql_s3_load`]).
#[must_use]
pub fn aws_s3_call(facts: &SessionFacts, sql: &str) -> Option<Result<AwsS3Call, EngineError>> {
    if facts.wire != EngineWire::Postgres {
        return None;
    }
    // The cheap reading first: a statement that does not name the schema at all cannot be a
    // call to one of its functions, and is not parsed twice for nothing.
    if !names_the_schema(sql) {
        return None;
    }
    // The same bound every other statement of a session is read under: a statement longer than
    // this endpoint serves is refused BEFORE any of it is parsed, so no caller-controlled text
    // sizes the work this module does ([`super::wire_session::MAX_STATEMENT_BYTES`]).
    if sql.len() > super::wire_session::MAX_STATEMENT_BYTES {
        return Some(Err(EngineError::new(
            EngineErrorKind::Syntax,
            format!(
                "the statement is longer than this emulator serves ({} bytes)",
                super::wire_session::MAX_STATEMENT_BYTES
            ),
        )));
    }
    // A statement this dialect cannot parse is left to the engine, which answers its own
    // condition for it: nothing here is decided from text that was never read as SQL.
    let statements = Parser::parse_sql(&PostgreSqlDialect {}, sql).ok()?;
    let [statement] = statements.as_slice() else {
        return None;
    };
    let (function, arguments) = aws_s3_function(statement)?;
    let call = match read_call(&function, &arguments) {
        Ok(call) => call,
        Err(refusal) => return Some(Err(refusal)),
    };
    // The IMPORT writes rows, through the same bulk load a client's `COPY … FROM STDIN`
    // performs, so the endpoint's own read-only state refuses it exactly as it refuses that
    // statement: a read replica or a standby serves no write, and neither does a session whose
    // own transaction is read-only ([`super::read_only`]). The EXPORT only reads the table it
    // queries, so it is served there.
    if matches!(call, AwsS3Call::Import { .. }) && facts.read_only_transaction() {
        return Some(Err(super::read_only::refusal(
            facts.wire,
            COPY_FROM_COMMAND,
        )));
    }
    Some(Ok(call))
}

/// The command a read-only endpoint's refusal of the import NAMES: the bulk load it performs
/// is `COPY … FROM`, which is the command the server reports for it
/// ([`super::read_only`]'s own catalogue of writing commands).
const COPY_FROM_COMMAND: &str = "COPY FROM";

/// Whether `sql` names the extension's schema at all, which is what makes it worth parsing.
fn names_the_schema(sql: &str) -> bool {
    sql.to_ascii_lowercase()
        .contains(&format!("{AWS_S3_SCHEMA}."))
}

/// The `aws_s3.<function>(…)` call `statement` makes, with its arguments — the select list
/// and the `FROM` list, which are the two places a client calls one from (`SELECT
/// aws_s3.table_import_from_s3(…)` and `SELECT * FROM aws_s3.query_export_to_s3(…)`, the
/// set-returning form).
fn aws_s3_function(statement: &Statement) -> Option<(String, Vec<FunctionArg>)> {
    let Statement::Query(query) = statement else {
        return None;
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return None;
    };
    for item in &select.projection {
        let expression = match item {
            SelectItem::UnnamedExpr(expression) => expression,
            SelectItem::ExprWithAlias { expr, .. } => expr,
            _ => continue,
        };
        if let Expr::Function(function) = expression {
            if let Some(named) = schema_function(&function.name, AWS_S3_SCHEMA) {
                return Some((named, argument_list(&function.args)));
            }
        }
    }
    for table in &select.from {
        if let TableFactor::Table {
            name,
            args: Some(arguments),
            ..
        } = &table.relation
        {
            if let Some(named) = schema_function(name, AWS_S3_SCHEMA) {
                return Some((named, arguments.args.clone()));
            }
        }
    }
    None
}

/// The function `name` names within `schema`, or `None` for a name outside it.
///
/// PostgreSQL folds an unquoted identifier to lower case, so a call spelled in another case
/// names the same function; a QUOTED identifier keeps its case and is compared as written.
fn schema_function(name: &ObjectName, schema: &str) -> Option<String> {
    let parts: Vec<String> = name
        .0
        .iter()
        .filter_map(ObjectNamePart::as_ident)
        .map(|ident| match ident.quote_style {
            Some(_) => ident.value.clone(),
            None => ident.value.to_ascii_lowercase(),
        })
        .collect();
    match parts.as_slice() {
        [qualifier, function] if qualifier == schema => Some(function.clone()),
        _ => None,
    }
}

/// The arguments of a function call, as the list the parse carries for it.
fn argument_list(arguments: &FunctionArguments) -> Vec<FunctionArg> {
    match arguments {
        FunctionArguments::List(list) => list.args.clone(),
        FunctionArguments::None | FunctionArguments::Subquery(_) => Vec::new(),
    }
}

/// One argument of a call, as this server can read it: the extension's arguments are
/// CONSTANTS and the composite values its helper functions build out of constants, which is
/// what a call to it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Argument {
    Text(String),
    Null,
    /// The `s3_info` value `aws_commons.create_s3_uri` builds.
    Object(S3Object),
    /// The credentials value `aws_commons.create_aws_credentials` builds. Read so a call
    /// that names it is served, and carried no further: this emulator's engine reaches the
    /// object store it is mounted beside rather than signing a request with keys.
    Credentials,
}

impl Argument {
    /// The text this argument carries, or `None` for one that carries none.
    fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// Read the call to `function` that `arguments` carries.
fn read_call(function: &str, arguments: &[FunctionArg]) -> Result<AwsS3Call, EngineError> {
    let read = read_arguments(function, arguments)?;
    match function {
        IMPORT_FUNCTION => read_import(&read),
        EXPORT_FUNCTION => read_export(&read),
        other => Err(undefined_function(other, arguments.len())),
    }
}

/// The arguments of ONE call, split the way PostgreSQL takes them: the positional ones in the
/// order they were written, and the ones NAMED by the parameter they name.
#[derive(Debug, Default)]
struct CallArguments {
    positional: Vec<Argument>,
    /// `(parameter, value)`, the parameter folded to lower case — which is how an unquoted
    /// parameter name is resolved.
    named: Vec<(String, Argument)>,
}

impl CallArguments {
    /// The argument for the parameter `parameter`, which is the one NAMED where the call named
    /// it and the one at position `at` otherwise — the resolution PostgreSQL makes for a call
    /// that mixes the two forms.
    fn at(&self, at: usize, parameter: &str) -> Option<&Argument> {
        self.named
            .iter()
            .find(|(name, _)| name == parameter)
            .map(|(_, value)| value)
            .or_else(|| self.positional.get(at))
    }

    /// The same argument as the text it carries.
    fn text_at(&self, at: usize, parameter: &str) -> Option<&str> {
        self.at(at, parameter).and_then(Argument::text)
    }
}

/// The call's arguments, split the way PostgreSQL takes them: the positional ones in order,
/// and the ones named with `=>` or `:=` by the parameter they name (folded to lower case,
/// which is how an unquoted parameter name is resolved).
fn read_arguments(
    function: &str,
    arguments: &[FunctionArg],
) -> Result<CallArguments, EngineError> {
    let mut read = CallArguments::default();
    let (positional, named) = (&mut read.positional, &mut read.named);
    for argument in arguments {
        match argument {
            // `parameter := value`, the way PostgreSQL names an argument of a function it
            // calls — the parse carries it as the assignment it is written as.
            FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::BinaryOp {
                left,
                op: BinaryOperator::Assignment,
                right,
            })) => {
                let Expr::Identifier(parameter) = left.as_ref() else {
                    return Err(undefined_function(function, arguments.len()));
                };
                named.push((
                    parameter.value.to_ascii_lowercase(),
                    read_argument(function, right)?,
                ));
            }
            FunctionArg::Unnamed(FunctionArgExpr::Expr(expression)) => {
                positional.push(read_argument(function, expression)?);
            }
            // `parameter => value`, the other spelling of a named argument.
            FunctionArg::Named {
                name,
                arg: FunctionArgExpr::Expr(expression),
                ..
            } => {
                named.push((
                    name.value.to_ascii_lowercase(),
                    read_argument(function, expression)?,
                ));
            }
            FunctionArg::ExprNamed {
                name: Expr::Identifier(name),
                arg: FunctionArgExpr::Expr(expression),
                ..
            } => {
                named.push((
                    name.value.to_ascii_lowercase(),
                    read_argument(function, expression)?,
                ));
            }
            _ => return Err(undefined_function(function, arguments.len())),
        }
    }
    Ok(read)
}

/// One argument expression, as a constant or a composite of constants.
fn read_argument(function: &str, expression: &Expr) -> Result<Argument, EngineError> {
    match expression {
        Expr::Value(value) => match &value.value {
            SqlValue::SingleQuotedString(text)
            | SqlValue::DoubleQuotedString(text)
            | SqlValue::EscapedStringLiteral(text)
            | SqlValue::UnicodeStringLiteral(text) => Ok(Argument::Text(text.clone())),
            SqlValue::Number(digits, _) => Ok(Argument::Text(digits.clone())),
            SqlValue::Boolean(flag) => Ok(Argument::Text(flag.to_string())),
            SqlValue::DollarQuotedString(quoted) => Ok(Argument::Text(quoted.value.clone())),
            SqlValue::Null => Ok(Argument::Null),
            _ => Err(argument_not_constant(function)),
        },
        // A cast and a parenthesised expression are the same value, written differently.
        Expr::Cast { expr, .. } | Expr::Nested(expr) => read_argument(function, expr),
        Expr::Function(inner) => match schema_function(&inner.name, AWS_COMMONS_SCHEMA) {
            Some(helper) if helper == CREATE_S3_URI => {
                read_s3_uri(&argument_list(&inner.args))
            }
            Some(helper) if helper == CREATE_AWS_CREDENTIALS => Ok(Argument::Credentials),
            _ => Err(argument_not_constant(function)),
        },
        _ => Err(argument_not_constant(function)),
    }
}

/// `aws_commons.create_s3_uri(bucket, file_path, region)` — the composite an `s3_info`
/// argument is.
fn read_s3_uri(arguments: &[FunctionArg]) -> Result<Argument, EngineError> {
    let read = read_arguments(CREATE_S3_URI, arguments)?;
    let (Some(bucket), Some(key)) = (read.text_at(0, "bucket"), read.text_at(1, "file_path"))
    else {
        return Err(argument_not_constant(CREATE_S3_URI));
    };
    Ok(Argument::Object(S3Object {
        bucket: bucket.to_string(),
        key: key.to_string(),
        region: read.text_at(2, "region").map(str::to_string),
    }))
}

/// Read `aws_s3.table_import_from_s3(table_name, column_list, options, …)`, in both the form
/// that names the bucket, key and Region and the one that takes the composite
/// `aws_commons.create_s3_uri` built.
fn read_import(read: &CallArguments) -> Result<AwsS3Call, EngineError> {
    let table = read
        .text_at(0, "table_name")
        .ok_or_else(|| argument_not_constant(IMPORT_FUNCTION))?
        .to_string();
    let columns = read.text_at(1, "column_list").unwrap_or_default().to_string();
    let options = read.text_at(2, "options").unwrap_or_default();
    let object = object_argument(IMPORT_FUNCTION, read.at(3, "s3_info"), || {
        (
            read.at(3, "bucket"),
            read.at(4, "file_path"),
            read.at(5, "region"),
        )
    })?;
    Ok(AwsS3Call::Import {
        table,
        columns: column_list(&columns),
        options: read_options(options)?,
        object,
    })
}

/// Read `aws_s3.query_export_to_s3(query, …)`, in the same two forms.
fn read_export(read: &CallArguments) -> Result<AwsS3Call, EngineError> {
    let query = read
        .text_at(0, "query")
        .ok_or_else(|| argument_not_constant(EXPORT_FUNCTION))?
        .to_string();
    // The composite form takes the options as its SECOND argument and the bucket form as its
    // fifth, the Region being the fourth there — the order each signature declares.
    let composite = matches!(read.at(1, "s3_info"), Some(Argument::Object(_)));
    let object = object_argument(EXPORT_FUNCTION, read.at(1, "s3_info"), || {
        (
            read.at(1, "bucket"),
            read.at(2, "file_path"),
            read.at(3, "region"),
        )
    })?;
    let options = match composite {
        true => read.text_at(2, "options"),
        false => read.text_at(4, "options"),
    }
    .unwrap_or_default();
    Ok(AwsS3Call::Export {
        query,
        options: read_options(options)?,
        object,
    })
}

/// The object a call names: the composite where it carries one, else the bucket, key and
/// Region arguments of the other signature.
fn object_argument<'a>(
    function: &str,
    composite: Option<&Argument>,
    fields: impl FnOnce() -> (Option<&'a Argument>, Option<&'a Argument>, Option<&'a Argument>),
) -> Result<S3Object, EngineError> {
    if let Some(Argument::Object(object)) = composite {
        return Ok(object.clone());
    }
    let (bucket, key, region) = fields();
    let (Some(bucket), Some(key)) = (
        bucket.and_then(Argument::text),
        key.and_then(Argument::text),
    ) else {
        return Err(argument_not_constant(function));
    };
    Ok(S3Object {
        bucket: bucket.to_string(),
        key: key.to_string(),
        region: region.and_then(Argument::text).map(str::to_string),
    })
}

/// The columns a `column_list` names: "a comma-separated list of columns", and an empty
/// string naming every column of the table.
fn column_list(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|column| !column.is_empty())
        .map(|column| column.trim_matches('"').to_string())
        .collect()
}

/// Read the `COPY` options a call carries: "arguments to the PostgreSQL `COPY` command", in
/// the parenthesised form `COPY` itself takes them in (`(format csv, header true)`).
///
/// An option `COPY` does not define is refused the way PostgreSQL refuses one, because the
/// options really are its own; an option it defines and this endpoint does not read the
/// bytes by is refused as the capability it is.
///
/// `pub(super)` because the options are `COPY`'s own in the first place: the wire's
/// `COPY … TO STDOUT` carries the same `WITH (…)` clause and reads it here
/// ([`super::pg_copy`]), so the two cannot read the same clause differently.
pub(super) fn read_options(options: &str) -> Result<CopyFormat, EngineError> {
    let mut format = CopyFormat::text();
    let trimmed = options.trim();
    let inner = trimmed
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(trimmed);
    let mut named_delimiter = None;
    let mut named_null = None;
    for item in split_options(inner) {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (keyword, value) = match item.split_once(char::is_whitespace) {
            Some((keyword, value)) => (keyword, unquoted(value.trim())),
            None => (item, String::new()),
        };
        match keyword.to_ascii_uppercase().as_str() {
            "FORMAT" => match value.to_ascii_lowercase().as_str() {
                "csv" => format.csv = true,
                "text" => format.csv = false,
                // The BINARY format, in which each value carries its own type's binary form
                // ([`super::pg_binary`]).
                "binary" => format.binary = true,
                other => {
                    return Err(EngineError::new(
                        EngineErrorKind::FeatureNotSupported,
                        format!("COPY format \"{other}\" is not served by this endpoint"),
                    )
                    .with_sqlstate("0A000"))
                }
            },
            "DELIMITER" => named_delimiter = Some(one_byte("DELIMITER", &value)?),
            "QUOTE" => format.quote = one_byte("QUOTE", &value)?,
            "ESCAPE" => format.escape = one_byte("ESCAPE", &value)?,
            "NULL" => named_null = Some(value),
            "HEADER" => {
                format.header = value.is_empty()
                    || matches!(
                        value.to_ascii_lowercase().as_str(),
                        "true" | "on" | "1" | "match"
                    );
            }
            // The encoding of the object's bytes: this endpoint reads them as the UTF-8 the
            // store holds, so the option is read and changes nothing.
            "ENCODING" => {}
            other => {
                return Err(EngineError::new(
                    EngineErrorKind::Syntax,
                    format!("option \"{}\" not recognized", other.to_ascii_lowercase()),
                )
                .with_sqlstate("42601"))
            }
        }
    }
    // The defaults of the FORMAT the options settled on, where the options named none of
    // their own: the separator and the NULL string differ between the two formats.
    if format.csv {
        format.delimiter = b',';
        format.null = String::new();
        format.escape = format.quote;
    }
    if let Some(delimiter) = named_delimiter {
        format.delimiter = delimiter;
    }
    if let Some(null) = named_null {
        format.null = null;
    }
    Ok(format)
}

/// The option items `inner` carries: the commas that separate them, with the ones inside a
/// quoted value left alone.
fn split_options(inner: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in inner.chars() {
        match character {
            '\'' => {
                quoted = !quoted;
                current.push(character);
            }
            ',' if !quoted => items.push(std::mem::take(&mut current)),
            _ => current.push(character),
        }
    }
    items.push(current);
    items
}

/// One option value with the quotes `COPY` writes it in taken off.
fn unquoted(value: &str) -> String {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .map_or_else(|| trimmed.to_string(), |inner| inner.replace("''", "'"))
}

/// The single byte an option that takes one character names, refused the way PostgreSQL
/// refuses a longer value for it.
fn one_byte(option: &str, value: &str) -> Result<u8, EngineError> {
    let bytes = value.as_bytes();
    match bytes {
        [byte] => Ok(*byte),
        _ => Err(EngineError::new(
            EngineErrorKind::Syntax,
            format!("COPY {option} must be a single one-byte character"),
        )
        .with_sqlstate("22023")),
    }
}

/// What a call to a function the `aws_s3` schema does not define is refused with, in
/// PostgreSQL's own words for a function it cannot resolve.
fn undefined_function(function: &str, arity: usize) -> EngineError {
    let arguments = std::iter::repeat_n("unknown", arity)
        .collect::<Vec<_>>()
        .join(", ");
    EngineError::new(
        EngineErrorKind::UndefinedObject,
        format!("function {AWS_S3_SCHEMA}.{function}({arguments}) does not exist"),
    )
    .with_sqlstate("42883")
}

/// What a call whose argument is not a value this server can read is refused with: the
/// extension's arguments are constants, so an expression that has to be evaluated over rows
/// is not one of its calls.
fn argument_not_constant(function: &str) -> EngineError {
    EngineError::new(
        EngineErrorKind::FeatureNotSupported,
        format!(
            "the arguments of {AWS_S3_SCHEMA}.{function} are read as constants by this \
             endpoint, and this call carries an expression that is not one"
        ),
    )
    .with_sqlstate("0A000")
}

/// What the extension answers when the instance has no credentials for the feature the call
/// needs: no IAM role is associated with it under that `FeatureName`, so the client's remedy
/// is the association rather than the call.
///
/// The sentence is the extension's own for that condition — it names the feature whose
/// credentials could not be read, under the token the extension writes it with — and carries
/// no other variable part.
#[must_use]
pub fn credentials_not_accessible(capability: S3Capability) -> EngineError {
    EngineError::new(
        EngineErrorKind::InsufficientPrivilege,
        format!(
            "credentials for the {} feature could not be accessed",
            capability.engine_token()
        ),
    )
}

/// What the extension answers when the object operation is not one the associated role may
/// perform: the feature whose credentials could not be used, and the Amazon S3 error code and
/// message the engine's own call was refused with, with the action and the object it names.
///
/// The condition the extension reports is the same one [`credentials_not_accessible`] names —
/// it has no credentials it can perform the operation with — and what the S3 call answered is
/// the detail of it, so the two are reported in one sentence: a client branching on the
/// feature's credentials and one branching on the Amazon S3 error class both find what they
/// match on.
#[must_use]
pub fn access_denied(capability: S3Capability, object_arn: &str) -> EngineError {
    EngineError::new(
        EngineErrorKind::InsufficientPrivilege,
        format!(
            "credentials for the {} feature could not be accessed: S3 API returned error: \
             AccessDenied: Access Denied ({} on {object_arn})",
            capability.engine_token(),
            capability.object_action()
        ),
    )
}

/// What the export answers for a query that is itself a call of this extension: the exported
/// query runs on the session, so one naming this schema would be served by the very path that
/// dispatches these functions, and the nesting would be the statement's to choose.
///
/// `0A000` `feature_not_supported` is what PostgreSQL reports for a construct the server does
/// not serve in the position it was written in, and the sentence names the function the query
/// may not call so a client can see which argument to change.
#[must_use]
fn nested_export() -> EngineError {
    EngineError::new(
        EngineErrorKind::FeatureNotSupported,
        format!(
            "the query {AWS_S3_SCHEMA}.{EXPORT_FUNCTION} exports must not itself call a \
             function of the {AWS_S3_SCHEMA} schema"
        ),
    )
    .with_sqlstate("0A000")
}

/// What it answers when Amazon S3 holds no such bucket: that service's own error code and
/// message for a read or a write of one.
#[must_use]
fn no_such_bucket() -> EngineError {
    EngineError::new(
        EngineErrorKind::UndefinedObject,
        "S3 API returned error: NoSuchBucket: The specified bucket does not exist",
    )
    .with_sqlstate("42704")
}

/// And when the bucket holds no such object.
#[must_use]
fn no_such_key() -> EngineError {
    EngineError::new(
        EngineErrorKind::UndefinedObject,
        "S3 API returned error: NoSuchKey: The specified key does not exist",
    )
    .with_sqlstate("42704")
}

/// What it answers when this emulator has no object store to reach at all — a failure of the
/// emulator rather than of the call, so nothing is claimed for it.
fn no_object_store() -> EngineError {
    EngineError::new(
        EngineErrorKind::Internal,
        "the Amazon S3 service this endpoint reads and writes objects through is not \
         reachable from this emulator",
    )
}

/// Serve one call to a function of the `aws_s3` extension.
///
/// In the extension's own order: the functions exist only where it is installed, the engine
/// needs credentials for the feature before it can call Amazon S3 at all, the role those
/// credentials are of has to be allowed the object operation, and only then is the object
/// read or written.
///
/// Blocking: the engine runs the load and the query on the calling thread, so a wire handler
/// calls this on a blocking task.
pub fn serve(
    context: &SessionContext,
    database: &InstanceDatabase,
    facts: &SessionFacts,
    call: &AwsS3Call,
) -> Result<StatementAnswer, EngineError> {
    // The functions come with the extension: a database that has not installed it has no
    // such schema, which is what PostgreSQL reports about the call.
    if !super::server_statements::extension_is_installed(database, facts, AWS_S3_SCHEMA)? {
        return Err(EngineError::new(
            EngineErrorKind::UndefinedObject,
            format!("schema \"{AWS_S3_SCHEMA}\" does not exist"),
        )
        .with_sqlstate("3F000"));
    }
    let capability = call.capability();
    // WHICH role the engine acts as: the one the resource has associated for this feature,
    // read fresh, so an association dropped under a live session is gone from the next call.
    let Some(role_arn) = context.associated_role_for_feature(capability.feature_name()) else {
        return Err(credentials_not_accessible(capability));
    };
    let object = call.object();
    let object_arn = engine_s3_role::object_arn(&object.bucket, &object.key);
    if engine_s3_role::role_withholds_object_action(
        context.iam.as_ref(),
        &role_arn,
        capability.object_action(),
        &object_arn,
    ) {
        return Err(access_denied(capability, &object_arn));
    }
    let Some(store) = &context.objects else {
        return Err(no_object_store());
    };
    match call {
        AwsS3Call::Import {
            table,
            columns,
            options,
            object,
        } => {
            let body = {
                let held = store.lock();
                let Some(bucket) = held.bucket(&object.bucket) else {
                    return Err(no_such_bucket());
                };
                let Some(stored) = bucket.object(&object.key) else {
                    return Err(no_such_key());
                };
                if stored.body.len() > MAX_LOAD_BYTES {
                    return Err(too_large());
                }
                stored.body.clone()
            };
            let rows = records(options, &body)?;
            let target = CopyTarget {
                table: relation_of(table),
                columns: columns.clone(),
                // The records were read out of the object by the format the CALL named; what
                // the load does with them is the same either way.
                format: options.clone(),
            };
            let loaded = super::pg_copy::load_rows(database, facts, &target, &rows)?;
            Ok(StatementAnswer::new(
                "SELECT",
                one_text_row(
                    call.function(),
                    &import_summary(loaded, &target.table, &object.key, body.len()),
                ),
            ))
        }
        AwsS3Call::Export {
            query,
            options,
            object,
        } => {
            // The query is this session's own statement: it is run the way every other
            // statement of the session is, so what is exported is what the client would have
            // read back.
            //
            // It is not run as a CALL OF THIS EXTENSION, though: the statement path it goes
            // down is the one that recognises `aws_s3` calls, so a query that is itself such a
            // call would re-enter this function once per level and the nesting would be the
            // statement's to choose rather than this emulator's. "The SQL query that the
            // PostgreSQL server runs" is a query of the database, so one that names this
            // schema is refused before it runs — the one depth bound this path needs, taken
            // from the text the call carried and not from how far a run has already gone.
            if names_the_schema(query) {
                return Err(nested_export());
            }
            let answer = wire_session::run_statement(database, facts, query, &[])?;
            if answer.result.columns.is_empty() {
                return Err(EngineError::new(
                    EngineErrorKind::Syntax,
                    format!(
                        "the query {AWS_S3_SCHEMA}.{EXPORT_FUNCTION} exports must be one that \
                         returns rows"
                    ),
                )
                .with_sqlstate("42601"));
            }
            let body = rendered(options, &answer.result)?;
            let rows = u64::try_from(answer.result.rows.len()).unwrap_or(u64::MAX);
            // A bucket Amazon S3 does not hold answers the engine's write the way it answers
            // a read of one, so the condition is asked before the object is written.
            if store.lock().bucket(&object.bucket).is_none() {
                return Err(no_such_bucket());
            }
            let written = crate::state::export_objects::write_objects_into(
                store,
                &object.bucket,
                &[crate::state::export_objects::ExportedObject {
                    key: object.key.clone(),
                    body,
                }],
                // The extension's own statement names no KMS key, so the object is stored
                // under the destination bucket's default encryption configuration, exactly
                // as any other write that names none is.
                None,
            );
            if written.objects == 0 {
                return Err(EngineError::new(
                    EngineErrorKind::LimitExceeded,
                    "S3 API returned error: the object store did not take the exported object",
                ));
            }
            Ok(StatementAnswer::new(
                "SELECT",
                export_counts(rows, written.bytes),
            ))
        }
    }
}

/// What a load of an object larger than this endpoint reads is refused with — the same
/// ceiling the other wire's Amazon S3 import is bounded by
/// ([`super::mysql_s3_load::MAX_LOAD_BYTES`]).
fn too_large() -> EngineError {
    EngineError::new(
        EngineErrorKind::LimitExceeded,
        format!("the object named is larger than this endpoint loads ({MAX_LOAD_BYTES} bytes)"),
    )
}

/// The relation a `table_name` argument names: `"<schema>.<table>"` where it carries one,
/// which is the table inside that schema.
///
/// The qualifier is read and dropped, because a database of this endpoint is ONE namespace —
/// the embedded engine has no schema layer, and every relation of a PostgreSQL database here
/// lives in the one every session's `search_path` names (`public`). It is the same reading
/// `TRUNCATE` makes of a qualified table name
/// ([`super::server_statements`]), so a qualified name means one relation whichever statement
/// carries it.
fn relation_of(table: &str) -> String {
    table
        .rsplit('.')
        .next()
        .unwrap_or(table)
        .trim_matches('"')
        .to_string()
}

/// The summary row `aws_s3.table_import_from_s3` answers with: how many rows were imported,
/// into which relation, from which file, and how many bytes that file held.
#[must_use]
fn import_summary(rows: u64, table: &str, key: &str, bytes: usize) -> String {
    format!("{rows} rows imported into relation \"{table}\" from file {key} of {bytes} bytes")
}

/// A one-column, one-row result set: what a function call in a select list answers with.
fn one_text_row(column: &str, value: &str) -> ResultSet {
    ResultSet::of_rows(
        vec![ColumnInfo {
            name: column.to_string(),
            declared_type: Some("text".to_string()),
        }],
        vec![vec![Cell::Text(value.to_string())]],
    )
}

/// The row `aws_s3.query_export_to_s3` answers with: `rows_uploaded`, `files_uploaded` and
/// `bytes_uploaded`, the three columns its record carries. One file is written, which is what
/// an export small enough to be written whole uploads.
fn export_counts(rows: u64, bytes: u64) -> ResultSet {
    let column = |name: &str| ColumnInfo {
        name: name.to_string(),
        declared_type: Some("bigint".to_string()),
    };
    ResultSet::of_rows(
        vec![
            column("rows_uploaded"),
            column("files_uploaded"),
            column("bytes_uploaded"),
        ],
        vec![vec![
            Cell::Integer(i64::try_from(rows).unwrap_or(i64::MAX)),
            Cell::Integer(1),
            Cell::Integer(i64::try_from(bytes).unwrap_or(i64::MAX)),
        ]],
    )
}

/// The records an object's bytes carry, read in the format the options named.
///
/// The `csv` format is read by the `csv` crate, configured from the options themselves, and
/// the text format by `COPY`'s own rules for it: the fields are separated by the delimiter,
/// the NULL string stands for a NULL and the backslash escapes are resolved.
fn records(options: &CopyFormat, body: &[u8]) -> Result<Records, EngineError> {
    let mut rows: Records = Vec::new();
    if options.binary {
        // The BINARY format is the WIRE's copy exchange, in which each value carries the
        // bytes its own type's binary form has: an object this call reads is text, so the
        // format is not one this call serves.
        return Err(EngineError::new(
            EngineErrorKind::FeatureNotSupported,
            "this call reads an object in the text and csv formats only".to_string(),
        )
        .with_sqlstate("0A000"));
    }
    if options.csv {
        let mut reader = ::csv::ReaderBuilder::new()
            .delimiter(options.delimiter)
            .quote(options.quote)
            .escape(Some(options.escape))
            .has_headers(options.header)
            .flexible(true)
            .from_reader(body);
        for (at, record) in reader.records().enumerate() {
            let record = record.map_err(|error| {
                EngineError::new(
                    EngineErrorKind::DataError,
                    format!("the object is not readable as CSV: {error}"),
                )
            })?;
            if rows.len() == MAX_LOAD_ROWS {
                return Err(too_many_rows());
            }
            rows.push(super::pg_copy::CopyRecord {
                // WHERE the reader found this record, which is what a load that stops on it
                // reports: the line the record's own position names, else its place among
                // the records after the header line the format may carry.
                line: record
                    .position()
                    .map_or(at + 1 + usize::from(options.header), |at| {
                        usize::try_from(at.line()).unwrap_or(usize::MAX)
                    }),
                fields: record
                    .iter()
                    .map(|field| match field == options.null {
                        true => None,
                        false => Some(field.to_string()),
                    })
                    .collect(),
            });
        }
        return Ok(rows);
    }
    // The text format is `COPY`'s own, so it is read where that format's one reading lives:
    // the records differ from a client's `COPY … FROM STDIN` only in the delimiter and the NULL
    // string the call named, and in the header line this form may carry.
    let mut rows = super::pg_copy::text_records(
        options.delimiter,
        &options.null,
        options.header,
        body,
    );
    if rows.len() > MAX_LOAD_ROWS {
        return Err(too_many_rows());
    }
    rows.shrink_to_fit();
    Ok(rows)
}

/// What an object carrying more records than this endpoint loads in one call is refused with
/// — the same ceiling the other wire's import is bounded by
/// ([`super::mysql_s3_load::MAX_LOAD_ROWS`]).
fn too_many_rows() -> EngineError {
    EngineError::new(
        EngineErrorKind::LimitExceeded,
        format!(
            "the object carries more records than this endpoint loads in one call \
             ({MAX_LOAD_ROWS})"
        ),
    )
}

/// The bytes an export writes: the query's rows in the format the options named, which is
/// the format `COPY … TO` would have written them in.
///
/// `pub(super)` for the same reason [`read_options`] is: the wire's `COPY … TO STDOUT` streams
/// the rows of a query in those same formats ([`super::pg_copy`]), and one writing of a format
/// is what keeps the two from writing a value differently.
pub(super) fn rendered(options: &CopyFormat, result: &ResultSet) -> Result<Vec<u8>, EngineError> {
    if options.binary {
        // The BINARY format's writing belongs to the copy exchange that streams it
        // ([`super::pg_copy`]), which is where it is written; an object this call writes is
        // text.
        return Err(EngineError::new(
            EngineErrorKind::FeatureNotSupported,
            "this call writes an object in the text and csv formats only".to_string(),
        )
        .with_sqlstate("0A000"));
    }
    if result.rows.len() > MAX_LOAD_ROWS {
        return Err(too_many_rows());
    }
    let mut body: Vec<u8> = Vec::new();
    if options.csv {
        let mut writer = ::csv::WriterBuilder::new()
            .delimiter(options.delimiter)
            .quote(options.quote)
            .from_writer(Vec::new());
        if options.header {
            let names: Vec<String> = result
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect();
            writer.write_record(&names).map_err(not_writable)?;
        }
        for row in &result.rows {
            let fields: Vec<String> = row
                .iter()
                .map(|cell| super::pg_copy::field_text(cell, &options.null))
                .collect();
            writer.write_record(&fields).map_err(not_writable)?;
        }
        body = writer.into_inner().map_err(|error| {
            EngineError::new(
                EngineErrorKind::Internal,
                format!("the exported rows could not be rendered: {error}"),
            )
        })?;
        bound_the_export(body.len())?;
        return Ok(body);
    }
    let separator = char::from(options.delimiter);
    if options.header {
        let names: Vec<&str> = result
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect();
        body.extend_from_slice(names.join(&separator.to_string()).as_bytes());
        body.push(b'\n');
    }
    for row in &result.rows {
        let fields: Vec<String> = row
            .iter()
            .map(|cell| super::pg_copy::written_field(cell, &options.null))
            .collect();
        body.extend_from_slice(fields.join(&separator.to_string()).as_bytes());
        body.push(b'\n');
        bound_the_export(body.len())?;
    }
    bound_the_export(body.len())?;
    Ok(body)
}

/// A `csv` record that could not be written, which is a failure of this endpoint rather than
/// of the query.
fn not_writable(error: ::csv::Error) -> EngineError {
    EngineError::new(
        EngineErrorKind::Internal,
        format!("the exported rows could not be rendered: {error}"),
    )
}

/// How many bytes one export may write, so a query over an unbounded table cannot make this
/// endpoint buffer without bound: the same ceiling an import reads under, and the same order
/// as the bulk load the wire's `COPY` serves.
fn bound_the_export(bytes: usize) -> Result<(), EngineError> {
    match bytes > MAX_LOAD_BYTES {
        true => Err(EngineError::new(
            EngineErrorKind::LimitExceeded,
            format!(
                "the query exports more than this endpoint writes in one object \
                 ({MAX_LOAD_BYTES} bytes)"
            ),
        )),
        false => Ok(()),
    }
}

// One exported value as its field, and one field with the format's backslash escapes written
// back, are the FORMAT's own readings — so both live where the format is read and written
// (`super::pg_copy::field_text`, `super::pg_copy::escaped_field`), and an export cannot write
// a value differently from the way a load reads it.

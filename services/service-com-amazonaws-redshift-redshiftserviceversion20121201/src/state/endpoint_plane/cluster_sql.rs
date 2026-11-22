// SPDX-License-Identifier: CC0-1.0

//! The SQL a session on a cluster's endpoint speaks: AMAZON REDSHIFT's dialect.
//!
//! A client that connects to `Cluster$Endpoint` writes Redshift SQL — a
//! `CREATE TABLE` carrying the storage attributes of this warehouse
//! (`DISTSTYLE`, `DISTKEY`, `SORTKEY`, `ENCODE`), a `::` cast, a
//! schema-qualified name, a `LOCK`, a transaction control the warehouse answers
//! with a warning — and reads the catalogue the warehouse publishes
//! (`PG_TABLE_DEF`). The engine behind the endpoint
//! ([`::service_common::sql_wire_planes::sql_engine`]) is a different SQL
//! engine, so those statements are READ as this dialect before it sees them.
//!
//! That reading is NOT written here. The Amazon Redshift Data API runs the same
//! dialect on the same kind of engine, and its reading of it is this emulator's
//! one statement of what a Redshift statement means
//! (`::service_com_amazonaws_redshiftdata_redshiftdata::state::redshift_sql`):
//! this module CALLS it, and does what the reading asks for over the engine
//! behind this endpoint. A statement no reading applies to goes to the engine
//! exactly as the client wrote it, and what the engine answers — its rows, its
//! counts and its own error messages — is what the client is told.
//!
//! What this module adds is the half that belongs to the endpoint rather than to
//! the dialect:
//!
//! * the catalogue of schemas and the temporary relations this endpoint's engine
//!   holds, which is what decides whether a qualified name reaches a schema's
//!   table ([`Catalogue`]);
//! * `PG_TABLE_DEF`, the view a Redshift client reads a table's columns out of,
//!   materialised from the engine's own catalog
//!   ([`::service_common::sql_wire_planes::catalog_views`], the one reading of
//!   that catalog) immediately before a statement that references it runs;
//! * the sentence `version()` answers, which names Amazon Redshift and the
//!   engine build this service publishes;
//! * the `SQLSTATE` a refused statement is reported under, which for this
//!   warehouse is not always the class a PostgreSQL server would use.

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use ::service_com_amazonaws_redshiftdata_redshiftdata::state::{redshift_sql, TempRelations};
use ::service_common::sql_wire_planes::catalog_views;
use ::service_common::sql_wire_planes::session::Answer;
use ::service_common::sql_wire_planes::sql_engine::{
    Cell, DatabaseEngine, EngineError, EngineErrorKind, EngineView, ResultSet,
};
use ::service_common::sql_wire_planes::{DialectReading, LoginRefusal, Refusal, SqlDialect};

use redshift_sql::{SchemaSet, Translated};

/// The view a Redshift client reads a table's columns out of, by the name a
/// statement references it under.
///
/// Source: the Amazon Redshift *System catalog tables* reference — "PG_TABLE_DEF
/// … stores information about table columns" — whose columns are the ones
/// [`materialise_table_def`] declares.
const TABLE_DEF_VIEW: &str = "pg_table_def";

/// The schema prefix this endpoint's engine holds the temporary relations of its
/// connection under.
///
/// The engine behind the endpoint runs ONE connection for the database, so the
/// temporary relations of this endpoint are that connection's: they are held
/// under one prefix, spelled after the schema PostgreSQL names a session's
/// temporary relations with (`pg_temp`), and the catalogue views leave a
/// relation held under it out of the database's own catalogue.
const TEMPORARY_PREFIX: &str = "pg_temp_endpoint";

/// How many schemas the database behind one endpoint may hold.
///
/// A client creates them one statement at a time, so the catalogue is bounded
/// like every other thing a caller can multiply; a `CREATE SCHEMA` past this is
/// refused rather than reported as created.
const MAX_SCHEMAS: usize = 64;

/// How many temporary relations the connection behind one endpoint may hold,
/// bounded for the same reason.
const MAX_TEMPORARY_RELATIONS: usize = 64;

/// The sentence a Redshift backend reports for itself, which is what `version()`
/// answers.
///
/// Source: the Amazon Redshift Database Developer Guide, "Amazon Redshift and
/// PostgreSQL" — Amazon Redshift is based on PostgreSQL 8.0.2, and its
/// `version()` reports that PostgreSQL version, the platform it was built on and
/// then the Amazon Redshift engine build. The PostgreSQL version is the one the
/// endpoint publishes as `server_version`
/// ([`super::CLUSTER_SERVER_VERSION`]) and the build is the engine build this
/// service publishes for its cluster version
/// ([`crate::state::PUBLISHED_ENGINE_BUILD`]), so the sentence cannot disagree
/// with either.
fn redshift_version_sentence(server_version: &str) -> String {
    format!(
        "PostgreSQL {server_version} on i686-pc-linux-gnu, compiled by GCC gcc (GCC) 3.4.2 \
         20041017 (Red Hat 3.4.2-6.fc3), Redshift {}",
        crate::state::PUBLISHED_ENGINE_BUILD
    )
}

/// The catalogue the database behind ONE endpoint holds beside its tables: the
/// schemas a `CREATE SCHEMA` added, and the temporary relations its connection
/// holds.
///
/// Both decide how a NAME resolves, so they are held for as long as the endpoint
/// is: a qualified name reaches a schema's table only while the database holds
/// that schema, and a temporary relation is resolved only while the connection
/// holds it.
#[derive(Debug)]
struct Catalogue {
    /// The schemas the database holds, which a qualified name is resolved
    /// against.
    schemas: SchemaSet,
    /// The temporary relations the endpoint's connection holds, as the caller
    /// named them.
    temporary: BTreeSet<String>,
}

/// A database holds the CATALOGUE SCHEMAS from the moment it is created: a
/// client reads `information_schema` without creating anything, so the schema a
/// statement qualifies those views with is one this database holds — which is
/// what makes `information_schema.columns` resolve to the view this endpoint
/// serves rather than to a relation of a schema nobody made.
impl Default for Catalogue {
    fn default() -> Self {
        Self {
            schemas: [catalog_views::INFORMATION_SCHEMA.to_string()]
                .into_iter()
                .collect(),
            temporary: BTreeSet::new(),
        }
    }
}

/// The dialect of one cluster's engine: Amazon Redshift's.
#[derive(Debug, Default)]
pub struct ClusterSql {
    catalogue: Mutex<Catalogue>,
}

impl ClusterSql {
    /// The dialect of a cluster whose database holds nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SqlDialect for ClusterSql {
    fn read(
        &self,
        statement: &str,
        engine: &DatabaseEngine,
        view: &mut EngineView,
    ) -> DialectReading {
        if let Err(error) = prepare_catalogue(statement, engine, view) {
            return DialectReading::Refused(error);
        }
        let mut held = self.catalogue.lock().unwrap_or_else(PoisonError::into_inner);
        let translated = {
            let temporary = TempRelations::held_by(TEMPORARY_PREFIX, &held.temporary);
            redshift_sql::translate(statement, &held.schemas, &temporary)
        };
        match translated {
            Translated::Run(sql) => DialectReading::Run(sql),
            // A temporary relation belongs to the connection that created it, so
            // the name the caller gave it is recorded here: the next statement
            // of this endpoint resolves it to the relation the engine really
            // holds.
            Translated::RunCreatingTemp { sql, relation } => {
                if held.temporary.len() >= MAX_TEMPORARY_RELATIONS {
                    return DialectReading::Refused(EngineError::new(
                        EngineErrorKind::LimitExceeded,
                        format!(
                            "this connection may hold at most {MAX_TEMPORARY_RELATIONS} \
                             temporary relations at once"
                        ),
                    ));
                }
                held.temporary.insert(relation.to_ascii_lowercase());
                DialectReading::Run(sql)
            }
            Translated::CreateSchema {
                name,
                if_not_exists,
            } => create_schema(&mut held, &name, if_not_exists),
            Translated::DropSchema {
                names,
                if_exists,
                cascade,
            } => drop_schemas(&mut held, engine, view, &names, if_exists, cascade),
            // "LOCK … Restricts access to a database table": the engine behind
            // this endpoint runs ONE connection for the database, so no other
            // session's statement can run while this one holds it and the
            // access the statement restricts is already restricted for the
            // whole of its transaction. The tables it names are RESOLVED all
            // the same — a `LOCK` on a table that is not there is an error on
            // the real service too — so the statement is reported as taken only
            // when they are.
            Translated::Lock { tables, .. } => match resolve_relations(engine, view, &tables) {
                Ok(()) => DialectReading::Answered(answered("LOCK TABLE")),
                Err(error) => DialectReading::Refused(error),
            },
            // A database is a database of the WAREHOUSE, and this endpoint is
            // the endpoint of ONE of them: the database the cluster was created
            // with ([`DatabaseFacts::master_database_name`]). Nothing here can
            // add another, so nothing here claims to.
            Translated::CreateDatabase { name } => DialectReading::Refused(EngineError::new(
                EngineErrorKind::FeatureNotSupported,
                format!(
                    "database \"{name}\" cannot be created on this connection: this endpoint \
                     serves the database the cluster was created with"
                ),
            )),
            // "BEGIN … There is no effect on any transaction that is already in
            // progress", and a `COMMIT` or `ROLLBACK` outside a transaction is
            // likewise answered with a warning rather than an error — so a
            // control with nothing to do reports the statement as run, where
            // the engine would refuse it.
            Translated::Transaction(control) => {
                let verb = control.as_sql();
                if control.opens() == !engine.is_autocommit() {
                    return DialectReading::Answered(answered(verb));
                }
                DialectReading::Run(verb.to_string())
            }
            // The statements of the warehouse's own vocabulary that reach
            // Amazon S3 or the AWS Glue Data Catalog. They are the Data API's
            // to run — that is where this emulator's loads, exports and
            // external schemas live — so a session on this endpoint is told so
            // rather than answered as though a load had happened.
            Translated::Warehouse(statement) => DialectReading::Refused(EngineError::new(
                EngineErrorKind::FeatureNotSupported,
                format!(
                    "{} is not served on a session of this cluster's endpoint; submit it through \
                     the Amazon Redshift Data API",
                    warehouse_statement_verb(&statement)
                ),
            )),
        }
    }

    fn version_sentence(&self, server_version: &str) -> Option<String> {
        Some(redshift_version_sentence(server_version))
    }

    /// Amazon Redshift reports a row it REFUSED in the internal-error class
    /// (`XX000`, its "Invalid operation"), not in the integrity classes a
    /// PostgreSQL server uses for them: a client that branches on the SQLSTATE
    /// of a refused insert reads `XX000` from a cluster. Every other class —
    /// the syntax error, the undefined relation, the cancelled statement — is
    /// the one PostgreSQL's own appendix names, so the wire's mapping stands.
    ///
    /// A refusal the warehouse has its OWN sentence and error number for is
    /// reported as the warehouse reports it ([`refused_row`]).
    fn refusal(&self, error: &EngineError) -> Option<Refusal> {
        match error.kind {
            EngineErrorKind::NotNullViolation => Some(refused_row(
                NULL_INTO_NOT_NULL_CODE,
                null_into_not_null_message(&error.message),
            )),
            EngineErrorKind::CheckViolation
            | EngineErrorKind::UniqueViolation
            | EngineErrorKind::ForeignKeyViolation
            | EngineErrorKind::DataError => Some(Refusal {
                sqlstate: INVALID_OPERATION_SQLSTATE,
                message: error.message.clone(),
                detail: None,
            }),
            // The two classes the warehouse has its own sentence for, which it
            // takes from PostgreSQL: what the engine behind this endpoint wrote
            // is its own, and the client is told what Amazon Redshift says.
            EngineErrorKind::Syntax | EngineErrorKind::UndefinedObject => {
                warehouse_sentence(&error.message).map(|(sqlstate, message)| Refusal {
                    sqlstate,
                    message,
                    detail: None,
                })
            }
            _ => None,
        }
    }

    /// The database user a credential this cluster's control plane minted logs
    /// on as.
    ///
    /// `GetClusterCredentials` "returns the database user name prefixed with
    /// `IAM:` if `AutoCreate` is `False` or `IAMA:` if `AutoCreate` is `True`"
    /// (`smithy_query(query="doc", operation="GetClusterCredentials")`): the
    /// `IAMA:` prefix says the user is CREATED on the way in, and the user it
    /// creates is that identity's own database user — the one under the plain
    /// `IAM:` prefix. So a session that logs on with an auto-created credential
    /// runs as that user, which is what the engine's session functions answer
    /// and what a `GRANT` to it names. Every other login runs as the name it
    /// logged on under.
    fn session_user(&self, login: &str) -> String {
        match login.strip_prefix(super::AUTO_CREATED_DB_USER_PREFIX) {
            Some(name) => format!(
                "{}{name}",
                ::service_common::redshift_database_users::IAM_IDENTITY_DB_USER_PREFIX
            ),
            None => login.to_string(),
        }
    }

    /// A login with a TEMPORARY CREDENTIAL the control plane minted is an IAM
    /// authentication, and a cluster refuses one as that: the account it names
    /// is an IAM identity's database user, and what failed is the
    /// authentication of that identity rather than a password the database
    /// holds for an account of its own. Every other login — the cluster's admin
    /// user — is refused in the words the wire itself states, and so is every
    /// refusal that is not an authentication failure.
    fn login_refusal(&self, refusal: &LoginRefusal, user: &str) -> Option<(&'static str, String)> {
        if !matches!(refusal, LoginRefusal::AuthenticationFailed { .. })
            || !is_iam_database_user(user)
        {
            return None;
        }
        Some((
            IAM_AUTHENTICATION_SQLSTATE,
            format!("IAM authentication failed for user \"{user}\""),
        ))
    }
}

/// The `SQLSTATE` an IAM authentication failure is reported under:
/// `invalid_authorization_specification`, the class of a login the server will
/// not accept the identity of — as opposed to `invalid_password`, which is an
/// account whose password the database itself holds.
///
/// Source: the codes in PostgreSQL's "Appendix A. Error Codes", which Amazon
/// Redshift's wire carries.
const IAM_AUTHENTICATION_SQLSTATE: &str = "28000";

/// Whether a login name is the database user of an IAM identity, which is what
/// a credential minted by `GetClusterCredentials` or
/// `GetClusterCredentialsWithIAM` logs on as.
///
/// The prefixes are the ones those operations document for the user they report
/// ([`super::AUTO_CREATED_DB_USER_PREFIX`], and the identity and role prefixes
/// of the one mapping from an IAM identity to a database user).
fn is_iam_database_user(user: &str) -> bool {
    [
        super::AUTO_CREATED_DB_USER_PREFIX,
        ::service_common::redshift_database_users::IAM_IDENTITY_DB_USER_PREFIX,
        ::service_common::redshift_database_users::IAM_ROLE_DB_USER_PREFIX,
    ]
    .iter()
    .any(|prefix| user.starts_with(prefix))
}

/// The `SQLSTATE` and the sentence Amazon Redshift reports a statement it could
/// not READ under, from what the engine behind this endpoint wrote for the same
/// refusal.
///
/// Source: the sentences PostgreSQL — which this warehouse is derived from and
/// whose wire it speaks — words these refusals with, and the classes its
/// "Appendix A. Error Codes" gives them: a relation, a column, a function or an
/// index the database does not hold (`undefined_table`, `undefined_column`,
/// `undefined_function`, `undefined_object`), and a statement whose text it
/// could not read (`syntax_error`), which names the token it stopped at.
///
/// The name and the token are the ones the ENGINE named for the same refusal —
/// the client's own, read out of its own statement. A sentence of any other
/// shape is left as the engine wrote it rather than re-worded into a sentence
/// about something this emulator did not read.
fn warehouse_sentence(engine_message: &str) -> Option<(&'static str, String)> {
    let named = |prefix: &str| -> Option<String> {
        engine_message
            .strip_prefix(prefix)
            .map(|named| named.trim().to_string())
            .filter(|named| !named.is_empty())
    };
    if let Some(relation) = named("no such table:") {
        return Some(("42P01", format!("relation \"{relation}\" does not exist")));
    }
    if let Some(relation) = named("no such view:") {
        return Some(("42P01", format!("relation \"{relation}\" does not exist")));
    }
    if let Some(column) = named("no such column:") {
        return Some(("42703", format!("column \"{column}\" does not exist")));
    }
    if let Some(function) = named("no such function:") {
        return Some(("42883", format!("function {function} does not exist")));
    }
    if let Some(index) = named("no such index:") {
        return Some(("42704", format!("index \"{index}\" does not exist")));
    }
    // `near "<token>": syntax error`, which is where the engine stopped reading.
    let token = engine_message
        .strip_prefix("near ")
        .and_then(|rest| rest.strip_suffix(": syntax error"))
        .map(str::trim)
        .filter(|token| !token.is_empty())?;
    Some(("42601", format!("syntax error at or near {token}")))
}

/// The `SQLSTATE` Amazon Redshift reports its own "Invalid operation" refusals
/// under: the internal-error class, which is the class a cluster answers a
/// refused row in rather than the integrity classes PostgreSQL uses.
const INVALID_OPERATION_SQLSTATE: &str = "XX000";

/// The Amazon Redshift error number for a null written into a column that does
/// not admit one.
///
/// Source: the error the warehouse reports for it — `Cannot insert a NULL value
/// into column <column>`, error 8007 of the internal errors its
/// `STL_ERROR`/detail block reports.
const NULL_INTO_NOT_NULL_CODE: i64 = 8007;

/// How Amazon Redshift reports a refusal of its own: the sentence as the
/// message, and the DETAIL block that carries the error number a client reads to
/// tell one internal-error refusal from another.
///
/// The block carries what can be stated about this refusal and nothing else: the
/// sentence, the number, and the stage the statement was in when it was refused.
fn refused_row(code: i64, message: String) -> Refusal {
    let detail = format!(
        "\n  -----------------------------------------------\n  \
         error:  {message}\n  code:      {code}\n  context:   query execution\n  \
         -----------------------------------------------\n"
    );
    Refusal {
        sqlstate: INVALID_OPERATION_SQLSTATE,
        message,
        detail: Some(detail),
    }
}

/// Amazon Redshift's sentence for a null written into a column that does not
/// admit one, which names the COLUMN the row was refused for.
///
/// The column is the one the engine named in its own message for the same
/// refusal (`NOT NULL constraint failed: <relation>.<column>`); a message that
/// does not name one at all is reported as the engine wrote it, because a
/// sentence naming a column this emulator did not read would be a worse answer
/// than the engine's own.
fn null_into_not_null_message(engine_message: &str) -> String {
    let column = engine_message
        .rsplit_once(':')
        .map(|(_, named)| named.trim())
        .and_then(|named| named.rsplit('.').next())
        .filter(|column| !column.is_empty() && !column.contains(' '));
    match column {
        Some(column) => format!("Cannot insert a NULL value into column {column}"),
        None => engine_message.to_string(),
    }
}

/// A statement the dialect carried out itself, reported under `verb`.
fn answered(verb: &str) -> Answer {
    Answer {
        verb: verb.to_string(),
        result: ResultSet::default(),
    }
}

/// `CREATE SCHEMA`: add a schema to the database's own catalogue.
///
/// The catalogue is what decides whether a qualified name reaches a schema's
/// table, so creating one is a real change to this database and not a statement
/// reported as run and then forgotten.
fn create_schema(held: &mut Catalogue, name: &str, if_not_exists: bool) -> DialectReading {
    if redshift_sql::holds_schema(&held.schemas, name) {
        // "IF NOT EXISTS … the command makes no changes and returns a message
        // that the schema exists, rather than terminating with an error."
        if if_not_exists {
            return DialectReading::Answered(answered("CREATE SCHEMA"));
        }
        return DialectReading::Refused(EngineError::new(
            EngineErrorKind::DuplicateObject,
            format!("schema \"{name}\" already exists"),
        ));
    }
    if held.schemas.len() >= MAX_SCHEMAS {
        return DialectReading::Refused(EngineError::new(
            EngineErrorKind::LimitExceeded,
            format!("schema limit exceeded: a database holds at most {MAX_SCHEMAS} schemas"),
        ));
    }
    held.schemas.insert(name.to_string());
    DialectReading::Answered(answered("CREATE SCHEMA"))
}

/// `DROP SCHEMA`: remove schemas from the database's catalogue.
///
/// `CASCADE` drops the tables each schema holds with it, which is what makes the
/// schema's name reusable; the default `RESTRICT` refuses a schema that still
/// holds objects, so nothing is dropped silently.
fn drop_schemas(
    held: &mut Catalogue,
    engine: &DatabaseEngine,
    view: &mut EngineView,
    names: &[String],
    if_exists: bool,
    cascade: bool,
) -> DialectReading {
    for name in names {
        if !redshift_sql::holds_schema(&held.schemas, name) {
            if if_exists {
                continue;
            }
            return DialectReading::Refused(EngineError::new(
                EngineErrorKind::UndefinedObject,
                format!("schema \"{name}\" does not exist"),
            ));
        }
        let objects = match schema_objects(engine, view, name) {
            Ok(objects) => objects,
            Err(error) => return DialectReading::Refused(error),
        };
        if !objects.is_empty() && !cascade {
            return DialectReading::Refused(EngineError::new(
                EngineErrorKind::DataError,
                format!("cannot drop schema {name} because other objects depend on it"),
            ));
        }
        for (object, kind) in objects {
            let statement = match kind.as_str() {
                "view" => format!("DROP VIEW {}", quoted(&object)),
                _ => format!("DROP TABLE {}", quoted(&object)),
            };
            if let Err(error) = engine.execute_as(view, &statement) {
                return DialectReading::Refused(error);
            }
        }
        held.schemas
            .retain(|schema| !schema.eq_ignore_ascii_case(name));
    }
    DialectReading::Answered(answered("DROP SCHEMA"))
}

/// The tables and views one schema holds, by engine name and kind: the objects
/// held under that schema's qualified prefix, read from the engine's own
/// catalog.
fn schema_objects(
    engine: &DatabaseEngine,
    view: &mut EngineView,
    schema: &str,
) -> Result<Vec<(String, String)>, EngineError> {
    let listing = engine.execute_as(
        view,
        &format!(
            "SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') \
             AND instr(name, '.') > 0 AND substr(name, 1, instr(name, '.') - 1) = {}",
            literal(schema)
        ),
    )?;
    Ok(listing
        .rows
        .iter()
        .filter_map(|row| match (row.first(), row.get(1)) {
            (Some(Cell::Text(name)), Some(Cell::Text(kind))) => {
                Some((name.clone(), kind.clone()))
            }
            _ => None,
        })
        .collect())
}

/// Resolve the relations a statement names against the engine's own catalog,
/// answering the engine's own message for one it does not hold.
fn resolve_relations(
    engine: &DatabaseEngine,
    view: &mut EngineView,
    relations: &[String],
) -> Result<(), EngineError> {
    for relation in relations {
        engine.execute_as(view, &format!("SELECT 1 FROM {} LIMIT 0", quoted(relation)))?;
    }
    Ok(())
}

/// Put the catalogue views a statement REFERENCES on the engine before it runs:
/// the standard `information_schema` views and the warehouse's own
/// `PG_TABLE_DEF`.
///
/// Rebuilt per statement, because the catalog has moved on since the last
/// statement read it. A statement that references none of them prepares nothing.
fn prepare_catalogue(
    statement: &str,
    engine: &DatabaseEngine,
    view: &mut EngineView,
) -> Result<(), EngineError> {
    let relations = redshift_sql::referenced_relations(statement);
    if relations.is_empty() {
        return Ok(());
    }
    let standard = catalog_views::referenced(&relations);
    if !standard.is_empty() {
        let catalog = view.database.clone();
        catalog_views::materialise(engine, view, &catalog, &standard)?;
    }
    if relations
        .iter()
        .any(|relation| relation.eq_ignore_ascii_case(TABLE_DEF_VIEW))
    {
        materialise_table_def(engine, view)?;
    }
    Ok(())
}

/// `PG_TABLE_DEF`: one row for each column of each table the database holds.
///
/// Every column of the view, and every column's type, is the one the Amazon
/// Redshift *System catalog tables* reference publishes for it: `schemaname`,
/// `tablename`, `column`, `type`, `encoding`, `distkey`, `sortkey` and
/// `notnull`. The rows are the engine's own catalog, read through the one
/// reading of it ([`catalog_views::declared_columns`]), with each column's type
/// under the name the warehouse reports it by.
///
/// The three columns that describe how a CLUSTER STORES a column — its
/// compression encoding, whether it is the distribution key and its position in
/// the sort key — are not facts of this engine, which stores a table one way and
/// is handed a `CREATE TABLE` with its storage attributes already read off it:
/// they carry the values a column that declares none has, which is what every
/// column of this engine is.
fn materialise_table_def(
    engine: &DatabaseEngine,
    view: &mut EngineView,
) -> Result<(), EngineError> {
    let declared = catalog_views::declared_columns(engine, view)?;
    let rows: Vec<String> = declared
        .iter()
        .map(|column| {
            format!(
                "({}, {}, {}, {}, {}, 0, 0, {})",
                literal(&column.table_schema),
                literal(&column.table_name),
                literal(&column.column_name),
                literal(&column.full_data_type()),
                literal(NO_COMPRESSION_ENCODING),
                i64::from(column.not_null),
            )
        })
        .collect();
    catalog_views::rebuild(
        engine,
        view,
        TABLE_DEF_VIEW,
        "schemaname VARCHAR(128), tablename VARCHAR(128), \"column\" VARCHAR(128), type TEXT, \
         encoding CHARACTER(32), distkey BOOLEAN, sortkey INTEGER, \"notnull\" BOOLEAN",
        &rows,
    )
}

/// What `PG_TABLE_DEF` reports as the compression encoding of a column that is
/// stored uncompressed, which every column of this engine is.
///
/// Source: the Amazon Redshift *Compression encodings* reference — `RAW` is "no
/// compression", and the catalog reports it as `none`.
const NO_COMPRESSION_ENCODING: &str = "none";

/// The verb a statement of the warehouse's own vocabulary is named by, for the
/// refusal that says where it is served.
fn warehouse_statement_verb(
    statement: &::service_com_amazonaws_redshiftdata_redshiftdata::state::warehouse_statements::WarehouseStatement,
) -> &'static str {
    use ::service_com_amazonaws_redshiftdata_redshiftdata::state::warehouse_statements::WarehouseStatement;
    match statement {
        WarehouseStatement::Copy(_) => "COPY",
        WarehouseStatement::Unload(_) => "UNLOAD",
        WarehouseStatement::CreateExternalSchema(_) => "CREATE EXTERNAL SCHEMA",
        WarehouseStatement::CreateExternalTable(_) => "CREATE EXTERNAL TABLE",
        WarehouseStatement::Set { .. } => "SET",
        WarehouseStatement::Reset { .. } => "RESET",
        WarehouseStatement::Show { .. } => "SHOW",
    }
}

/// `name` as one quoted identifier: a name carrying the schema it is in has to
/// be, since unquoted it would address a schema the engine does not have.
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `text` as a SQL string literal.
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sentence `version()` answers names the PostgreSQL version the
    /// endpoint publishes AND Amazon Redshift's own engine build, which is what
    /// a client reads to know which warehouse it is connected to.
    #[test]
    fn the_version_sentence_names_redshift_and_the_published_build() {
        let sentence = redshift_version_sentence(super::super::CLUSTER_SERVER_VERSION);
        assert!(
            sentence.contains("Redshift"),
            "case the version sentence: it must name Redshift, got {sentence:?}"
        );
        assert!(
            sentence.contains(crate::state::PUBLISHED_ENGINE_BUILD),
            "case the version sentence: it must carry the published engine build {:?}, got \
             {sentence:?}",
            crate::state::PUBLISHED_ENGINE_BUILD
        );
        assert!(
            sentence.starts_with(&format!("PostgreSQL {}", super::super::CLUSTER_SERVER_VERSION)),
            "case the version sentence: it must open with the PostgreSQL version the endpoint \
             publishes, got {sentence:?}"
        );
    }

    /// A refused row is reported as Amazon Redshift reports it — its own
    /// sentence, its own error number in the detail block, and the
    /// internal-error class — and every other class is left to the wire's own
    /// mapping.
    #[test]
    fn a_refused_row_carries_the_warehouses_own_refusal() {
        let dialect = ClusterSql::new();
        for (case, kind, message, expected) in [
            (
                "a null into a NOT NULL column",
                EngineErrorKind::NotNullViolation,
                "NOT NULL constraint failed: orders.total",
                Some(("XX000", "Cannot insert a NULL value into column total", true)),
            ),
            (
                "a null the engine named no column for",
                EngineErrorKind::NotNullViolation,
                "the row was refused",
                Some(("XX000", "the row was refused", true)),
            ),
            (
                "a check violation",
                EngineErrorKind::CheckViolation,
                "CHECK constraint failed: positive",
                Some(("XX000", "CHECK constraint failed: positive", false)),
            ),
            (
                "a syntax error",
                EngineErrorKind::Syntax,
                "near \"SELCT\": syntax error",
                Some(("42601", "syntax error at or near \"SELCT\"", false)),
            ),
            (
                "an undefined relation",
                EngineErrorKind::UndefinedObject,
                "no such table: orders",
                Some(("42P01", "relation \"orders\" does not exist", false)),
            ),
            (
                "an undefined column",
                EngineErrorKind::UndefinedObject,
                "no such column: total",
                Some(("42703", "column \"total\" does not exist", false)),
            ),
            (
                "a sentence of no shape this reads",
                EngineErrorKind::Syntax,
                "the engine stopped",
                None,
            ),
        ] {
            let error = EngineError::new(kind, message);
            let refusal = dialect.refusal(&error);
            match (expected, refusal) {
                (None, reported) => assert!(
                    reported.is_none(),
                    "case {case:?}: expected the wire's own mapping, got {reported:?}"
                ),
                (Some((state, sentence, detailed)), Some(reported)) => {
                    assert_eq!(
                        (reported.sqlstate, reported.message.as_str()),
                        (state, sentence),
                        "case {case:?}: expected {state} {sentence:?}, got {reported:?}"
                    );
                    assert_eq!(
                        reported.detail.is_some(),
                        detailed,
                        "case {case:?}: expected a detail block: {detailed}, got {reported:?}"
                    );
                    if detailed {
                        let detail = reported.detail.unwrap_or_default();
                        assert!(
                            detail.contains(&format!("code:      {NULL_INTO_NOT_NULL_CODE}")),
                            "case {case:?}: the detail block must carry the warehouse's error \
                             number, got {detail:?}"
                        );
                    }
                }
                (Some(expected), None) => panic!(
                    "case {case:?}: expected {expected:?}, got the wire's own mapping"
                ),
            }
        }
    }

    /// A login with a credential the control plane minted is refused as an IAM
    /// authentication failure; a login of the cluster's own admin user is left
    /// to the wire's own words, and so is a refusal that is not an
    /// authentication failure.
    #[test]
    fn an_iam_login_is_refused_as_an_iam_authentication() {
        let dialect = ClusterSql::new();
        let failed = LoginRefusal::AuthenticationFailed {
            user: "whoever".to_string(),
            password_given: true,
        };
        for (case, user, expected) in [
            (
                "an auto-created credential",
                "IAMA:scopeduser",
                Some("IAM authentication failed for user \"IAMA:scopeduser\"".to_string()),
            ),
            (
                "a credential for an existing user",
                "IAM:analyst",
                Some("IAM authentication failed for user \"IAM:analyst\"".to_string()),
            ),
            (
                "a role identity",
                "IAMR:WarehouseAdmin",
                Some("IAM authentication failed for user \"IAMR:WarehouseAdmin\"".to_string()),
            ),
            ("the cluster's admin user", "adminuser", None),
        ] {
            let reported = dialect.login_refusal(&failed, user);
            assert_eq!(
                reported.as_ref().map(|(_, message)| message.clone()),
                expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
        assert!(
            dialect
                .login_refusal(
                    &LoginRefusal::UnknownDatabase {
                        database: "nowhere".to_string()
                    },
                    "IAM:analyst"
                )
                .is_none(),
            "case a database that is not there: expected the wire's own words"
        );
    }

    /// A credential minted with `AutoCreate` logs on under the `IAMA:` prefix
    /// and runs as the identity's own database user; every other login runs as
    /// the name it logged on under.
    #[test]
    fn an_auto_created_credential_runs_as_the_identitys_database_user() {
        let dialect = ClusterSql::new();
        for (case, login, expected) in [
            ("an auto-created credential", "IAMA:analyst", "IAM:analyst"),
            ("a credential for an existing user", "IAM:analyst", "IAM:analyst"),
            ("a role identity", "IAMR:WarehouseAdmin", "IAMR:WarehouseAdmin"),
            ("the cluster's admin user", "adminuser", "adminuser"),
            ("a name that merely starts the same", "IAMAnalyst", "IAMAnalyst"),
        ] {
            let ran_as = dialect.session_user(login);
            assert_eq!(
                ran_as, expected,
                "case {case:?}: expected {login:?} to run as {expected:?}, got {ran_as:?}"
            );
        }
    }
}

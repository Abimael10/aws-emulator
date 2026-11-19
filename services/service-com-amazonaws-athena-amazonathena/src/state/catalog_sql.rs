// SPDX-License-Identifier: CC0-1.0

//! Athena's STATEMENT layer: what a submitted statement means to the DATA CATALOG, and
//! how it is run.
//!
//! Athena is a query service over a data catalog, not a database of its own: a statement
//! either DEFINES or REPORTS a catalog object, or it READS data through relations the
//! catalog resolves. So a statement is first read as what it is
//! ([`statement::read`]), and then:
//!
//! * a catalog statement — `CREATE DATABASE`, `DROP DATABASE`, `SHOW DATABASES`,
//!   `CREATE EXTERNAL TABLE`, `DROP TABLE`, `CREATE VIEW`, `DROP VIEW`,
//!   `SHOW CREATE VIEW`, `SHOW TABLES`, `SHOW VIEWS` — is performed against the AWS Glue
//!   Data Catalog ([`ddl`]), which is the one catalog `AwsDataCatalog` names, so what this
//!   DDL writes is what Glue's own reads report and what Glue writes is what these
//!   statements see;
//! * a statement that reads relations has each one resolved through that same catalog and
//!   the OBJECTS at its location decoded into rows ([`data`]), which the SQL engine then
//!   computes over ([`super::query_engine`]);
//! * anything else — a projection of constants, a statement over a table an earlier
//!   statement defined in the engine itself — is the engine's own business and runs as
//!   submitted.
//!
//! A name the catalog cannot resolve is a FAILED execution carrying Athena's own error
//! classification for it ([`Failure`]): the category, the error type and the message of
//! that failure class, with the identifiers and the line and column taken from the
//! submitted statement.

/// The objects under a catalog table's location, as the rows they hold.
pub mod data;
/// The statements the Data Catalog performs.
pub mod ddl;
/// Why an unregistered relation fails on the relation: the database the catalog carries.
mod default_database;
/// What Athena says about text its query grammar cannot read as a statement.
pub mod grammar;
/// The `CREATE DATABASE` clauses Hive's DDL grammar takes beyond the general parser.
pub mod hive_database;
/// What one submitted statement is, and the plan for running it.
pub mod statement;

use ::std::collections::{HashMap, HashSet};
use ::std::time::{Duration, Instant, SystemTime};

use ::service_com_amazonaws_glue_awsglue::state as glue_state;
use ::service_com_amazonaws_s3_amazons3::state as s3_state;

use super::lake_formation::{DataLakeAccess, TableAccess};
use super::query_engine::{
    self, Completion, DatabaseHandle, FailureCause, LocalColumn, LocalObject, LocalTable, Outcome,
    StoredResultSet,
};
use data::{Budget, DataSource};
use statement::{
    CatalogStatement, Plan, Position, QueryContext, Reading, Relation, RelationalStatement,
};

/// The `AthenaError$ErrorCategory` of a failure the SERVICE is the cause of: the
/// documented category list is "1 - System, 2 - User, 3 - Other".
pub const ERROR_CATEGORY_SYSTEM: i32 = 1;

/// The `AthenaError$ErrorCategory` of a failure the CALLER's own input is the cause of;
/// see [`ERROR_CATEGORY_SYSTEM`].
pub const ERROR_CATEGORY_USER: i32 = 2;

/// The `AthenaError$ErrorType` Athena publishes for a failure the data catalog LAYER
/// raises rather than the query engine: a task the catalog's own metastore refused, and a
/// catalog name the statement's context addresses that the account has not registered.
///
/// RECORDED behaviour: a wire fixture submits a statement whose `QueryExecutionContext`
/// names a catalog nothing registered, and the execution settles FAILED with this type
/// beside [`ERROR_CATEGORY_USER`].
pub const ERROR_TYPE_CATALOG_LAYER: i32 = 1006;

/// The `AthenaError$ErrorType` Athena publishes for a DDL TASK the catalog's own
/// metastore refused: a statement that would define an object the catalog already holds,
/// and one that would drop a database that still holds tables.
pub const ERROR_TYPE_DDL_TASK: i32 = ERROR_TYPE_CATALOG_LAYER;

/// The `AthenaError$ErrorType` Athena publishes for the NOT-FOUND class: a statement
/// naming a table or a database the data catalog does not hold.
pub const ERROR_TYPE_ENTITY_NOT_FOUND: i32 = 1301;

/// The `AthenaError$ErrorType` Athena publishes for a VIEW a statement addresses that is
/// not the view it needs: one that already exists where a plain `CREATE VIEW` would
/// define it, and one that is not there — or is not a view at all — where a `DROP VIEW` or
/// a `SHOW CREATE VIEW` addresses it.
pub const ERROR_TYPE_VIEW: i32 = 1502;

/// The `AthenaError$ErrorType` Athena publishes for the GENERIC user-error class: a
/// statement the caller has to change, of no narrower published class — which is the
/// class whose messages carry the `GENERIC_USER_ERROR` prefix.
pub const ERROR_TYPE_GENERIC_USER: i32 = 1000;

/// The `AthenaError$ErrorType` Athena publishes for a table DEFINITION the caller got
/// wrong: a `CREATE EXTERNAL TABLE` whose storage the catalog cannot register the table
/// on, such as a `LOCATION` that is not a folder.
pub const ERROR_TYPE_TABLE_DEFINITION: i32 = 1100;

/// The `AthenaError$ErrorType` Athena publishes for a `LOCATION` the object store does not
/// hold at all: a `CREATE EXTERNAL TABLE` whose bucket does not exist, so there is nothing
/// for the table to be registered over.
///
/// RECORDED behaviour, like the category and the wording that go with it
/// ([`LOCATION_BUCKET_ABSENT`]): two wire fixtures submit such a statement — one naming the
/// table in a bucket nothing created, one under `IF NOT EXISTS` over the bucket root — and
/// each settles FAILED with this type beside [`ERROR_CATEGORY_USER`]. It is a type of its
/// own rather than the table-definition one ([`ERROR_TYPE_TABLE_DEFINITION`]) because what
/// the statement declares is a location that is NOT THERE, not a location of the wrong
/// shape.
pub const ERROR_TYPE_LOCATION_NOT_FOUND: i32 = 1306;

/// Athena's message for a `CREATE EXTERNAL TABLE` whose `LOCATION` denotes a file: fixed
/// text with no variable part — it names neither the statement's table nor its location.
const LOCATION_ON_FILE: &str = "Cannot create table on file, please specify a directory";

/// Athena's message for a `CREATE EXTERNAL TABLE` whose `LOCATION` names a bucket the
/// object store does not hold: fixed text with no variable part — it names neither the
/// bucket nor the statement's table. Recorded, with the classification beside it; see
/// [`ERROR_TYPE_LOCATION_NOT_FOUND`].
const LOCATION_BUCKET_ABSENT: &str = "The bucket specified in the ddl statement does not exist";

/// Athena's message for a read of a table the catalog marks as an Iceberg table and holds
/// no metadata location for: fixed text with no variable part — it names neither the
/// database, the table nor the serialization library.
const ICEBERG_WITHOUT_METADATA_LOCATION: &str = "GENERIC_USER_ERROR: Detected Iceberg type \
     table without metadata location. Please make sure an Iceberg-enabled compute engine \
     such as Athena or EMR Spark is used to create the table, or the table is created by \
     using the Iceberg open source AWS library iceberg-aws. Setting table_type parameter \
     in Glue metastore to create an Iceberg table is not supported.";

/// How many levels of view a statement may read through: a view over a view over a table
/// is resolved, and a chain longer than this is reported rather than followed.
const MAX_VIEW_DEPTH: usize = 4;

/// Whether `database` — folded the way the Data Catalog folds a name — is a database the
/// catalog SERVES whether or not it holds a record of it.
///
/// Athena's own `default` database is one: it is the database an unqualified name resolves
/// against when a submission names none of its own
/// ([`crate::state::DEFAULT_QUERY_DATABASE`]), and every account's Data Catalog carries it,
/// so a statement addressing a relation in it addresses a database that IS there and holds
/// exactly the relations the catalog records in it — none at all, where the catalog holds no
/// record of the database. A statement naming a relation there that the catalog does not
/// hold is therefore the not-found refusal for the RELATION, never one for the database: a
/// database the catalog serves does not fail to resolve.
///
/// Only the resolution of a RELATION's database is decided here. A statement that acts on
/// the database RECORD itself — the entries `CREATE DATABASE`, `DROP DATABASE` and
/// `SHOW DATABASES` write and read, which are the same records the catalog's own API writes
/// and reads — acts on what the catalog HOLDS, and this says nothing about those.
pub fn served_without_record(database: &str) -> bool {
    database == glue_state::fold_name(crate::state::DEFAULT_QUERY_DATABASE)
}

/// Why one statement did not run: the message Athena reports as the execution's
/// `AthenaError$ErrorMessage`, the `StateChangeReason` it reports beside it where the two
/// differ, and the classification that failure belongs to.
#[derive(Debug, Clone)]
pub struct Failure {
    /// The message, as Athena words it for this failure class.
    pub message: String,
    /// The execution's `StateChangeReason`, where the engine that performed the statement
    /// reports something other than the message itself — the Hive DDL engine reports the
    /// TASK that failed and the metastore exception it wrapped. `None` where the two are
    /// the same text, which is what every other failure class reports.
    pub reason: Option<String>,
    /// Whose doing the failure was, and the error type Athena publishes for it.
    pub cause: FailureCause,
}

impl Failure {
    /// A failure this EMULATOR is the cause of: a bound it holds a read to, or a
    /// capability it does not have. Neither the caller's statement being wrong nor the
    /// service failing, which is what the third category names.
    pub fn emulator(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            reason: None,
            cause: FailureCause::Emulator,
        }
    }

    /// A failure the submitted statement is the cause of, of no error class Athena
    /// publishes a type for.
    pub fn user(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            reason: None,
            cause: FailureCause::Athena {
                category: ERROR_CATEGORY_USER,
                error_type: None,
            },
        }
    }

    /// A failure of one of Athena's published error classes.
    fn classified(message: String, category: i32, error_type: i32) -> Self {
        Self {
            message,
            reason: None,
            cause: FailureCause::Athena {
                category,
                error_type: Some(error_type),
            },
        }
    }

    /// A failure the Hive DDL engine's own TASK reports: the metastore exception's
    /// `message` is what `AthenaError$ErrorMessage` carries, and the task line — the task
    /// that failed, with the metastore `exception` it wrapped — is what the execution's
    /// `StateChangeReason` carries, because that is what the DDL engine wrote about it.
    fn ddl_task(message: String, exception: String) -> Self {
        Self {
            message,
            reason: Some(format!("{DDL_TASK_FAILED} {exception}")),
            cause: FailureCause::Athena {
                category: ERROR_CATEGORY_USER,
                error_type: Some(ERROR_TYPE_DDL_TASK),
            },
        }
    }

    /// A `CREATE DATABASE` names a database the catalog already holds.
    pub fn database_already_exists(database: &str) -> Self {
        Self::ddl_task(
            format!("Database {database} already exists"),
            format!("AlreadyExistsException(message:Database {database} already exists)"),
        )
    }

    /// A `DROP DATABASE` without `CASCADE` names a database that still holds tables.
    pub fn database_not_empty(database: &str) -> Self {
        Self::ddl_task(
            format!("Cannot drop database {database} as its not empty."),
            format!(
                "MetaException(message:Unable to drop database: \
                 InvalidOperationException(message:Database {database} is not empty.))"
            ),
        )
    }

    /// The statement names a table the data catalog does not hold.
    pub fn table_not_found(relation: &Relation) -> Self {
        Self::classified(
            format!(
                "TABLE_NOT_FOUND: line {}:{}: Table '{}' does not exist",
                relation.at.line,
                relation.at.column,
                relation.qualified()
            ),
            ERROR_CATEGORY_USER,
            ERROR_TYPE_ENTITY_NOT_FOUND,
        )
    }

    /// The statement names a column no relation it reads carries.
    ///
    /// Athena's own class for an unresolvable name, worded like the not-found classes
    /// above: the `COLUMN_NOT_FOUND: ` prefix and "cannot be resolved" are fixed text, the
    /// column is the one the statement wrote, and the line and column are where it wrote
    /// it.
    pub fn column_not_found(at: Position, column: &str) -> Self {
        Self::user(format!(
            "{COLUMN_NOT_FOUND}: line {}:{}: Column '{column}' cannot be resolved",
            at.line, at.column
        ))
    }

    /// The statement names a column the relation it reads DECLARES and the caller may not
    /// see, because the Lake Formation grant it reads that relation through covers other
    /// columns.
    ///
    /// The same class as [`Self::column_not_found`] and the same prefix, with the clause
    /// Athena adds where a name is unresolvable BECAUSE of the reader's permissions — the
    /// one its not-authorized wording closes with, so the reader is not told which of the
    /// two it is. Fixed text either side of the column the statement wrote.
    pub fn column_not_authorized(column: &str) -> Self {
        Self::user(format!(
            "{COLUMN_NOT_FOUND}: Column '{column}' cannot be resolved or requester is not \
             authorized to access requested resources"
        ))
    }

    /// The statement names a database the data catalog does not hold.
    pub fn schema_not_found(relation: &Relation) -> Self {
        Self::schema_not_found_at(relation.at, &relation.database)
    }

    /// The statement names a database the data catalog does not hold, where the database
    /// is named on its own rather than as part of a relation.
    pub fn schema_not_found_at(at: Position, database: &str) -> Self {
        Self::classified(
            format!(
                "SCHEMA_NOT_FOUND: line {}:{}: Schema '{database}' does not exist",
                at.line, at.column
            ),
            ERROR_CATEGORY_USER,
            ERROR_TYPE_ENTITY_NOT_FOUND,
        )
    }

    /// The statement would define a table the data catalog already holds.
    ///
    /// The statement is one the HIVE DDL engine performs, so what it reports is that
    /// engine's own: the metastore exception's message, with the task line that wrapped it
    /// as the state change reason (see [`Self::ddl_task`]).
    pub fn table_already_exists(target: &Relation) -> Self {
        Self::ddl_task(
            format!("Table {} already exists", target.table),
            format!(
                "AlreadyExistsException(message:Table {} already exists)",
                target.table
            ),
        )
    }

    /// A statement the HIVE DDL engine performs names a database the catalog does not
    /// hold.
    ///
    /// That engine resolves the name while it COMPILES the statement, so what it reports is
    /// the semantic error of its own compilation rather than the query engine's planning
    /// error ([`Self::schema_not_found_at`]), which is what a statement the query engine
    /// plans reports instead.
    pub fn hive_database_not_found(database: &str) -> Self {
        Self {
            message: format!("{DATABASE_DOES_NOT_EXIST}: {database}"),
            reason: Some(format!(
                "FAILED: SemanticException [Error {HIVE_DATABASE_NOT_EXISTS}]: \
                 {DATABASE_DOES_NOT_EXIST}: {database}"
            )),
            cause: FailureCause::Athena {
                category: ERROR_CATEGORY_USER,
                error_type: Some(ERROR_TYPE_ENTITY_NOT_FOUND),
            },
        }
    }

    /// A plain `CREATE VIEW` names a view that already exists.
    pub fn view_already_exists(target: &Relation) -> Self {
        Self::classified(
            format!(
                "line {}:{}: View already exists: '{}.{}'",
                target.at.line, target.at.column, target.database, target.table
            ),
            ERROR_CATEGORY_SYSTEM,
            ERROR_TYPE_VIEW,
        )
    }

    /// A statement addresses a view the catalog does not hold.
    pub fn view_not_found(target: &Relation) -> Self {
        Self::classified(
            format!(
                "line {}:{}: View '{}.{}' does not exist",
                target.at.line, target.at.column, target.database, target.table
            ),
            ERROR_CATEGORY_SYSTEM,
            ERROR_TYPE_VIEW,
        )
    }

    /// A statement that addresses a view names an entry that is a TABLE.
    pub fn not_a_view(target: &Relation) -> Self {
        Self::classified(
            format!(
                "line {}:{}: Relation '{}.{}' is a table, not a view",
                target.at.line, target.at.column, target.database, target.table
            ),
            ERROR_CATEGORY_SYSTEM,
            ERROR_TYPE_VIEW,
        )
    }

    /// The statement names a data catalog this account has not registered.
    ///
    /// A classified failure, not an unclassified user one: the catalog layer is what
    /// cannot resolve the name, so the execution carries the error type Athena publishes
    /// for that layer ([`ERROR_TYPE_CATALOG_LAYER`]) and a client can branch on the
    /// category and the type instead of reading the message. The `CATALOG_NOT_FOUND: `
    /// prefix and "Catalog '"/"' does not exist" are fixed text; the catalog is the one
    /// the request named, folded the way the catalog folds a name, and the line and column
    /// are where the statement addressed it.
    pub fn catalog_not_found(at: Position, catalog: &str) -> Self {
        Self::classified(
            format!(
                "CATALOG_NOT_FOUND: line {}:{}: Catalog '{}' does not exist",
                at.line,
                at.column,
                catalog.to_ascii_lowercase()
            ),
            ERROR_CATEGORY_USER,
            ERROR_TYPE_CATALOG_LAYER,
        )
    }

    /// A `CREATE EXTERNAL TABLE` declares a `LOCATION` that denotes a single OBJECT
    /// rather than the folder an external table's data lives under.
    pub fn location_on_file() -> Self {
        Self::classified(
            LOCATION_ON_FILE.to_string(),
            ERROR_CATEGORY_USER,
            ERROR_TYPE_TABLE_DEFINITION,
        )
    }

    /// A `CREATE EXTERNAL TABLE` declares a `LOCATION` whose BUCKET the object store does
    /// not hold, so the statement names no storage the table could be registered over.
    pub fn location_bucket_absent() -> Self {
        Self::classified(
            LOCATION_BUCKET_ABSENT.to_string(),
            ERROR_CATEGORY_USER,
            ERROR_TYPE_LOCATION_NOT_FOUND,
        )
    }

    /// A statement reads a table the catalog marks as an Iceberg table and holds no
    /// Iceberg metadata location for, so there is no table metadata to read it through.
    pub fn iceberg_without_metadata_location() -> Self {
        Self::classified(
            ICEBERG_WITHOUT_METADATA_LOCATION.to_string(),
            ERROR_CATEGORY_USER,
            ERROR_TYPE_GENERIC_USER,
        )
    }

    /// This failure as the outcome of a run that produced nothing.
    fn outcome(self) -> Outcome {
        Outcome::Failed {
            message: self.message,
            reason: self.reason,
            cause: self.cause,
        }
    }
}

/// What the Hive DDL engine writes in front of the metastore exception a failed task
/// wrapped: the task that failed and the return code it failed with. The text is the
/// engine's own and carries nothing from the request; the exception after it is the one
/// the metastore raised.
const DDL_TASK_FAILED: &str =
    "FAILED: Execution Error, return code 1 from org.apache.hadoop.hive.ql.exec.DDLTask.";

/// What the Hive DDL engine says about a database it cannot resolve while compiling a
/// statement, and the error number its own catalogue gives that semantic error.
const DATABASE_DOES_NOT_EXIST: &str = "Database does not exist";

/// The error number of [`DATABASE_DOES_NOT_EXIST`] in the Hive DDL engine's catalogue.
const HIVE_DATABASE_NOT_EXISTS: i32 = 10_072;

/// Everything one run of a statement needs beside the statement itself.
#[derive(Debug, Clone, Copy)]
pub struct Submission<'r> {
    /// The catalog and database an unqualified name in the statement resolves against.
    pub context: &'r QueryContext,
    /// The values bound to the statement's placeholders, in order.
    pub parameters: &'r [String],
    /// What Lake Formation said the caller may read in the Data Catalogs this statement
    /// addresses, resolved before the statement was planned: it decides whether each
    /// relation the statement reads is there for this caller at all, and which of its
    /// columns and rows are.
    pub access: &'r DataLakeAccess,
    /// When the submission was made, which is what a catalog write it performs records.
    pub now: SystemTime,
}

/// Run the statement `reading` planned, and report what it produced.
///
/// The catalog and the object store are reached here rather than in the handler because
/// this is the one place that knows what the statement needs from each: a catalog
/// statement takes the catalog's own guard and writes or reads it; a statement over
/// relations resolves them in the catalog, releases that guard, reads the objects their
/// locations name, and only then hands the SQL to the engine.
///
/// BLOCKING, deliberately, exactly as [`query_engine::run`] is: the engine is
/// synchronous, so a caller runs this on a blocking pool.
pub fn execute<Eng: Send + Sync + 'static>(
    engine: &Eng,
    handle: &DatabaseHandle,
    sql: &str,
    reading: Reading,
    submission: &Submission<'_>,
) -> Completion {
    let Submission {
        context,
        parameters,
        access,
        now,
    } = *submission;
    match reading.plan {
        Plan::Engine => query_engine::run(handle, sql, reading.counts_rows, parameters),
        // A `DROP TABLE` naming a database the catalog does not hold addresses no catalog
        // table, so it is the ENGINE's to perform — the same engine whose own database a
        // `CREATE TABLE` that declares no location defines its table in ([`Plan::Engine`]),
        // which is the only way a relation outside the catalog comes to exist here. Decided
        // before the catalog's guard is taken for the drop itself.
        Plan::Catalog(statement) if drops_a_table_of_the_engines_own(engine, &statement) => {
            query_engine::run(handle, sql, reading.counts_rows, parameters)
        }
        Plan::Catalog(statement) => {
            let started = Instant::now();
            let outcome = match perform(engine, &statement, now) {
                Ok(result) => Outcome::Succeeded {
                    result,
                    update_count: None,
                    data_scanned_bytes: 0,
                    // A catalog statement names no relation and materialises no object, so
                    // it read no rows of stored data.
                    input_rows: 0,
                },
                Err(failure) => failure.outcome(),
            };
            Completion {
                outcome,
                engine_time: started.elapsed(),
                // A catalog statement names no relation to resolve and reads no data: the
                // whole of its run is the task the catalog performs, so the planning this
                // run did is the READING of the statement, which the caller measured.
                planning_time: Duration::ZERO,
            }
        }
        Plan::Relations(relational) => {
            let started = Instant::now();
            match resolved_sql(engine, handle, *relational, context, access) {
                Ok(prepared) => {
                    // Resolving the statement's relations against the catalog and reading
                    // the objects at their locations is what Athena measures as the query
                    // PLANNING phase — "the time spent retrieving table partitions from
                    // the data source" — and it is over before the engine is handed the
                    // SQL, so the two spans are measured separately rather than reported
                    // as one.
                    let planning_time = started.elapsed();
                    let mut completion = query_engine::run_over(
                        handle,
                        &prepared.objects,
                        &prepared.sql,
                        reading.counts_rows,
                        parameters,
                        prepared.scanned_bytes,
                    );
                    completion.planning_time = planning_time;
                    // A name the relations it read do not carry is a failure of one of
                    // Athena's own classes, reported in Athena's words about the SUBMITTED
                    // statement rather than in the engine's about the one it ran.
                    classify_unresolvable_name(&mut completion, sql, &prepared.hidden_columns);
                    completion
                }
                Err(failure) => Completion {
                    outcome: failure.outcome(),
                    engine_time: Duration::ZERO,
                    planning_time: started.elapsed(),
                },
            }
        }
    }
}

/// The prefix Athena's message for an unresolvable name opens with, which is also the error
/// code a client reads off it.
const COLUMN_NOT_FOUND: &str = "COLUMN_NOT_FOUND";

/// Report a run the SQL engine stopped over a NAME the SUBMITTED statement wrote and it
/// could not resolve as the failure of Athena's own class for it.
///
/// The engine resolves the names of the statement it was handed, which is the submitted one
/// with each relation rewritten to the rows it was materialised as — so what it says about
/// an unresolvable column names that column but neither the class Athena publishes for it
/// nor where the submitted statement wrote it. Both are added here: the class from the
/// engine's own diagnostic ([`ENGINE_UNRESOLVABLE_COLUMN`], its fixed wording for that one
/// failure), and the position from the submitted text.
///
/// A name the submitted statement does NOT carry is not the caller's to be told about in
/// those words — the engine also resolves names this emulator wrote for it, among them the
/// columns a Lake Formation row filter's own expression refers to — so such a failure is
/// left exactly as the engine reported it, as is every other engine failure.
fn classify_unresolvable_name(
    completion: &mut Completion,
    sql: &str,
    hidden_columns: &HashSet<String>,
) {
    let Outcome::Failed {
        message,
        cause: FailureCause::Sql,
        ..
    } = &completion.outcome
    else {
        return;
    };
    let Some(column) = unresolvable_column(message) else {
        return;
    };
    // A column the relation DECLARES and this reader may not see is unresolvable because of
    // what the reader holds, which Athena says in a sentence of its own.
    let failure = if hidden_columns.contains(&glue_state::fold_name(&column)) {
        Failure::column_not_authorized(&column)
    } else {
        match statement::identifier_position(sql, &column) {
            Some(at) => Failure::column_not_found(at, &column),
            None => return,
        }
    };
    completion.outcome = failure.outcome();
}

/// The column the SQL engine could not resolve, read off its own diagnostic for that
/// failure — `None` for every other engine failure.
///
/// The engine words it as [`ENGINE_UNRESOLVABLE_COLUMN`] followed by the name as the
/// statement wrote it, and then by what it was resolving the name in. A name the statement
/// QUOTED is quoted there too, and carries the engine's own hint about the quoting after it;
/// the name itself is what is taken, unquoted, because that is what Athena's own message
/// names.
fn unresolvable_column(message: &str) -> Option<String> {
    let named = message.strip_prefix(ENGINE_UNRESOLVABLE_COLUMN)?.trim_start();
    let name = match named.strip_prefix(ENGINE_QUOTE) {
        // A quoted name runs to its closing quote, so a name carrying a space is one name.
        Some(quoted) => quoted.split(ENGINE_QUOTE).next().unwrap_or(quoted),
        None => named.split_whitespace().next().unwrap_or(named),
    };
    (!name.is_empty()).then(|| name.to_string())
}

/// How the SQL engine opens its diagnostic for a column it cannot resolve.
const ENGINE_UNRESOLVABLE_COLUMN: &str = "no such column: ";

/// How that diagnostic quotes a name the statement quoted.
const ENGINE_QUOTE: char = '"';

/// Perform one catalog statement against the Data Catalog.
fn perform<Eng: Send + Sync + 'static>(
    engine: &Eng,
    statement: &CatalogStatement,
    now: SystemTime,
) -> Result<StoredResultSet, Failure> {
    let (catalog, at) = match statement {
        CatalogStatement::CreateDatabase(database) => {
            (database.target.catalog.clone(), database.target.at)
        }
        CatalogStatement::DropDatabase { target, .. } => (target.catalog.clone(), target.at),
        CatalogStatement::ShowDatabases { catalog, .. } => (catalog.clone(), Position::START),
        CatalogStatement::CreateExternalTable(table) => {
            (table.target.catalog.clone(), table.target.at)
        }
        CatalogStatement::CreateView(view) => (view.target.catalog.clone(), view.target.at),
        CatalogStatement::DropView { target, .. }
        | CatalogStatement::DropTable { target, .. }
        | CatalogStatement::ShowCreateView { target } => (target.catalog.clone(), target.at),
        CatalogStatement::ShowRelations { schema, .. } => (schema.catalog.clone(), schema.at),
    };
    let catalog_id = catalog_id_of(engine, &catalog, at)?;
    // An external table's LOCATION has to name storage the table can be registered over:
    // a bucket the object store holds, and a FOLDER inside it. Both halves are validated
    // here, before the catalog entry the statement would define is resolved, because what
    // it declares is wrong whatever the catalog holds; the bucket first, since what sits at
    // a key of a bucket that is not there is not a question. The object store's guard is
    // taken and released inside, so no two stores are ever held at once.
    if let CatalogStatement::CreateExternalTable(table) = statement {
        if location_bucket_is_absent(engine, table.location.as_deref()) {
            return Err(Failure::location_bucket_absent());
        }
        if location_denotes_an_object(engine, table.location.as_deref()) {
            return Err(Failure::location_on_file());
        }
    }
    let Some(state) = glue_state::service_state(engine) else {
        return Err(Failure::emulator(CATALOG_UNREACHABLE));
    };
    let mut store = state.lock();
    match statement {
        CatalogStatement::CreateDatabase(database) => {
            ddl::create_database(&mut store, &catalog_id, database, now)
        }
        CatalogStatement::DropDatabase {
            target,
            if_exists,
            cascade,
        } => {
            let dropped =
                ddl::drop_database(&mut store, &catalog_id, target, *if_exists, *cascade)?;
            forget_partition_indexes(engine, &dropped);
            Ok(StoredResultSet::default())
        }
        CatalogStatement::ShowDatabases { filter, .. } => {
            ddl::show_databases(&store, &catalog_id, filter.as_ref())
        }
        CatalogStatement::CreateExternalTable(table) => {
            ddl::create_external_table(&mut store, &catalog_id, table, now)
        }
        CatalogStatement::CreateView(view) => {
            ddl::create_view(&mut store, &catalog_id, view, now)
        }
        CatalogStatement::DropTable { target, if_exists } => {
            let dropped = ddl::drop_table(&mut store, &catalog_id, target, *if_exists)?;
            forget_partition_indexes(engine, &dropped);
            Ok(StoredResultSet::default())
        }
        CatalogStatement::DropView { target, if_exists } => {
            ddl::drop_view(&mut store, &catalog_id, target, *if_exists)
        }
        CatalogStatement::ShowCreateView { target } => {
            ddl::show_create_view(&store, &catalog_id, target)
        }
        CatalogStatement::ShowRelations {
            schema,
            views_only,
            filter,
        } => ddl::show_relations(&store, &catalog_id, schema, *views_only, filter.as_ref()),
    }
}

/// Whether `statement` is a `DROP TABLE` whose name the DATA CATALOG holds no database
/// for, and which therefore addresses a table of the ENGINE's own.
///
/// Every table Athena's DDL registers lives in the catalog, and so does every table a
/// client registers through the catalog API — but a `CREATE TABLE` that declares no
/// location is not a catalog statement here ([`Plan::Engine`]) and defines its table in the
/// engine database the query's context addresses. A drop naming a database the catalog does
/// not hold can only be addressing one of those, so it is performed where its create was,
/// rather than refused by a catalog that was never asked to hold it. A database the catalog
/// DOES hold is a catalog database: a drop inside it is the catalog's, including its own
/// table-does-not-exist refusal.
fn drops_a_table_of_the_engines_own<Eng: Send + Sync + 'static>(
    engine: &Eng,
    statement: &CatalogStatement,
) -> bool {
    let CatalogStatement::DropTable { target, .. } = statement else {
        return false;
    };
    let Ok(catalog_id) = catalog_id_of(engine, &target.catalog, target.at) else {
        return false;
    };
    let Some(state) = glue_state::service_state(engine) else {
        return false;
    };
    let store = state.lock();
    store.database(&catalog_id, &target.database).is_none()
}

/// Forget the PARTITION INDEX records of the tables a drop removed.
///
/// An index is a resource OF its table, so a table the catalog no longer holds leaves none
/// behind — the same cascade the catalog's own deletes perform, taken in the same order:
/// the catalog's guard is still held while these records are written.
fn forget_partition_indexes<Eng: Send + Sync + 'static>(
    engine: &Eng,
    tables: &[glue_state::TableEntry],
) {
    if tables.is_empty() {
        return;
    }
    if let Some(records) =
        ::service_com_amazonaws_glue_awsglue::service::partition_indexes::partition_index_store(
            engine,
        )
    {
        records.lock().forget_tables(tables.iter());
    }
}

/// The message for a store this engine does not carry.
const CATALOG_UNREACHABLE: &str =
    "the Data Catalog is not reachable on this engine, so no statement can resolve a table";

/// The message for an object store this engine does not carry.
const OBJECT_STORE_UNREACHABLE: &str =
    "the object store is not reachable on this engine, so no table's data can be read";

/// Whether `location` names a BUCKET the object store does not hold, so there is no
/// storage for a table declared over it.
///
/// A location is resolved against the object store before the statement is committed: the
/// bucket it names has to be one the account holds, because an external table is a
/// definition OVER data that already exists. A location that is no object-store path at all
/// names no bucket here, and so is not judged — the same reading
/// [`location_denotes_an_object`] takes of one.
fn location_bucket_is_absent<Eng: Send + Sync + 'static>(
    engine: &Eng,
    location: Option<&str>,
) -> bool {
    let Some(location) = location
        .map(str::trim)
        .filter(|location| !location.is_empty())
    else {
        return false;
    };
    let Some((bucket_name, _)) = data::parse_s3_uri(location) else {
        return false;
    };
    let Some(state) = s3_state::service_state(engine) else {
        // Without an object store there is nothing to resolve the location against, so the
        // statement is not refused over what cannot be read.
        return false;
    };
    let store = state.lock();
    store.bucket(&bucket_name).is_none()
}

/// Whether `location` denotes a single OBJECT rather than the folder an external table's
/// data lives under.
///
/// Athena's own rule for the clause is that a location names a FOLDER: its documentation
/// tells a caller to close a folder or bucket location with a trailing separator and never
/// to write a file name in it, and a statement that does name a file is refused rather
/// than registered. So both halves of what is known decide it:
///
/// * what the OBJECT STORE holds there — an object stored at exactly the key the location
///   names is a file, and a key other objects live UNDER is a folder, whatever either is
///   named;
/// * where the object store holds nothing at that key at all, the NAME itself: a location
///   closed with a separator names a folder, and one whose last segment carries a file
///   extension names a file.
///
/// A location that is no object-store path at all names no object here.
fn location_denotes_an_object<Eng: Send + Sync + 'static>(
    engine: &Eng,
    location: Option<&str>,
) -> bool {
    let Some(location) = location
        .map(str::trim)
        .filter(|location| !location.is_empty())
    else {
        return false;
    };
    let Some((bucket_name, key)) =
        data::parse_s3_uri(location).filter(|(_, key)| !key.is_empty())
    else {
        return false;
    };
    match held_at(engine, &bucket_name, &key) {
        HeldAt::Object => true,
        HeldAt::Folder => false,
        HeldAt::Nothing => !location.ends_with('/') && names_a_file(&key),
    }
}

/// What the object store holds at one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldAt {
    /// An object stored at exactly that key.
    Object,
    /// No object at the key itself, but objects under it: a folder.
    Folder,
    /// Nothing at the key and nothing under it.
    Nothing,
}

/// What the object store holds at `key` of `bucket_name`, under ONE guard of its store.
fn held_at<Eng: Send + Sync + 'static>(engine: &Eng, bucket_name: &str, key: &str) -> HeldAt {
    let Some(state) = s3_state::service_state(engine) else {
        return HeldAt::Nothing;
    };
    let store = state.lock();
    let Some(bucket) = store.bucket(bucket_name) else {
        return HeldAt::Nothing;
    };
    if bucket.object(key).is_some() {
        return HeldAt::Object;
    }
    let under = format!("{key}/");
    if bucket.objects().any(|(held, _)| held.starts_with(&under)) {
        return HeldAt::Folder;
    }
    HeldAt::Nothing
}

/// Whether the last segment of `key` is a FILE name: a name closed by an extension, which
/// is the form Athena's documentation of the clause tells a caller not to write in it.
///
/// An extension is letters: a segment closed by digits — a dated or versioned folder, say
/// — is a folder name, and so is one carrying no extension at all.
fn names_a_file(key: &str) -> bool {
    let name = key.rsplit('/').next().unwrap_or(key);
    name.rsplit_once('.')
        .is_some_and(|(stem, extension)| {
            !stem.is_empty()
                && !extension.is_empty()
                && extension.len() <= MAX_FILE_EXTENSION
                && extension.chars().all(|letter| letter.is_ascii_alphabetic())
        })
}

/// How long the extension of a file name may be for [`names_a_file`] to read it as one.
const MAX_FILE_EXTENSION: usize = 10;

/// The `CatalogId` the data catalog `catalog` names, or `None` when this emulator holds no
/// metadata for it.
///
/// The same resolution a statement's own relations take ([`catalog_id_of`]), for a caller
/// that needs the id rather than the failure to report: the Lake Formation decision a
/// submission resolves before it is planned
/// ([`crate::state::lake_formation::resolved`]) is keyed by the id, and a catalog whose
/// metadata this emulator does not serve is a catalog nothing is known about.
pub fn catalog_id<Eng: Send + Sync + 'static>(engine: &Eng, catalog: &str) -> Option<String> {
    catalog_id_of(engine, catalog, Position::START).ok()
}

/// The `CatalogId` the data catalog `catalog` names, or why its metadata cannot be read.
///
/// The SAME resolution Athena's metadata reads use: `AwsDataCatalog` is the account's own
/// Glue Data Catalog, a registered `GLUE` catalog naming that catalog id is that catalog
/// under a second name, and one whose metadata an external metastore or another account's
/// catalog serves is not one this emulator holds.
fn catalog_id_of<Eng: Send + Sync + 'static>(
    engine: &Eng,
    catalog: &str,
    at: Position,
) -> Result<String, Failure> {
    let Some(state) = crate::state::service_state(engine) else {
        return Err(Failure::emulator(
            "Athena state is not reachable on this engine",
        ));
    };
    // The registry's guard is taken and released here, before the catalog's own store is
    // reached, so no two stores are ever held at once.
    let source = {
        let store = state.lock();
        crate::state::metadata_source(&store.data_catalogs, catalog)
    };
    match source {
        crate::state::MetadataSource::GlueDataCatalog(catalog_id) => Ok(catalog_id),
        crate::state::MetadataSource::ExternalMetastore(catalog_type) => Err(Failure::emulator(
            crate::state::metastore_unreachable(catalog, &catalog_type),
        )),
        crate::state::MetadataSource::ForeignGlueCatalog(catalog_id) => Err(Failure::emulator(
            crate::state::foreign_glue_catalog(catalog, &catalog_id),
        )),
        crate::state::MetadataSource::Unregistered => Err(Failure::catalog_not_found(at, catalog)),
    }
}

/// A statement whose relations are resolved: the objects to materialise in the engine
/// database, the SQL that reads them, and the bytes the resolution read.
struct Prepared {
    /// The tables and views the statement reads, base tables first.
    objects: Vec<LocalObject>,
    /// The statement, with every resolved relation naming its materialised rows.
    sql: String,
    /// The bytes of object data this statement scanned.
    scanned_bytes: i64,
    /// The columns the relations this statement reads declare and the reader may not see,
    /// as the catalog folds their names — what makes a statement that names one of them a
    /// failure of authorization rather than a failure to resolve a name at all.
    hidden_columns: HashSet<String>,
}

/// Resolve every relation one statement reads, and render the SQL the engine runs.
fn resolved_sql<Eng: Send + Sync + 'static>(
    engine: &Eng,
    handle: &DatabaseHandle,
    relational: RelationalStatement,
    context: &QueryContext,
    access: &DataLakeAccess,
) -> Result<Prepared, Failure> {
    let RelationalStatement {
        mut statement,
        relations,
    } = relational;
    // ONE budget for the whole statement: how many relations may be resolved at all
    // (which is what bounds the PRODUCT of the relations a statement names and the levels
    // of view they are defined over), how many rows and bytes may be materialised, and
    // when the resolution must stop whatever it has reached.
    let mut budget = Budget::default();
    let mut resolution = Resolution::default();
    let resolved = resolve(engine, &relations, 0, &mut resolution, &mut budget, access)?;

    // The catalog's guard is released before the object store's is taken, so no two
    // stores are ever held at once.
    let mut objects: Vec<LocalObject> = resolution
        .schema_tables
        .into_iter()
        .map(LocalObject::Table)
        .collect();
    let mut scanned_bytes = 0usize;
    let mut hidden_columns: HashSet<String> = HashSet::new();
    if !resolution.sources.is_empty() {
        let Some(state) = s3_state::service_state(engine) else {
            return Err(Failure::emulator(OBJECT_STORE_UNREACHABLE));
        };
        let store = state.lock();
        // The hidden metadata columns the statement NAMES: each one says something about the
        // OBJECT a row was read from rather than carrying a field of it, and a statement
        // sees one only where it addresses it — so a read carries exactly the ones this
        // statement reads, and a column list of `*` expands to none of them.
        let metadata: Vec<data::MetadataColumn> = data::MetadataColumn::ALL
            .iter()
            .copied()
            .filter(|column| statement::names_column(&statement, column.name()))
            .collect();
        for source in &resolution.sources {
            hidden_columns.extend(source.hidden_columns());
            let (object, scanned) = source.read(&store, &mut budget, &metadata)?;
            scanned_bytes = scanned_bytes.saturating_add(scanned);
            objects.push(object);
        }
    }
    objects.extend(
        resolution
            .views
            .into_iter()
            .map(|(name, sql)| LocalObject::View { name, sql }),
    );

    // A relation the catalog holds nothing for may still be a table an earlier statement
    // defined in THIS engine database, which is where a query that defined its own table
    // reads it from; one that is neither is the catalog's own not-found refusal.
    let mut rewrites = resolved.rewrites;
    for (relation, miss) in resolved.unresolved {
        let local = relation.table.clone();
        if relation.database == glue_state::fold_name(&context.database)
            && query_engine::holds_relation(handle, &local)
        {
            rewrites.insert(relation.written.clone(), local);
            continue;
        }
        return Err(miss);
    }

    Ok(Prepared {
        sql: statement::rewritten(&mut statement, &rewrites),
        objects,
        scanned_bytes: i64::try_from(scanned_bytes).unwrap_or(i64::MAX),
        hidden_columns,
    })
}

/// What resolving a statement's relations accumulated, across every level of view it
/// followed.
#[derive(Debug, Default)]
struct Resolution {
    /// The catalog tables to read, in the order they were resolved.
    sources: Vec<DataSource>,
    /// The catalog's own reporting tables the statement reads, already built.
    schema_tables: Vec<LocalTable>,
    /// The views to define over them, base-first.
    views: Vec<(String, String)>,
    /// The local names already resolved, ACROSS the whole statement, so a relation two
    /// branches of it share is resolved and read exactly once.
    resolved: HashSet<String>,
}

/// What resolving ONE list of relations established for the statement that names them.
#[derive(Debug, Default)]
struct Resolved {
    /// Which local name each written relation is rewritten to.
    rewrites: HashMap<String, String>,
    /// The relations the catalog holds nothing for, each with the refusal to report when
    /// nothing else resolves them either.
    unresolved: Vec<(Relation, Failure)>,
}

/// Resolve `relations` against the Data Catalog, following a view to the relations it
/// reads, and charging every relation against `budget`.
///
/// `access` is Lake Formation's answer about the caller: a relation it covers no `SELECT`
/// of is resolved to nothing — the table is not there for this caller — and one it covers
/// a NARROWED `SELECT` of is read through that narrowing.
fn resolve<Eng: Send + Sync + 'static>(
    engine: &Eng,
    relations: &[Relation],
    depth: usize,
    resolution: &mut Resolution,
    budget: &mut Budget,
    access: &DataLakeAccess,
) -> Result<Resolved, Failure> {
    if depth > MAX_VIEW_DEPTH {
        return Err(Failure::emulator(format!(
            "a view this statement reads is defined over more than {MAX_VIEW_DEPTH} levels \
             of view, which this emulator does not expand"
        )));
    }
    let mut resolved = Resolved::default();
    for relation in relations {
        let local = relation.local_name();
        if resolution.resolved.contains(&local) {
            resolved
                .rewrites
                .insert(relation.written.clone(), local.clone());
            continue;
        }
        budget.charge_relation()?;
        let catalog_id = catalog_id_of(engine, &relation.catalog, relation.at)?;
        // What LAKE FORMATION lets this caller read of the relation, decided before the
        // catalog is asked what it holds: a relation no `SELECT` of the caller's reaches is
        // hidden from it, and its cells are never read.
        let permitted =
            access.table_access(&catalog_id, &relation.database, &relation.table);
        // What the catalog holds for this relation is read under its guard, and the guard
        // is released before anything else is reached.
        let held = {
            let Some(state) = glue_state::service_state(engine) else {
                return Err(Failure::emulator(CATALOG_UNREACHABLE));
            };
            let store = state.lock();
            if relation.is_information_schema() {
                Held::ReportingTable(reporting_table(&store, &catalog_id, relation, budget)?)
            } else {
                match store.database(&catalog_id, &relation.database) {
                    // A database the catalog serves without holding a record of it holds no
                    // relation at all, so what the statement named is the relation that is
                    // not there ([`served_without_record`]).
                    None if served_without_record(&relation.database) => Held::NoTable,
                    None => Held::NoDatabase,
                    Some(database) => match database.tables.get(&relation.table) {
                        None => Held::NoTable,
                        // A relation Lake Formation hides is not there for this caller,
                        // whether it is a table or a view: the governance decision is taken
                        // on the relation the statement NAMES, and the relations a view
                        // reads are each governed in their own right when it is expanded.
                        //
                        // What a NARROWED grant on a view resource would show is not
                        // applied here: a view's rows are the rows its own query computes
                        // from the relations under it, and each of those is read through the
                        // grant the caller holds on IT — so the narrowing that reaches the
                        // caller is the one on the data, which is where Lake Formation holds
                        // a column scope and a row filter (`TableWithColumns` and
                        // `DataCellsFilter` both name a table).
                        Some(_) if matches!(permitted, TableAccess::Hidden) => Held::Hidden,
                        Some(table) if ddl::is_view(table) => Held::View(
                            table
                                .view_original_text
                                .as_deref()
                                .map(ddl::view_definition_sql)
                                .unwrap_or_default(),
                        ),
                        Some(table) => {
                            let source = DataSource::of(relation, table)?;
                            match &permitted {
                                TableAccess::Readable(scope) if !scope.is_unrestricted() => {
                                    let narrowed = source.narrowed(scope);
                                    // A grant that covers no column of the table lets its
                                    // holder read no cell of it, which is the same answer
                                    // as holding no grant at all.
                                    if narrowed.visible_columns().is_empty() {
                                        Held::Hidden
                                    } else {
                                        Held::Table(narrowed)
                                    }
                                }
                                _ => Held::Table(source),
                            }
                        }
                    },
                }
            }
        };
        match held {
            Held::ReportingTable(table) => {
                resolution.schema_tables.push(table);
                resolution.resolved.insert(local.clone());
                resolved.rewrites.insert(relation.written.clone(), local);
            }
            Held::Table(source) => {
                resolution.sources.push(source);
                resolution.resolved.insert(local.clone());
                resolved.rewrites.insert(relation.written.clone(), local);
            }
            Held::View(definition) => {
                resolution.resolved.insert(local.clone());
                resolved
                    .rewrites
                    .insert(relation.written.clone(), local.clone());
                let view_context = QueryContext {
                    catalog: relation.catalog.clone(),
                    database: relation.database.clone(),
                };
                let sql = view_sql(
                    engine,
                    &definition,
                    &view_context,
                    depth,
                    resolution,
                    budget,
                    access,
                )?;
                resolution.views.push((local, sql));
            }
            Held::NoDatabase => resolved
                .unresolved
                .push((relation.clone(), Failure::schema_not_found(relation))),
            Held::NoTable => resolved
                .unresolved
                .push((relation.clone(), Failure::table_not_found(relation))),
            // A relation the caller may not read is reported as one that is not there, and
            // nothing else of this engine database may answer for it: an earlier statement's
            // own table of that name is not what the statement addressed.
            Held::Hidden => return Err(Failure::table_not_found(relation)),
        }
    }
    Ok(resolved)
}

/// What the catalog holds for one relation.
enum Held {
    /// A table whose data lives at a location.
    Table(DataSource),
    /// One of the catalog's own reporting tables, already built.
    ReportingTable(LocalTable),
    /// A view, carrying the query it is defined by.
    View(String),
    /// No database of that name.
    NoDatabase,
    /// No relation of that name in a database that is there.
    NoTable,
    /// A relation that IS there, which Lake Formation does not let this caller read.
    Hidden,
}

/// The SQL one view's rows come from: its own definition, with every relation IT reads
/// resolved and rewritten the same way the submitted statement's are.
fn view_sql<Eng: Send + Sync + 'static>(
    engine: &Eng,
    definition: &str,
    context: &QueryContext,
    depth: usize,
    resolution: &mut Resolution,
    budget: &mut Budget,
    access: &DataLakeAccess,
) -> Result<String, Failure> {
    let parsed = statement::parse_statements(definition)
        .and_then(|mut statements| (statements.len() == 1).then(|| statements.remove(0)));
    let Some(mut parsed) = parsed else {
        return Err(Failure::emulator(
            "a view this statement reads carries a definition that is not one SQL \
             statement, so its rows cannot be read",
        ));
    };
    let relations = statement::relations_of(&parsed, context);
    if relations.is_empty() {
        return Ok(statement::rewritten(&mut parsed, &HashMap::new()));
    }
    let resolved = resolve(engine, &relations, depth + 1, resolution, budget, access)?;
    // A view whose own source is missing cannot be read at all: the view is there, and
    // the relation under it is not.
    if let Some((_, miss)) = resolved.unresolved.into_iter().next() {
        return Err(miss);
    }
    Ok(statement::rewritten(&mut parsed, &resolved.rewrites))
}

/// One of the catalog's own reporting tables, as the rows it holds.
///
/// `information_schema` is how a SQL client asks the engine what the catalog holds, and
/// every row below is read off the catalog itself: the databases it holds, the relations
/// in each, what kind each relation is, and the columns each one declares.
fn reporting_table(
    store: &glue_state::ComAmazonawsGlueAwsglueState,
    catalog_id: &str,
    relation: &Relation,
    budget: &mut Budget,
) -> Result<LocalTable, Failure> {
    let catalog = relation.catalog.to_ascii_lowercase();
    let held = store.catalogs.get(catalog_id);
    let databases = held.map(glue_state::Catalog::databases_ordered).unwrap_or_default();
    let mut rows: Vec<Vec<::rusqlite::types::Value>> = Vec::new();
    let columns: Vec<&str> = match relation.table.as_str() {
        "schemata" => {
            for database in &databases {
                budget.charge_row(relation, SCHEMATA_COLUMNS)?;
                rows.push(vec![text(&catalog), text(&database.name)]);
            }
            vec!["catalog_name", "schema_name"]
        }
        "tables" => {
            for database in &databases {
                for table in database.tables_ordered() {
                    budget.charge_row(relation, TABLES_COLUMNS)?;
                    rows.push(vec![
                        text(&catalog),
                        text(&database.name),
                        text(&table.name),
                        text(if ddl::is_view(table) {
                            RELATION_TYPE_VIEW
                        } else {
                            RELATION_TYPE_BASE_TABLE
                        }),
                    ]);
                }
            }
            vec!["table_catalog", "table_schema", "table_name", "table_type"]
        }
        "views" => {
            for database in &databases {
                for table in database.tables_ordered() {
                    if !ddl::is_view(table) {
                        continue;
                    }
                    budget.charge_row(relation, VIEWS_COLUMNS)?;
                    rows.push(vec![
                        text(&catalog),
                        text(&database.name),
                        text(&table.name),
                        text(
                            &table
                                .view_original_text
                                .as_deref()
                                .map(ddl::view_definition_sql)
                                .unwrap_or_default(),
                        ),
                    ]);
                }
            }
            vec![
                "table_catalog",
                "table_schema",
                "table_name",
                "view_definition",
            ]
        }
        "columns" => {
            for database in &databases {
                for table in database.tables_ordered() {
                    for (position, column) in ddl::table_columns(table).iter().enumerate() {
                        budget.charge_row(relation, COLUMNS_COLUMNS)?;
                        rows.push(vec![
                            text(&catalog),
                            text(&database.name),
                            text(&table.name),
                            text(&column.name),
                            ::rusqlite::types::Value::Integer(
                                i64::try_from(position.saturating_add(1)).unwrap_or(i64::MAX),
                            ),
                            text(&column.data_type),
                            text(COLUMN_IS_NULLABLE),
                            column
                                .comment
                                .as_deref()
                                .map_or(::rusqlite::types::Value::Null, text),
                        ]);
                    }
                }
            }
            vec![
                "table_catalog",
                "table_schema",
                "table_name",
                "column_name",
                "ordinal_position",
                "data_type",
                "is_nullable",
                "comment",
            ]
        }
        _ => return Err(Failure::table_not_found(relation)),
    };
    Ok(LocalTable {
        name: relation.local_name(),
        columns: columns
            .into_iter()
            .map(|name| LocalColumn {
                name: name.to_string(),
                declared: data::engine_type(if name == "ordinal_position" {
                    "int"
                } else {
                    "string"
                }),
            })
            .collect(),
        rows,
    })
}

/// How many columns `information_schema.schemata` reports.
const SCHEMATA_COLUMNS: usize = 2;

/// How many columns `information_schema.tables` reports.
const TABLES_COLUMNS: usize = 4;

/// How many columns `information_schema.views` reports.
const VIEWS_COLUMNS: usize = 4;

/// How many columns `information_schema.columns` reports.
const COLUMNS_COLUMNS: usize = 8;

/// How `information_schema.tables` classifies a view.
const RELATION_TYPE_VIEW: &str = "VIEW";

/// How `information_schema.tables` classifies a table that holds data.
const RELATION_TYPE_BASE_TABLE: &str = "BASE TABLE";

/// What `information_schema.columns` reports for a column's nullability: the catalog
/// records none for a Hive column, and every such column admits a missing value.
const COLUMN_IS_NULLABLE: &str = "YES";

/// One text value, as the engine holds it.
fn text(value: &str) -> ::rusqlite::types::Value {
    ::rusqlite::types::Value::Text(value.to_string())
}

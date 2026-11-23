// SPDX-License-Identifier: CC0-1.0

//! The cluster's SQL CLIENT, as the environment of a step's command line provides it: the
//! `hive` and `spark-sql` commands an Amazon EMR cluster runs statements with.
//!
//! # The rule this module implements
//!
//! "command-runner.jar … lets you run commands on your cluster" (Amazon EMR documentation),
//! so a `command-runner.jar` step's arguments are a command line and what that command line
//! does is the step's own work. A cluster whose applications include Hive or Spark provides
//! that client as a command of its nodes, so a command line that invokes it REALLY runs the
//! statements it is given — and the file it writes its output to, and the transfer that
//! carries that file to Amazon S3 ([`super::step_command_transfers`]), are then the ordinary
//! stages of the same command line rather than work this emulator skips.
//!
//! This module is ONE PROGRAM of that command line's environment
//! ([`::service_common::posix_shell_session`], whose session-local utilities, bounds and
//! containment are stated there): the client's own invocation — its `-e` statement, its `-f`
//! script of the session's tree, the database it starts in and the options it takes — and
//! nothing else. No host command, process or shell is started here or anywhere this calls.
//!
//! What a statement COMES TO is [`super::step_sql_engine`]'s: a change applied to the
//! cluster's Hive metastore, or the rows the in-process SQL engine computed over the data
//! that metastore's tables point at. The client prints what it answered — the rows of a
//! query, the `OK` of a statement the metastore applied — and exits with the status the
//! shell around it reads: zero when every statement it was given ran, and
//! [`SQL_CLIENT_FAILURE_STATUS`] when one of them was refused, carrying the driver's own
//! error.
//!
//! # What it performs nothing of
//!
//! An invocation carrying a statement of no class this emulator can account for (one that
//! writes table data, defines or drops a relation), an invocation on a cluster whose Hive
//! metastore this emulator does not hold, and an invocation written in a form this program
//! does not read are answered with `None`: the session reports a command it could not run,
//! and the step then reports that its work was not run
//! ([`super::STEP_NOT_EXECUTED_REASON`]) — never a success for a statement nothing performed.
//! Every statement is classified BEFORE any of them is performed, so such an invocation
//! leaves the metastore exactly as it was.
//!
//! A statement nothing performed where an earlier statement of the SAME invocation already
//! changed the metastore is not that case: the change happened, so the client exits non-zero
//! carrying the reason that says the rest of its work was not run and its step FAILS. It is
//! never reported as a command nothing ran, which would say the catalog was untouched when it
//! was not.

use ::std::sync::{Arc, Mutex, PoisonError};
use ::std::time::SystemTime;

use ::service_com_amazonaws_glue_awsglue::state as glue_state;
use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};
use sqlparser::ast::Statement;

use super::*;

use super::step_sql_engine;

/// The names a command line invokes the cluster's SQL client by: Hive's own CLI and the
/// Spark SQL one, the two clients an Amazon EMR cluster runs statements with.
pub const SQL_CLIENT_PROGRAMS: [&str; 2] = ["hive", "spark-sql"];

/// The status the client exits with when a statement it was given was refused: the failure a
/// shell reads as one, which is what `set -e` around it stops on.
pub const SQL_CLIENT_FAILURE_STATUS: i32 = 1;

/// What the client prints for a statement the metastore applied, as the CLI writes it once
/// the statement has run.
const APPLIED_LINE: &str = "OK";

/// How the client prefixes the error of a statement that was refused, as the CLI writes it.
const FAILED_PREFIX: &str = "FAILED: ";

/// The option the client reads its statements from: "the quoted query string"
/// (`hive -e '<quoted-query-string>'`, Amazon EMR / Hive CLI documentation), the form a step
/// submits a statement in.
const STATEMENT_OPTION: [&str; 2] = ["-e", "--execute"];

/// The option the client reads its statements out of a FILE with (`hive -f <filename>`),
/// which here is a file of the session's own tree.
const SCRIPT_OPTION: [&str; 2] = ["-f", "--file"];

/// The option that names the database the client's statements start in.
const DATABASE_OPTION: [&str; 1] = ["--database"];

/// The options the client takes that carry a VALUE this program reads nothing out of: the
/// configuration and variable settings, and the initialisation script.
const IGNORED_VALUE_OPTIONS: [&str; 6] = ["--hiveconf", "--hivevar", "--define", "-d", "-i", "-j"];

/// The options the client takes that carry no value: the quiet and verbose forms, which
/// decide only how much the client prints about a statement it ran.
const SILENT_OPTIONS: [&str; 2] = ["-S", "--silent"];

/// See [`SILENT_OPTIONS`]: the forms that carry no value and change nothing this program
/// reads.
const FLAG_OPTIONS: [&str; 2] = ["-v", "--verbose"];

/// How many statements ONE invocation of the client runs. CHOSEN: a bound on what one
/// command can cost, beside the bounds the engine and the object reads carry of their own.
const MAX_STATEMENTS_PER_INVOCATION: usize = 32;

/// How long one invocation's statement text may be. CHOSEN: the model caps "the string
/// values passed into HadoopJarStep" at 10240 characters in total (`AddJobFlowSteps` doc), so
/// nothing a step submits carries more than this.
const MAX_STATEMENT_BYTES: usize = 10_240;

/// The cluster's SQL client as one command's environment provides it.
///
/// It holds the METASTORE the statements run against — the one AWS Glue Data Catalog this
/// emulator holds, and `None` for a cluster whose Hive metastore is not that catalog — and
/// the object store the data under it is read from and written to, which is the same program
/// the command line's transfers are made by, so one command's reads and writes share its
/// budget and its credential.
#[derive(Clone)]
pub struct StepSqlCli {
    /// The cluster's Hive metastore, where this emulator holds it.
    catalog: Option<::engine::ServiceState<glue_state::ComAmazonawsGlueAwsglueState>>,
    /// What the relations' data is read out of.
    objects: Option<StepObjectCli>,
    /// The driver error the last refused statement of this command answered, shared with the
    /// caller that reports the step: a step whose command line's client was refused reports
    /// that error rather than only the status the command exited with.
    failure: Arc<Mutex<Option<StepFailure>>>,
}

/// Written by hand rather than derived: the environment trait requires it and the state
/// handle this holds carries no `Debug`. It names no field, because neither the metastore
/// handle nor the credential the object program holds belongs in a log.
impl ::std::fmt::Debug for StepSqlCli {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter.debug_struct("StepSqlCli").finish()
    }
}

/// The SQL client one command of a step gets.
///
/// `metastore_is_the_catalog` is whether the cluster's configuration selects the AWS Glue
/// Data Catalog as its Hive metastore ([`uses_glue_data_catalog`]): a cluster whose metastore
/// is its own master node's has one this emulator does not run, so its client performs
/// nothing and the step reports that its work was not run.
#[must_use]
pub fn step_sql_cli<Eng: Send + Sync + 'static>(
    engine: &Eng,
    metastore_is_the_catalog: bool,
    objects: Option<StepObjectCli>,
) -> StepSqlCli {
    StepSqlCli {
        catalog: metastore_is_the_catalog
            .then(|| glue_state::service_state(engine))
            .flatten(),
        objects,
        failure: Arc::new(Mutex::new(None)),
    }
}

impl StepSqlCli {
    /// The driver error a statement of this command was refused with, once one was: what the
    /// step reports as its failure ([`command_outcome`]).
    #[must_use]
    pub fn driver_failure(&self) -> Option<StepFailure> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Record the driver error this command's client was refused with.
    fn record_failure(&self, failure: StepFailure) {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(failure);
    }
}

/// Whether `name` invokes the cluster's SQL client, however the command line spelled its
/// path.
#[must_use]
pub fn names_sql_client(name: &str) -> bool {
    name.split('/')
        .next_back()
        .is_some_and(|basename| SQL_CLIENT_PROGRAMS.contains(&basename))
}

/// One invocation of the client: the statements it was given and how it was asked to run
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SqlInvocation {
    /// The statement text, in the order the options carried it.
    text: String,
    /// `--database`: the database the statements start in.
    database: Option<String>,
    /// Whether the client was asked to print only what a statement answered.
    silent: bool,
}

impl EnvironmentPrograms for StepSqlCli {
    /// The statements this invocation of the client was given, run against the cluster's
    /// metastore; `None` for every other program name and every invocation this program
    /// performs nothing of, which the session reports as a command it could not run.
    ///
    /// Once a statement of the invocation has CHANGED the metastore, nothing of it answers
    /// `None` any more: the change happened, so a statement nothing performed after it exits
    /// non-zero instead (see this module's header).
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        if !names_sql_client(name) {
            return None;
        }
        // The metastore is what every statement is resolved and applied against, so a client
        // on a cluster whose metastore this emulator does not hold performs nothing.
        let catalog = self.catalog.as_ref()?;
        let invocation = invocation(arguments, files)?;
        let statements = parse_statements(&invocation.text)?;
        if statements.is_empty() || statements.len() > MAX_STATEMENTS_PER_INVOCATION {
            return None;
        }
        // EVERY statement has to be one this emulator can account for before any of them is
        // performed: a statement whose effect is one nothing here applies makes the whole
        // invocation one the session reports it could not run, and nothing of it is applied.
        if !statements.iter().all(accountable) {
            return None;
        }
        let mut database = invocation
            .database
            .unwrap_or_else(|| DEFAULT_DATABASE.to_string());
        let mut output: Vec<String> = Vec::new();
        // Whether a statement of this invocation has already CHANGED the metastore, which is
        // what makes a later statement nothing performed a failure of this client rather than
        // a command nothing ran.
        let mut changed_the_metastore = false;
        let now = SystemTime::now();
        for statement in &statements {
            if let Some(named) = database_used(statement) {
                database = named;
                continue;
            }
            match step_sql_engine::run_statement(
                catalog,
                self.objects.as_ref(),
                statement,
                &database,
                now,
            ) {
                Ok(step_sql_engine::StatementAnswer::Applied) => {
                    changed_the_metastore = true;
                    if !invocation.silent {
                        output.push(APPLIED_LINE.to_string());
                    }
                }
                Ok(step_sql_engine::StatementAnswer::Rows(rows)) => output.extend(rows),
                Err(step_sql_engine::SqlFault::Driver(failure)) => {
                    output.push(format!("{FAILED_PREFIX}{}", failure.message));
                    self.record_failure(failure);
                    return Some(ProgramOutcome {
                        status: SQL_CLIENT_FAILURE_STATUS,
                        output,
                    });
                }
                Err(step_sql_engine::SqlFault::NotPerformed) => {
                    // Nothing performed this statement. Where nothing of the invocation has
                    // changed the metastore either, the client answers for nothing of it at
                    // all and the session reports a command it could not run. Where an
                    // earlier statement DID change it, that change happened: the client exits
                    // non-zero carrying the reason that says the rest of its work was not
                    // run, so the step FAILS rather than reporting a command nothing ran over
                    // a catalog that was really written.
                    if !changed_the_metastore {
                        return None;
                    }
                    let failure = StepFailure {
                        reason: STEP_NOT_EXECUTED_REASON.to_string(),
                        message: STEP_NOT_EXECUTED_MESSAGE.to_string(),
                    };
                    output.push(format!("{FAILED_PREFIX}{}", failure.message));
                    self.record_failure(failure);
                    return Some(ProgramOutcome {
                        status: SQL_CLIENT_FAILURE_STATUS,
                        output,
                    });
                }
            }
        }
        Some(ProgramOutcome { status: 0, output })
    }
}

/// Whether `statement` is of a class this emulator can account for: a statement that moves
/// the session's database, one whose only effect is reading, or one whose whole effect is a
/// change to the metastore.
fn accountable(statement: &Statement) -> bool {
    database_used(statement).is_some()
        || reads_only(statement)
        || metastore_change(statement, DEFAULT_DATABASE).is_some()
}

/// The invocation `arguments` are, or `None` when they are in a form this program reads
/// nothing out of — an option it does not take, an operand, or no statement at all.
fn invocation(arguments: &[String], files: &dyn ProgramFiles) -> Option<SqlInvocation> {
    let mut read = SqlInvocation::default();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let word = argument.as_str();
        if STATEMENT_OPTION.contains(&word) {
            read.text.push_str(arguments.next()?);
            // Each `-e` carries its own statements, which run one after another.
            read.text.push(';');
            continue;
        }
        if SCRIPT_OPTION.contains(&word) {
            // The script is a file of the SESSION's own tree, which the stages before this
            // command wrote; a path the tree holds no file at is a script the client could
            // not read.
            read.text.push_str(&files.read(arguments.next()?)?);
            read.text.push(';');
            continue;
        }
        if DATABASE_OPTION.contains(&word) {
            read.database = Some(arguments.next()?.clone());
            continue;
        }
        if IGNORED_VALUE_OPTIONS.contains(&word) {
            arguments.next()?;
            continue;
        }
        if SILENT_OPTIONS.contains(&word) {
            read.silent = true;
            continue;
        }
        if FLAG_OPTIONS.contains(&word) {
            continue;
        }
        // Anything else is an option or an operand this program performs nothing of.
        return None;
    }
    (!read.text.trim_matches(';').trim().is_empty() && read.text.len() <= MAX_STATEMENT_BYTES)
        .then_some(read)
}

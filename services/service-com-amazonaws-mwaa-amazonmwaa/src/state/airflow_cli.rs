// SPDX-License-Identifier: CC0-1.0

//! The Apache Airflow CLI of ONE environment: the tokens that authenticate to it, and what
//! a command run through it answers.
//!
//! `CreateCliToken` exists for exactly one client: the Apache Airflow CLI. It mints "an
//! Airflow CLI login token" and reports the "Airflow web server hostname for the
//! environment" beside it, and what a holder of the pair then does is `POST
//! <WebServerHostname>/aws_mwaa/cli` with `Authorization: Bearer <CliToken>` and an Apache
//! Airflow CLI command as the request body. That endpoint is the web server's own, so it is
//! in no Smithy model; it answers HTTP 200 with a JSON document whose `stdout` and `stderr`
//! members carry the command's output, base64-encoded.
//!
//! Two halves live here, because they are two halves of one thing:
//!
//! * the TOKENS. A token authenticates to the environment's Apache Airflow web server, so
//!   the environment has to be able to recognise the one it was minted for: the minting
//!   operation remembers the token's DIGEST on that environment's record ([`remembered`
//!   below](EnvironmentRecord::remember_cli_token)) and the endpoint resolves a presented
//!   token back to the environment that minted it. The token itself is never stored — a
//!   store that held it could hand it out again — and the digest is what makes a token
//!   minted for one environment useless against another.
//! * the COMMANDS. A command is READ and then ANSWERED FROM THE ENVIRONMENT'S OWN STORED
//!   Apache Airflow metadata — the variables and slot pools its REST API
//!   (`crate::ops::invoke_rest_api`) reads and writes, and the Apache Airflow version its
//!   configuration names. Nothing is echoed back: `variables list` lists what the store
//!   holds, and a command that CHANGES something changes that same store, so a change made
//!   through the CLI is read back through the REST API and the other way round.
//!
//! # What `dags list` answers, and why
//!
//! An Apache Airflow DAG is defined by the Python file the environment loads from its
//! Amazon S3 DAG path, and its `dag_id` is whatever that file's code sets when the DAG
//! processor imports it — so the inventory can only be known by EXECUTING those files, and
//! no Apache Airflow DAG processor runs in this process (see
//! [`crate::state::AirflowMetadata`]). The inventory is therefore empty, and `dags list`
//! answers what the Apache Airflow CLI prints for a collection with no rows: `No data
//! found`. That is the computed answer for an empty inventory, not a placeholder.
//!
//! # The commands this CLI serves
//!
//! The groups and commands below are the ones whose answer this emulator can compute from
//! stored state. Anything else is answered the way the Apache Airflow CLI's own argument
//! parser answers an unknown command: a usage error on `stderr`, with nothing done — never
//! an empty success, which a client cannot tell from a command that ran and had nothing to
//! say.

use std::collections::BTreeMap;

use super::{
    AirflowPool, AirflowVariable, ComAmazonawsMwaaAmazonmwaaState, EnvironmentRecord,
    MAX_AIRFLOW_KEY_LENGTH, MAX_AIRFLOW_POOLS, MAX_AIRFLOW_VALUE_LENGTH, MAX_AIRFLOW_VARIABLES,
};

/// How many CLI tokens one environment recognises at a time.
///
/// Every `CreateCliToken` mints a fresh token, so without a ceiling a caller looping on it
/// would grow this process's memory without bound. Once the ledger is full the token minted
/// LONGEST ago makes room, so the tokens a client is actually using — the ones it just
/// minted — are the ones that still authenticate. The number is the emulator's own ceiling
/// and no fidelity claim about how many tokens Amazon MWAA keeps live for an environment.
pub const MAX_CLI_TOKENS: usize = 16;

/// The command name the Apache Airflow CLI itself is invoked under, which a client may
/// leave in front of the command it sends (`airflow dags list`) or leave out (`dags list`,
/// the form the Amazon MWAA documentation sends).
const CLI_PROGRAM: &str = "airflow";

/// The option the Apache Airflow CLI takes an output FORMAT under (`-o`, `--output`).
const OUTPUT_OPTION: [&str; 2] = ["-o", "--output"];

/// The output format the CLI prints a collection in when the command names none, and the
/// one every other format is named against.
const OUTPUT_TABLE: &str = "table";

/// The output format that prints a collection as a JSON array of its rows.
const OUTPUT_JSON: &str = "json";

/// The formats this CLI prints a collection in, in the order its parser lists them.
const OUTPUT_FORMATS: [&str; 2] = [OUTPUT_JSON, OUTPUT_TABLE];

/// What the Apache Airflow CLI prints for a collection that holds no rows
/// (`airflow.cli.simple_table.AirflowConsole.print_as_table`).
const NO_DATA: &str = "No data found";

/// The columns `dags list` prints, from the mapper that command hands the console
/// (`dag_id`, `filepath`, `owner`, `paused`).
const DAG_COLUMNS: [&str; 4] = ["dag_id", "filepath", "owner", "paused"];

/// The column `variables list` prints: a variable's key.
const VARIABLE_COLUMNS: [&str; 1] = ["key"];

/// The columns the pool commands print for a slot pool.
const POOL_COLUMNS: [&str; 4] = ["pool", "slots", "description", "include_deferred"];

/// The command groups this CLI serves, in the order a usage error lists them.
const GROUPS: [&str; 4] = ["dags", "pools", "variables", "version"];

/// What one command run through the CLI wrote.
///
/// Both streams, because that is what the endpoint reports: a command that printed nothing
/// to one of them carries the empty string for it, and the endpoint leaves an empty stream
/// out of the document it answers with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOutput {
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    /// One line of standard output, as every print of the CLI writes one.
    fn out(line: impl Into<String>) -> Self {
        Self {
            stdout: format!("{}\n", line.into()),
            stderr: String::new(),
        }
    }

    /// Several lines of standard output, in the order they were printed.
    fn lines(lines: &[String]) -> Self {
        Self {
            stdout: lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
            stderr: String::new(),
        }
    }

    /// One usage error, which the CLI's argument parser writes to standard error and which
    /// leaves the environment untouched.
    fn error(message: impl Into<String>) -> Self {
        Self {
            stdout: String::new(),
            stderr: format!("{}\n", message.into()),
        }
    }
}

/// The digest of a CLI token: what an environment remembers instead of the token itself.
///
/// SHA-256, hex-encoded — the one-way reading this crate already uses for its
/// content-derived values — so the ledger holds nothing a reader could present as a
/// credential.
fn token_digest(token: &str) -> String {
    let digest = <::sha2::Sha256 as ::sha2::Digest>::digest(token.as_bytes());
    ::hex::encode(digest)
}

impl EnvironmentRecord {
    /// Remember that `token` was minted for this environment's Apache Airflow CLI, so a
    /// request presenting it resolves to this environment ([`Self::accepts_cli_token`]).
    ///
    /// Only the digest is kept ([`token_digest`]), and the ledger is bounded
    /// ([`MAX_CLI_TOKENS`]).
    pub fn remember_cli_token(&mut self, token: &str) {
        let digest = token_digest(token);
        if self.cli_tokens.contains(&digest) {
            return;
        }
        while self.cli_tokens.len() >= MAX_CLI_TOKENS {
            self.cli_tokens.pop_front();
        }
        self.cli_tokens.push_back(digest);
    }

    /// Whether `token` is a CLI token this environment minted and still recognises.
    pub fn accepts_cli_token(&self, token: &str) -> bool {
        let digest = token_digest(token);
        self.cli_tokens.iter().any(|known| *known == digest)
    }

    /// Whether this environment's Apache Airflow WEB SERVER is up, which is what decides
    /// whether its CLI endpoint answers a command at all.
    ///
    /// The web server is a component of a RUNNING environment: an environment still being
    /// created has none yet (`CREATING` is "the request to create the environment is in
    /// progress", `PENDING` waits for VPC endpoints, and `CREATE_FAILED` never finished),
    /// and one being torn down has none any more. An environment under an accepted update
    /// keeps serving — its database is being snapshotted or its workers replaced, which is
    /// what those statuses say — so a command addressed to it is answered.
    pub fn webserver_is_running(&self) -> bool {
        matches!(
            self.status.as_str(),
            super::STATUS_AVAILABLE | super::STATUS_CREATING_SNAPSHOT | super::STATUS_UPDATING
        )
    }
}

impl ComAmazonawsMwaaAmazonmwaaState {
    /// The environment whose Apache Airflow CLI `token` authenticates to, for running a
    /// command against it, or `None` when no stored environment minted that token.
    ///
    /// A request to `/aws_mwaa/cli` carries no credential scope and no environment name —
    /// the token is the whole of what identifies the environment, which is what makes a
    /// token minted for one environment useless against another — so the lookup is over
    /// every stored environment of every Region rather than scoped to one.
    pub fn environment_for_cli_token(&mut self, token: &str) -> Option<&mut EnvironmentRecord> {
        self.environments
            .values_mut()
            .find(|record| record.accepts_cli_token(token))
    }
}

/// Run one Apache Airflow CLI `command` against `environment`, as the environment's web
/// server runs it: the answer is computed from (and, for a command that changes something,
/// written to) that environment's own stored Apache Airflow metadata.
///
/// The command line is split the way a shell splits one, so a quoted value travels as one
/// argument (`variables set greeting "hello world"`).
pub fn run(environment: &mut EnvironmentRecord, command: &str) -> CliOutput {
    let Some(arguments) = ::shlex::split(command) else {
        // An unbalanced quote is not a command line at all: there is no argument list to
        // dispatch on, which is what the shell reports rather than running something else.
        return CliOutput::error(format!(
            "{CLI_PROGRAM}: error: cannot parse command: unbalanced quoting in {command:?}"
        ));
    };
    let mut arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    if arguments.first() == Some(&CLI_PROGRAM) {
        arguments.remove(0);
    }
    let Some((group, rest)) = arguments.split_first() else {
        return CliOutput::error(format!(
            "{CLI_PROGRAM}: error: the following arguments are required: GROUP_OR_COMMAND"
        ));
    };
    match *group {
        "version" => version(environment, rest),
        "dags" => dags(environment, rest),
        "variables" => variables(environment, rest),
        "pools" => pools(environment, rest),
        unknown => CliOutput::error(invalid_choice("GROUP_OR_COMMAND", unknown, &GROUPS)),
    }
}

/// `airflow version`: the Apache Airflow release this environment runs, read off its stored
/// configuration.
fn version(environment: &EnvironmentRecord, rest: &[&str]) -> CliOutput {
    match rest.first() {
        None => CliOutput::out(environment.spec.airflow_version.clone()),
        Some(extra) => CliOutput::error(unrecognised("version", extra)),
    }
}

/// The `dags` group.
fn dags(environment: &EnvironmentRecord, rest: &[&str]) -> CliOutput {
    let Some((command, rest)) = rest.split_first() else {
        return CliOutput::error(missing_command("dags"));
    };
    match *command {
        "list" => match output_format(rest) {
            // The inventory is empty, and for the reason this module's docs give: no DAG
            // processor imports the environment's DAG files, so nothing names a `dag_id`.
            // The rows are still rendered through the one renderer, so an empty inventory
            // prints what the CLI prints for an empty collection.
            Ok(format) => collection(&DAG_COLUMNS, &dag_rows(environment), format),
            Err(refusal) => refusal,
        },
        unknown => CliOutput::error(invalid_choice("COMMAND", unknown, &["list"])),
    }
}

/// The rows `dags list` prints for this environment: one per DAG its DAG processor
/// imported, which is none.
fn dag_rows(_environment: &EnvironmentRecord) -> Vec<Vec<String>> {
    Vec::new()
}

/// The `variables` group.
fn variables(environment: &mut EnvironmentRecord, rest: &[&str]) -> CliOutput {
    const COMMANDS: [&str; 4] = ["delete", "get", "list", "set"];
    let Some((command, rest)) = rest.split_first() else {
        return CliOutput::error(missing_command("variables"));
    };
    match *command {
        "list" => match output_format(rest) {
            Ok(format) => {
                let rows: Vec<Vec<String>> = environment
                    .airflow
                    .variables
                    .values()
                    .map(|variable| vec![variable.key.clone()])
                    .collect();
                collection(&VARIABLE_COLUMNS, &rows, format)
            }
            Err(refusal) => refusal,
        },
        "get" => match positional("variables get", rest, ["key"]) {
            Ok(([key], extra)) => no_more_arguments("variables get", extra).unwrap_or_else(|| {
                match environment.airflow.variables.get(key) {
                    Some(variable) => CliOutput::out(variable.value.clone()),
                    // What the CLI reports for a key the metadata database does not hold.
                    None => CliOutput::error(format!("Variable {key} does not exist")),
                }
            }),
            Err(refusal) => refusal,
        },
        "set" => match positional("variables set", rest, ["key", "value"]) {
            Ok(([key, value], extra)) => no_more_arguments("variables set", extra)
                .unwrap_or_else(|| set_variable(environment, key, value)),
            Err(refusal) => refusal,
        },
        "delete" => match positional("variables delete", rest, ["key"]) {
            Ok(([key], extra)) => {
                no_more_arguments("variables delete", extra).unwrap_or_else(|| {
                    match environment.airflow.variables.remove(key) {
                        Some(_removed) => CliOutput::out(format!("Variable {key} deleted")),
                        None => CliOutput::error(format!("Variable {key} does not exist")),
                    }
                })
            }
            Err(refusal) => refusal,
        },
        unknown => CliOutput::error(invalid_choice("COMMAND", unknown, &COMMANDS)),
    }
}

/// `variables set KEY VALUE`: store the pair, under the same ceilings the environment's
/// REST API stores one under — this is the same metadata database, so what one face of it
/// refuses the other cannot accept.
fn set_variable(environment: &mut EnvironmentRecord, key: &str, value: &str) -> CliOutput {
    if let Some(refusal) = too_long("key", key, MAX_AIRFLOW_KEY_LENGTH) {
        return refusal;
    }
    if let Some(refusal) = too_long("value", value, MAX_AIRFLOW_VALUE_LENGTH) {
        return refusal;
    }
    let variables = &mut environment.airflow.variables;
    if !variables.contains_key(key) && variables.len() >= MAX_AIRFLOW_VARIABLES {
        return CliOutput::error(format!(
            "Variable {key} was not created: this environment holds the most variables this \
             emulator keeps ({MAX_AIRFLOW_VARIABLES})"
        ));
    }
    variables.insert(
        key.to_string(),
        AirflowVariable {
            key: key.to_string(),
            value: value.to_string(),
            // The CLI's own `variables set` carries no description, so the stored variable
            // has none: a description invented here would describe nothing a caller wrote.
            description: None,
        },
    );
    CliOutput::out(format!("Variable {key} created"))
}

/// The `pools` group.
fn pools(environment: &mut EnvironmentRecord, rest: &[&str]) -> CliOutput {
    const COMMANDS: [&str; 4] = ["delete", "get", "list", "set"];
    let Some((command, rest)) = rest.split_first() else {
        return CliOutput::error(missing_command("pools"));
    };
    match *command {
        "list" => match output_format(rest) {
            Ok(format) => {
                let rows: Vec<Vec<String>> =
                    environment.airflow.pools.values().map(pool_row).collect();
                collection(&POOL_COLUMNS, &rows, format)
            }
            Err(refusal) => refusal,
        },
        "get" => match positional("pools get", rest, ["pool"]) {
            Ok(([name], extra)) => match output_format(extra) {
                Ok(format) => match environment.airflow.pools.get(name) {
                    Some(pool) => collection(&POOL_COLUMNS, &[pool_row(pool)], format),
                    None => CliOutput::error(format!("Pool {name} does not exist")),
                },
                Err(refusal) => refusal,
            },
            Err(refusal) => refusal,
        },
        "set" => match positional("pools set", rest, ["pool", "slots", "description"]) {
            Ok(([name, slots, description], extra)) => no_more_arguments("pools set", extra)
                .unwrap_or_else(|| set_pool(environment, name, slots, description)),
            Err(refusal) => refusal,
        },
        "delete" => match positional("pools delete", rest, ["pool"]) {
            Ok(([name], extra)) => no_more_arguments("pools delete", extra).unwrap_or_else(|| {
                match environment.airflow.pools.remove(name) {
                    Some(_removed) => CliOutput::out(format!("Pool {name} deleted")),
                    None => CliOutput::error(format!("Pool {name} does not exist")),
                }
            }),
            Err(refusal) => refusal,
        },
        unknown => CliOutput::error(invalid_choice("COMMAND", unknown, &COMMANDS)),
    }
}

/// `pools set POOL SLOTS DESCRIPTION`: store the pool, under the same ceilings the
/// environment's REST API stores one under.
fn set_pool(
    environment: &mut EnvironmentRecord,
    name: &str,
    slots: &str,
    description: &str,
) -> CliOutput {
    let Ok(slots) = slots.parse::<i64>() else {
        return CliOutput::error(format!(
            "{CLI_PROGRAM} pools set: error: argument slots: invalid int value: {slots:?}"
        ));
    };
    if slots < 0 {
        return CliOutput::error(format!(
            "Pool {name} was not created: slots must be zero or more, got {slots}"
        ));
    }
    if let Some(refusal) = too_long("pool", name, MAX_AIRFLOW_KEY_LENGTH) {
        return refusal;
    }
    if let Some(refusal) = too_long("description", description, MAX_AIRFLOW_VALUE_LENGTH) {
        return refusal;
    }
    let existing = environment.airflow.pools.get(name);
    if existing.is_none() && environment.airflow.pools.len() >= MAX_AIRFLOW_POOLS {
        return CliOutput::error(format!(
            "Pool {name} was not created: this environment holds the most slot pools this \
             emulator keeps ({MAX_AIRFLOW_POOLS})"
        ));
    }
    // `include_deferred` is not a positional of this command, so a pool that already
    // exists keeps the value it was stored with and a new one gets the CLI's own default.
    let include_deferred = existing.is_some_and(|pool| pool.include_deferred);
    environment.airflow.pools.insert(
        name.to_string(),
        AirflowPool {
            name: name.to_string(),
            slots,
            description: Some(description.to_string()),
            include_deferred,
        },
    );
    CliOutput::out(format!("Pool {name} created"))
}

/// The row the pool commands print for one stored pool, in [`POOL_COLUMNS`] order.
fn pool_row(pool: &AirflowPool) -> Vec<String> {
    vec![
        pool.name.clone(),
        pool.slots.to_string(),
        pool.description.clone().unwrap_or_default(),
        pool.include_deferred.to_string(),
    ]
}

/// The output format `rest` asks for, or the usage error its value earns.
///
/// `-o`/`--output` is the only option the collection commands take here; anything else is
/// an unrecognised argument, which is what the CLI's parser says about it.
fn output_format<'a>(rest: &[&'a str]) -> Result<&'a str, CliOutput> {
    match rest {
        [] => Ok(OUTPUT_TABLE),
        [option, value] if OUTPUT_OPTION.contains(option) => {
            if OUTPUT_FORMATS.contains(value) {
                Ok(value)
            } else {
                Err(CliOutput::error(invalid_choice(
                    "-o/--output",
                    value,
                    &OUTPUT_FORMATS,
                )))
            }
        }
        [option] if OUTPUT_OPTION.contains(option) => Err(CliOutput::error(format!(
            "{CLI_PROGRAM}: error: argument {option}: expected one argument"
        ))),
        [extra, ..] => Err(CliOutput::error(format!(
            "{CLI_PROGRAM}: error: unrecognized arguments: {extra}"
        ))),
    }
}

/// The `N` positional arguments `rest` carries for `command`, which takes exactly `names`,
/// together with whatever follows them.
///
/// A command called with too few arguments is the parser's "the following arguments are
/// required" error, and nothing is run. The arguments are handed back as an ARRAY, so a
/// caller reads each one by destructuring rather than by indexing a slice whose length it
/// would have to trust.
fn positional<'a, 'r, const N: usize>(
    command: &str,
    rest: &'r [&'a str],
    names: [&str; N],
) -> Result<([&'a str; N], &'r [&'a str]), CliOutput> {
    let missing = || {
        let missing = names
            .get(rest.len()..)
            .unwrap_or_default()
            .join(", ");
        CliOutput::error(format!(
            "{CLI_PROGRAM} {command}: error: the following arguments are required: {missing}"
        ))
    };
    if rest.len() < N {
        return Err(missing());
    }
    let (taken, extra) = rest.split_at(N);
    // Exactly `N` long by the test above, so the conversion holds; a value it could not
    // hold earns the same usage error rather than a panic.
    let Ok(taken) = <[&'a str; N]>::try_from(taken) else {
        return Err(missing());
    };
    Ok((taken, extra))
}

/// The parser's error for an argument a command does not take, or `None` when `rest` is
/// empty — which is what a command taking no options past its positionals requires.
fn no_more_arguments(command: &str, rest: &[&str]) -> Option<CliOutput> {
    rest.first()
        .map(|extra| CliOutput::error(unrecognised(command, extra)))
}

/// `value` is longer than this store keeps a `member` of that kind, or `None` when it fits.
fn too_long(member: &str, value: &str, longest: usize) -> Option<CliOutput> {
    (value.chars().count() > longest).then(|| {
        CliOutput::error(format!(
            "{CLI_PROGRAM}: error: argument {member}: longer than the {longest} characters \
             this emulator keeps"
        ))
    })
}

/// The parser's error for a value outside the set an argument admits.
fn invalid_choice(argument: &str, value: &str, choices: &[&str]) -> String {
    let choices = choices
        .iter()
        .map(|choice| format!("'{choice}'"))
        .collect::<Vec<String>>()
        .join(", ");
    format!(
        "{CLI_PROGRAM}: error: argument {argument}: invalid choice: '{value}' (choose from \
         {choices})"
    )
}

/// The parser's error for a group called with no command of its own.
fn missing_command(group: &str) -> String {
    format!("{CLI_PROGRAM} {group}: error: the following arguments are required: COMMAND")
}

/// The parser's error for an argument a command does not take.
fn unrecognised(command: &str, extra: &str) -> String {
    format!("{CLI_PROGRAM} {command}: error: unrecognized arguments: {extra}")
}

/// One collection, printed in the format the command asked for.
fn collection(columns: &[&str], rows: &[Vec<String>], format: &str) -> CliOutput {
    if format == OUTPUT_JSON {
        return CliOutput::out(json_rows(columns, rows));
    }
    table(columns, rows)
}

/// A collection as the CLI's JSON output prints it: an array of objects, one per row, whose
/// members are the columns.
fn json_rows(columns: &[&str], rows: &[Vec<String>]) -> String {
    let documents: Vec<::serde_json::Value> = rows
        .iter()
        .map(|row| {
            let members: BTreeMap<&str, &str> = columns
                .iter()
                .copied()
                .zip(row.iter().map(String::as_str))
                .collect();
            ::serde_json::json!(members)
        })
        .collect();
    ::serde_json::Value::Array(documents).to_string()
}

/// A collection as the CLI tabulates one: a header of the column names, then one line per
/// row, each column padded to the widest value in it — and [`NO_DATA`] for a collection
/// that holds no rows, which is what the CLI's console prints for one.
fn table(columns: &[&str], rows: &[Vec<String>]) -> CliOutput {
    if rows.is_empty() {
        return CliOutput::out(NO_DATA);
    }
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(column, name)| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|value| value.chars().count())
                .chain([name.chars().count()])
                .max()
                .unwrap_or_default()
        })
        .collect();
    let line = |values: &[&str]| {
        values
            .iter()
            .zip(&widths)
            .map(|(value, width)| format!("{value:<width$}"))
            .collect::<Vec<String>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut lines = vec![line(columns)];
    for row in rows {
        let values: Vec<&str> = row.iter().map(String::as_str).collect();
        lines.push(line(&values));
    }
    CliOutput::lines(&lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An environment to run commands against: the record a create stores, with its
    /// metadata database as Apache Airflow seeds one.
    fn environment() -> EnvironmentRecord {
        let mut record = EnvironmentRecord {
            name: "MyMWAAEnvironment".to_string(),
            arn: "arn:aws:airflow:us-east-1:000000000000:environment/MyMWAAEnvironment"
                .to_string(),
            region: "us-east-1".to_string(),
            status: super::super::STATUS_AVAILABLE.to_string(),
            pending: Default::default(),
            created_at: ::std::time::SystemTime::UNIX_EPOCH,
            webserver_url: "127.0.0.1:9297".to_string(),
            service_role_arn: String::new(),
            celery_executor_queue: String::new(),
            webserver_vpc_endpoint_service: String::new(),
            database_vpc_endpoint_service: String::new(),
            spec: Default::default(),
            last_update: None,
            tags: Default::default(),
            airflow: Default::default(),
            cli_tokens: Default::default(),
        };
        record.spec.airflow_version = super::super::LATEST_AIRFLOW_VERSION.to_string();
        record
    }

    /// RULE: a token authenticates to the environment it was minted for and to no other,
    /// and the ledger holds the most recently minted ones.
    #[test]
    fn a_token_authenticates_to_the_environment_that_minted_it() {
        let mut one = environment();
        let mut another = environment();
        one.remember_cli_token("token-of-one");
        another.remember_cli_token("token-of-another");
        for (case, record, token, expected) in [
            ("its own token", &one, "token-of-one", true),
            ("another environment's token", &one, "token-of-another", false),
            ("a token nothing minted", &one, "made-up", false),
            ("its own token, elsewhere", &another, "token-of-another", true),
        ] {
            let accepted = record.accepts_cli_token(token);
            assert_eq!(
                accepted, expected,
                "case {case:?}: expected accepts_cli_token {expected}, got {accepted}"
            );
        }

        // Repeating a mint does not fill the ledger with one token.
        one.remember_cli_token("token-of-one");
        assert_eq!(
            one.cli_tokens.len(),
            1,
            "case \"a token minted twice\": expected one entry, got {}",
            one.cli_tokens.len()
        );

        // Past the ceiling the oldest token makes room, and the newest still authenticate.
        let minted: Vec<String> = (0..=MAX_CLI_TOKENS)
            .map(|index| format!("token-{index}"))
            .collect();
        let mut filling = environment();
        for token in &minted {
            filling.remember_cli_token(token);
        }
        assert_eq!(
            filling.cli_tokens.len(),
            MAX_CLI_TOKENS,
            "case \"past the ceiling\": expected {MAX_CLI_TOKENS} entries, got {}",
            filling.cli_tokens.len()
        );
        assert!(
            !filling.accepts_cli_token(&minted[0]),
            "case \"the token minted longest ago\": expected it to have made room"
        );
        assert!(
            filling.accepts_cli_token(minted.last().expect("a minted token")),
            "case \"the token minted last\": expected it to authenticate"
        );
        assert!(
            !filling.cli_tokens.iter().any(|held| held.contains("token-")),
            "no ledger entry may hold the token itself, got {:?}",
            filling.cli_tokens
        );
    }

    /// RULE: a command is answered from the environment's own stored metadata, and a
    /// command that changes something changes that same store.
    #[test]
    fn a_command_is_answered_from_the_stored_metadata() {
        let mut record = environment();
        // (case, command, expected stdout, expected stderr)
        let rows: Vec<(&str, &str, String, String)> = vec![
            (
                "the Apache Airflow version of the stored configuration",
                "version",
                format!("{}\n", super::super::LATEST_AIRFLOW_VERSION),
                String::new(),
            ),
            (
                "the CLI's own program name in front of the command",
                "airflow version",
                format!("{}\n", super::super::LATEST_AIRFLOW_VERSION),
                String::new(),
            ),
            (
                "an empty DAG inventory",
                "dags list",
                format!("{NO_DATA}\n"),
                String::new(),
            ),
            (
                "no variables yet",
                "variables list",
                format!("{NO_DATA}\n"),
                String::new(),
            ),
            (
                "a variable set through the CLI",
                "variables set greeting \"hello world\"",
                "Variable greeting created\n".to_string(),
                String::new(),
            ),
            (
                "that variable read back",
                "variables get greeting",
                "hello world\n".to_string(),
                String::new(),
            ),
            (
                "that variable listed",
                "variables list",
                "key\ngreeting\n".to_string(),
                String::new(),
            ),
            (
                "that variable as JSON",
                "variables list -o json",
                "[{\"key\":\"greeting\"}]\n".to_string(),
                String::new(),
            ),
            (
                "the seeded slot pool",
                "pools list",
                format!(
                    "pool          slots  description   include_deferred\n{}  {}    {}  false\n",
                    super::super::DEFAULT_POOL_NAME,
                    super::super::DEFAULT_POOL_SLOTS,
                    super::super::DEFAULT_POOL_DESCRIPTION,
                ),
                String::new(),
            ),
            (
                "a variable removed through the CLI",
                "variables delete greeting",
                "Variable greeting deleted\n".to_string(),
                String::new(),
            ),
            (
                "a key the store no longer holds",
                "variables get greeting",
                String::new(),
                "Variable greeting does not exist\n".to_string(),
            ),
            (
                "a group this CLI does not serve",
                "celery worker",
                String::new(),
                format!("{}\n", invalid_choice("GROUP_OR_COMMAND", "celery", &GROUPS)),
            ),
            (
                "a command the group does not serve",
                "dags trigger",
                String::new(),
                format!("{}\n", invalid_choice("COMMAND", "trigger", &["list"])),
            ),
            (
                "a command missing its argument",
                "variables get",
                String::new(),
                format!(
                    "{CLI_PROGRAM} variables get: error: the following arguments are \
                     required: key\n"
                ),
            ),
            (
                "an output format this CLI does not print",
                "variables list -o yaml",
                String::new(),
                format!(
                    "{}\n",
                    invalid_choice("-o/--output", "yaml", &OUTPUT_FORMATS)
                ),
            ),
            (
                "a command line that cannot be split",
                "variables set key \"unbalanced",
                String::new(),
                format!(
                    "{CLI_PROGRAM}: error: cannot parse command: unbalanced quoting in \
                     \"variables set key \\\"unbalanced\"\n"
                ),
            ),
            (
                "no command at all",
                "",
                String::new(),
                format!(
                    "{CLI_PROGRAM}: error: the following arguments are required: \
                     GROUP_OR_COMMAND\n"
                ),
            ),
        ];
        for (case, command, stdout, stderr) in rows {
            let answered = run(&mut record, command);
            assert_eq!(
                answered.stdout, stdout,
                "case {case:?}: expected stdout {stdout:?}, got {:?}",
                answered.stdout
            );
            assert_eq!(
                answered.stderr, stderr,
                "case {case:?}: expected stderr {stderr:?}, got {:?}",
                answered.stderr
            );
        }

        // A pool written through the CLI is in the store the REST API reads, with the
        // slots the command named.
        let answered = run(&mut record, "pools set priority 7 \"the priority pool\"");
        assert_eq!(
            answered.stdout, "Pool priority created\n",
            "case \"a pool set through the CLI\": got {answered:?}"
        );
        let stored = record
            .airflow
            .pools
            .get("priority")
            .expect("case \"a pool set through the CLI\": the store must hold it");
        assert_eq!(
            stored.slots, 7,
            "case \"a pool set through the CLI\": expected 7 slots, got {}",
            stored.slots
        );
    }
}

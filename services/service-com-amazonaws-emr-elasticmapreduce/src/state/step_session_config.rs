// SPDX-License-Identifier: CC0-1.0

//! The SESSION CONFIGURATION a cluster's SQL client holds: what its `set` command reads and
//! writes.
//!
//! # The rule this module implements
//!
//! `set` is not a statement the SQL driver runs: it is a command of the CLIENT itself, which
//! Hive's CLI answers out of the configuration its session holds ("`set <key>=<value>`" sets a
//! property for the session, "`set <key>`" prints what the session holds for it, and `set`
//! with no property named prints the properties it holds — Apache Hive's "Hive CLI" and
//! "Configuration properties" documentation). So an invocation carrying one runs: the client
//! applies or reports the property and exits zero, and NOTHING of the metastore or of a
//! relation's data is read or changed for it.
//!
//! What a session HOLDS is the cluster's own configuration — "the list of configurations
//! supplied for the EMR cluster you are creating" (`RunJobFlow$Configurations` doc), which is
//! where the properties of a cluster's Hive and Spark SQL engines come from — with whatever
//! the invocation itself set over it. So the value printed for a property is the one the
//! launch really recorded for that cluster, read out of the stored cluster record
//! ([`configuration_properties`]), and a property no configuration of the cluster carries is
//! reported as the client reports one it holds no value for ([`UNDEFINED_SUFFIX`]) rather than
//! as a value this emulator does not have.
//!
//! A property set by one invocation is held for that invocation alone: each `set <key>=<value>`
//! is a command of ONE client process, whose session ends with it, so nothing of it outlives
//! the command line the step submitted and no later caller sees a cluster whose configuration
//! a step rewrote.

use ::std::collections::BTreeMap;

use sqlparser::ast::{Expr, Set, Statement, Value};

/// The name a SQL client's own session-configuration command is written under.
pub const SET_COMMAND: &str = "set";

/// The option that asks for every property rather than one, as the client spells it
/// (`set -v`).
const VERBOSE_OPTION: &str = "-v";

/// How what the client prints names a property that has no value: the wording Hive's CLI
/// answers a `set <key>` for a property its session holds nothing under with.
pub const UNDEFINED_SUFFIX: &str = " is undefined";

/// How many properties ONE listing prints. CHOSEN: what a client's invocation writes reaches
/// its step's own log files, which keep a bounded amount of it ([`super::step_logs`]), so the
/// listing is bounded too.
const MAX_PROPERTIES_LISTED: usize = 512;

/// How long a property's name or value may be for this reading to take it. CHOSEN: a bound on
/// what one `set` command carries, in the units a configuration property is written in.
const MAX_PROPERTY_BYTES: usize = 1024;

/// What ONE `set` command of a client's session asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSetting {
    /// `set <key>=<value>`: what this session holds for that property from here on.
    Assign { key: String, value: String },
    /// `set <key>`: what the session holds for that property.
    Print(String),
    /// `set` with no property named: every property the session holds.
    PrintAll,
}

/// The `set` command `text` is, or `None` when it is no `set` command at all.
///
/// `text` is ONE command of the client's input, as the client's own reader hands it over: the
/// leading word names the command and the rest is what it was given. A form that names
/// something other than a property — a transaction characteristic, a role, a time zone, the
/// forms the SQL grammar reads a `SET` as — carries whitespace in the place a property name
/// stands, so it is no property this reads and the statement is left to the driver.
#[must_use]
pub fn session_setting(text: &str) -> Option<SessionSetting> {
    let command = text.trim().trim_end_matches(';');
    let (word, rest) = match command.trim().split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (command.trim(), ""),
    };
    if !word.eq_ignore_ascii_case(SET_COMMAND) {
        return None;
    }
    if rest.is_empty() || rest == VERBOSE_OPTION {
        return Some(SessionSetting::PrintAll);
    }
    match rest.split_once('=') {
        Some((key, value)) => assignment(key.trim(), value.trim()),
        None => named_property(rest).map(|key| SessionSetting::Print(key.to_string())),
    }
}

/// The session setting a SQL statement IS, where the grammar read the client's `set` command
/// as one.
///
/// `set <key>=<value>` is a statement the SQL grammar accepts, so a text carrying one parses
/// as a statement list and this is where that statement is recognised: it is the same command
/// of the client, and it is performed as one rather than submitted to the metastore.
#[must_use]
pub(super) fn statement_setting(statement: &Statement) -> Option<SessionSetting> {
    let Statement::Set(Set::SingleAssignment {
        variable, values, ..
    }) = statement
    else {
        return None;
    };
    let [value] = values.as_slice() else {
        return None;
    };
    assignment(&property_name(&variable.to_string()), &printed_expression(value))
}

/// The assignment of `value` to `key`, or `None` when either is past what one command carries
/// or `key` names no property.
fn assignment(key: &str, value: &str) -> Option<SessionSetting> {
    let key = named_property(key)?;
    (value.len() <= MAX_PROPERTY_BYTES).then(|| SessionSetting::Assign {
        key: key.to_string(),
        value: value.to_string(),
    })
}

/// `name` as a property name, or `None` when it is no single property name: a configuration
/// property is one word, so a value carrying whitespace names none, and so does an empty one
/// or one past the bound on what a command carries.
fn named_property(name: &str) -> Option<&str> {
    let name = name.trim();
    (!name.is_empty()
        && name.len() <= MAX_PROPERTY_BYTES
        && !name.contains(char::is_whitespace))
    .then_some(name)
}

/// A property name as the parse rendered it: the dotted name without the quoting a dialect
/// spells its parts with.
fn property_name(rendered: &str) -> String {
    rendered
        .split('.')
        .map(|part| part.trim_matches(|character| character == '`' || character == '"'))
        .collect::<Vec<&str>>()
        .join(".")
}

/// The VALUE a parsed assignment carries, as the client reads it: the text of a literal, of a
/// quoted string without its quotes, or of the expression as it was written.
fn printed_expression(value: &Expr) -> String {
    match value {
        Expr::Value(held) => printed_value(&held.value),
        Expr::Identifier(name) => name.value.clone(),
        other => other.to_string(),
    }
}

/// See [`printed_expression`]: one literal as the client reads it.
fn printed_value(value: &Value) -> String {
    match value {
        Value::SingleQuotedString(text)
        | Value::DoubleQuotedString(text)
        | Value::EscapedStringLiteral(text)
        | Value::UnicodeStringLiteral(text)
        | Value::SingleQuotedByteStringLiteral(text)
        | Value::DoubleQuotedByteStringLiteral(text) => text.clone(),
        Value::Number(number, _) => number.clone(),
        Value::Boolean(held) => held.to_string(),
        other => other.to_string(),
    }
}

/// The configuration ONE invocation of a cluster's SQL client reads and writes.
///
/// `cluster` is what the launch recorded for the cluster the client runs on, and `session`
/// what this invocation has set over it — which is why the overlay is held BY the invocation:
/// each client process has a session of its own, and nothing it set outlives it.
#[derive(Debug, Clone, Default)]
pub struct SessionConfiguration {
    cluster: BTreeMap<String, String>,
    session: BTreeMap<String, String>,
}

impl SessionConfiguration {
    /// The configuration a client invocation on a cluster whose configuration carries
    /// `properties` starts with.
    #[must_use]
    pub fn of_cluster(properties: BTreeMap<String, String>) -> Self {
        Self {
            cluster: properties,
            session: BTreeMap::new(),
        }
    }

    /// PERFORM one `set` command and answer the lines the client prints for it.
    ///
    /// An assignment prints nothing, exactly as the client prints nothing for a property it
    /// set; the two reporting forms print the property as `<key>=<value>`, and a property the
    /// session holds no value for is reported as one it holds none for.
    #[must_use]
    pub(super) fn perform(&mut self, setting: &SessionSetting) -> Vec<String> {
        match setting {
            SessionSetting::Assign { key, value } => {
                self.session.insert(key.clone(), value.clone());
                Vec::new()
            }
            SessionSetting::Print(key) => vec![match self.value(key) {
                Some(value) => format!("{key}={value}"),
                None => format!("{key}{UNDEFINED_SUFFIX}"),
            }],
            SessionSetting::PrintAll => self
                .held()
                .into_iter()
                .take(MAX_PROPERTIES_LISTED)
                .map(|(key, value)| format!("{key}={value}"))
                .collect(),
        }
    }

    /// What this session holds for `key`: what the invocation set, and otherwise what the
    /// cluster's own configuration carries.
    fn value(&self, key: &str) -> Option<&String> {
        self.session.get(key).or_else(|| self.cluster.get(key))
    }

    /// Every property this session holds, by name.
    fn held(&self) -> BTreeMap<&String, &String> {
        self.cluster
            .iter()
            .chain(self.session.iter())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::state::{GLUE_METASTORE_FACTORY_CLASS, HIVE_METASTORE_FACTORY_PROPERTY};

    /// RULE: a `set` command names either one property — to report or to assign — or none at
    /// all, and a `SET` form naming something other than a property is no `set` command of the
    /// client.
    #[test]
    fn a_set_command_names_a_property_an_assignment_or_nothing() {
        let rows: [(&str, &str, Option<SessionSetting>); 10] = [
            (
                "a property reported",
                "SET hive.metastore.client.factory.class;",
                Some(SessionSetting::Print(
                    "hive.metastore.client.factory.class".to_string(),
                )),
            ),
            (
                "the lower-cased command",
                "set hive.exec.parallel",
                Some(SessionSetting::Print("hive.exec.parallel".to_string())),
            ),
            (
                "an assignment",
                "SET hive.exec.parallel=true;",
                Some(SessionSetting::Assign {
                    key: "hive.exec.parallel".to_string(),
                    value: "true".to_string(),
                }),
            ),
            (
                "an assignment written with spaces",
                "SET  mapreduce.job.reduces = 4 ",
                Some(SessionSetting::Assign {
                    key: "mapreduce.job.reduces".to_string(),
                    value: "4".to_string(),
                }),
            ),
            ("every property", "SET;", Some(SessionSetting::PrintAll)),
            (
                "every property, verbosely",
                "set -v;",
                Some(SessionSetting::PrintAll),
            ),
            ("a role", "SET ROLE admin;", None),
            ("a time zone", "SET TIME ZONE 'UTC';", None),
            ("another statement", "SELECT 1;", None),
            ("an empty assignment", "SET =value;", None),
        ];
        for (case, text, expected) in rows {
            let actual = session_setting(text);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: what the client prints for a reported property is the value the session holds —
    /// the cluster's own configuration, or what this invocation set over it — and a property
    /// nothing holds a value for is reported as holding none.
    #[test]
    fn a_reported_property_prints_what_the_session_holds() {
        let cluster: BTreeMap<String, String> = [
            (
                HIVE_METASTORE_FACTORY_PROPERTY.to_string(),
                GLUE_METASTORE_FACTORY_CLASS.to_string(),
            ),
            ("hive.exec.parallel".to_string(), "false".to_string()),
        ]
        .into_iter()
        .collect();
        let rows: [(&str, &str, &str); 3] = [
            (
                "a property the launch configured",
                HIVE_METASTORE_FACTORY_PROPERTY,
                GLUE_METASTORE_FACTORY_CLASS,
            ),
            ("another one", "hive.exec.parallel", "false"),
            ("one nothing configured", "hive.cli.print.header", ""),
        ];
        for (case, property, value) in rows {
            let mut configuration = SessionConfiguration::of_cluster(cluster.clone());
            let printed = configuration.perform(&SessionSetting::Print(property.to_string()));
            let expected = if value.is_empty() {
                format!("{property}{UNDEFINED_SUFFIX}")
            } else {
                format!("{property}={value}")
            };
            assert_eq!(
                printed,
                vec![expected.clone()],
                "case {case:?}: expected {expected:?}, got {printed:?}"
            );
        }
    }

    /// RULE: an assignment prints nothing and is what a later report of that property reads,
    /// over whatever the cluster's own configuration carries for it.
    #[test]
    fn an_assignment_prints_nothing_and_is_what_a_later_report_reads() {
        let cluster: BTreeMap<String, String> =
            [("hive.exec.parallel".to_string(), "false".to_string())]
                .into_iter()
                .collect();
        let rows: [(&str, &str, &str); 3] = [
            ("over a configured property", "hive.exec.parallel", "true"),
            ("a property nothing configured", "hive.cli.print.header", "1"),
            ("an empty value", "hive.exec.reducers.max", ""),
        ];
        for (case, property, value) in rows {
            let mut configuration = SessionConfiguration::of_cluster(cluster.clone());
            let printed = configuration.perform(&SessionSetting::Assign {
                key: property.to_string(),
                value: value.to_string(),
            });
            assert!(
                printed.is_empty(),
                "case {case:?}: expected nothing printed, got {printed:?}"
            );
            let reported = configuration.perform(&SessionSetting::Print(property.to_string()));
            let expected = vec![format!("{property}={value}")];
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }
}

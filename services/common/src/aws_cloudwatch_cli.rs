// SPDX-License-Identifier: CC0-1.0

//! The AWS CLI's `aws cloudwatch put-metric-data`, as a program a caller-supplied command line
//! can ask for: what one command line asks of the CLI, and the call that publishes the
//! observations it names.
//!
//! # The rule this module implements
//!
//! Several emulated services run a command line a client supplied — a container's `command`, a
//! cluster step's arguments, a build's commands — and such a line reaches a service of the
//! account through the AWS CLI, which "provides direct access to the public APIs of Amazon Web
//! Services" one command at a time. `aws cloudwatch put-metric-data` "publishes metric data to
//! Amazon CloudWatch" (`PutMetricData`: "publishes metric data to Amazon CloudWatch. CloudWatch
//! associates the data with the specified metric. If the specified metric does not exist,
//! CloudWatch creates the metric"), so a command line that publishes an observation has that
//! observation published: a later read of the metric it names — `GetMetricStatistics`,
//! `GetMetricData`, an alarm evaluating it — sees the datapoint, and the status the command exits
//! with is the status of that call. WHO made the call is whichever principal the session running
//! the line signs with, so CloudWatch judges it against the policies in force on that principal
//! and the command reports the refusal it answered, verbatim as the CLI words a service's own
//! refusal.
//!
//! That is ONE rule, so it lives here once rather than per service, beside the object-store and
//! notification command groups ([`crate::aws_s3_cli`], [`crate::aws_sns_cli`]): two services
//! running the same command line must not publish a different observation or answer a different
//! status. What each service keeps is its own — the SESSION the line runs in and WHICH credential
//! the call is signed with.
//!
//! This module holds the two halves nothing about a caller makes different:
//!
//! * what one command line ASKS for ([`requested_put_metric_data`]) — the command group, the
//!   command and the members of the call, each read as the CLI reads them
//!   ([`crate::aws_cli_options`]). Every other command group, every other command of this group,
//!   and every FORM of this command this program does not perform (a parameter it does not take,
//!   a line missing the members the call requires) is answered `None`, which the caller's session
//!   reports as a command it could not run rather than as a call;
//! * the CALL that publishes it ([`published_metric_data`]), made with the in-process CloudWatch
//!   client the caller hands over ([`crate::cloudwatch_client`]) — so the observation a command
//!   published is the one that metric's own readers answer from.
//!
//! Nothing here builds a client, resolves a name, opens a socket or starts a process. The client
//! these functions are HANDED is the one this workspace constructs for that hop
//! ([`crate::cloudwatch_client`]), whose HTTP client IS the emulator's own in-process callback
//! (`smithy_server_runtime::EmbeddedEmulator::http_client`): a `send()` on it is dispatched inside
//! this process to the emulated CloudWatch service and reaches nothing off it — the SDK's own
//! HTTPS stack is not even compiled in (`aws-sdk-cloudwatch` is declared
//! `default-features = false`), and the endpoint URL that client carries is addressing for that
//! transport rather than a host, built from the Region alone. So the Region a command line names
//! selects an endpoint of THIS emulator and can never be an address, and the metric a command line
//! names is a metric of this emulator.

use crate::aws_cli_options::{CommandLine, OutputFormat, GLOBAL_PARAMETERS};

/// The CLI's command GROUP this program answers for: `aws cloudwatch`, the commands of Amazon
/// CloudWatch.
pub const CLOUDWATCH_COMMAND_GROUP: &str = "cloudwatch";

/// The command of that group this program performs, as the CLI spells it: `put-metric-data`.
/// SOURCE: the `aws cloudwatch` command reference. Every other command of the group — and every
/// other group — is one this program performs nothing of.
pub const PERFORMED_CLOUDWATCH_COMMANDS: [&str; 1] = [PUT_METRIC_DATA_COMMAND];

/// `put-metric-data`, the one command of [`PERFORMED_CLOUDWATCH_COMMANDS`].
const PUT_METRIC_DATA_COMMAND: &str = "put-metric-data";

/// The API operation that command calls, as the CLI names it in its account of a refusal.
pub const PUT_METRIC_DATA_OPERATION: &str = "PutMetricData";

/// The parameters of the command's SHORTHAND form, which names one observation inline: "as an
/// alternative to using the `--metric-data` argument you can use `--metric-name`, `--dimensions`,
/// `--timestamp`, `--value`, `--statistic-values`, `--unit` and `--storage-resolution`" (the
/// `aws cloudwatch put-metric-data` command reference).
const NAMESPACE_PARAMETER: &str = "namespace";
const METRIC_DATA_PARAMETER: &str = "metric-data";
const METRIC_NAME_PARAMETER: &str = "metric-name";
const DIMENSIONS_PARAMETER: &str = "dimensions";
const TIMESTAMP_PARAMETER: &str = "timestamp";
const VALUE_PARAMETER: &str = "value";
const STATISTIC_VALUES_PARAMETER: &str = "statistic-values";
const UNIT_PARAMETER: &str = "unit";
const STORAGE_RESOLUTION_PARAMETER: &str = "storage-resolution";

/// Every parameter this command takes, beside the options every command of the CLI takes.
const TAKEN_PARAMETERS: [&str; 9] = [
    NAMESPACE_PARAMETER,
    METRIC_DATA_PARAMETER,
    METRIC_NAME_PARAMETER,
    DIMENSIONS_PARAMETER,
    TIMESTAMP_PARAMETER,
    VALUE_PARAMETER,
    STATISTIC_VALUES_PARAMETER,
    UNIT_PARAMETER,
    STORAGE_RESOLUTION_PARAMETER,
];

/// The members a datum of the `--metric-data` document carries, as `MetricDatum` spells them —
/// the document form of the shorthand above. A datum carrying any other member is one this
/// program does not publish, so the line it was written on is not an invocation it performs.
const DATUM_MEMBERS: [&str; 7] = [
    "MetricName",
    "Dimensions",
    "Timestamp",
    "Value",
    "StatisticValues",
    "Unit",
    "StorageResolution",
];

/// The four members of a statistic set, each required by the call: "the `StatisticValues`
/// parameter … you must specify `SampleCount`, `Sum`, `Minimum` and `Maximum`" (`StatisticSet`).
const STATISTIC_MEMBERS: [&str; 4] = ["SampleCount", "Sum", "Minimum", "Maximum"];

/// The aggregated observation one datum may carry instead of a single value: "the statistical
/// values for the metric" (`MetricDatum$StatisticValues`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatisticSet {
    /// "The number of samples used for the statistic set."
    pub sample_count: f64,
    /// "The sum of values for the sample set."
    pub sum: f64,
    /// "The minimum value of the sample set."
    pub minimum: f64,
    /// "The maximum value of the sample set."
    pub maximum: f64,
}

/// ONE observation a command line publishes: a `MetricDatum` as the line named it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Datum {
    /// "The name of the metric" (`MetricDatum$MetricName`, required).
    pub metric_name: String,
    /// "The dimensions associated with the metric" (`MetricDatum$Dimensions`), in the order the
    /// line wrote them.
    pub dimensions: Vec<(String, String)>,
    /// "The time the metric data was received" (`MetricDatum$Timestamp`), as the line named it
    /// and read as the instant it names, and `None` for a datum naming none — stamped by
    /// CloudWatch itself.
    pub timestamp: Option<::aws_sdk_cloudwatch::primitives::DateTime>,
    /// "The value for the metric" (`MetricDatum$Value`).
    pub value: Option<f64>,
    /// The aggregated observation, where the line named one instead of a value.
    pub statistic_values: Option<StatisticSet>,
    /// "When you are using a `Put` operation, this defines what unit you want to use when storing
    /// the metric" (`MetricDatum$Unit`).
    pub unit: Option<String>,
    /// "Valid values are 1 and 60" (`MetricDatum$StorageResolution`).
    pub storage_resolution: Option<i32>,
}

/// What ONE `aws cloudwatch put-metric-data` command line asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct PutMetricDataCall {
    /// "The namespace for the metric data" (`PutMetricData$Namespace`, required).
    pub namespace: String,
    /// The observations the line publishes — one for its shorthand form, and the datums of its
    /// `--metric-data` document otherwise.
    pub data: Vec<Datum>,
    /// `--region`, and `None` for a line naming none.
    pub region: Option<String>,
    /// `--output`, the format the answer would be printed in — this operation answers no document
    /// of its own, so nothing is printed either way.
    pub output: OutputFormat,
}

/// What the command line `arguments` — the words AFTER the program name — asks of the CLI's
/// CloudWatch commands, or `None` for every invocation this program performs nothing of.
///
/// It performs [`PUT_METRIC_DATA_COMMAND`] over the parameters that command takes
/// ([`TAKEN_PARAMETERS`]) and the options every command takes. Every other command of the group,
/// every other parameter, a line naming no namespace, a line naming neither a datum document nor
/// a metric inline, one naming both, and a value that is not the type the member takes, is
/// answered nothing for — as is a datum naming neither a value nor a statistic set, since "you
/// must specify either a value or a statistic set" for one (`MetricDatum`).
#[must_use]
pub fn requested_put_metric_data(arguments: &[String]) -> Option<PutMetricDataCall> {
    let mut words = arguments.iter().map(String::as_str);
    if words.next()? != CLOUDWATCH_COMMAND_GROUP {
        return None;
    }
    if !PERFORMED_CLOUDWATCH_COMMANDS.contains(&words.next()?) {
        return None;
    }
    let line = CommandLine::read(words, &[], &[])?;
    let mut taken: Vec<&str> = TAKEN_PARAMETERS.to_vec();
    taken.extend(GLOBAL_PARAMETERS);
    if !line.names_only(&taken) {
        return None;
    }
    let data = match (
        line.one(METRIC_DATA_PARAMETER),
        line.one(METRIC_NAME_PARAMETER),
    ) {
        // The document form: the datums it carries, each read as the call's own member names.
        (Some(document), None) => datums_of(document)?,
        // The shorthand form: the one observation the line names inline.
        (None, Some(metric_name)) => vec![Datum {
            metric_name: metric_name.to_string(),
            dimensions: named_dimensions(line.one(DIMENSIONS_PARAMETER))?,
            timestamp: match line.one(TIMESTAMP_PARAMETER) {
                Some(written) => Some(stamped(written)?),
                None => None,
            },
            value: match line.one(VALUE_PARAMETER) {
                Some(written) => Some(written.parse().ok()?),
                None => None,
            },
            statistic_values: match line.one(STATISTIC_VALUES_PARAMETER) {
                Some(written) => Some(named_statistics(written)?),
                None => None,
            },
            unit: line.one(UNIT_PARAMETER).map(ToString::to_string),
            storage_resolution: match line.one(STORAGE_RESOLUTION_PARAMETER) {
                Some(written) => Some(written.parse().ok()?),
                None => None,
            },
        }],
        // A line naming both forms, or neither, is not a call this command makes.
        _ => return None,
    };
    // A datum names what was OBSERVED: "each `MetricDatum` ... must specify either `Value` or
    // `StatisticValues`" (`PutMetricData`), so a line naming neither is one the CLI's own service
    // would refuse and this reading does not turn into a call.
    if data
        .iter()
        .any(|datum| datum.value.is_none() && datum.statistic_values.is_none())
    {
        return None;
    }
    // The members of the shorthand form name nothing on the document form: a line carrying both
    // would be two observations at once.
    if line.carries(METRIC_DATA_PARAMETER)
        && [
            DIMENSIONS_PARAMETER,
            TIMESTAMP_PARAMETER,
            VALUE_PARAMETER,
            STATISTIC_VALUES_PARAMETER,
            UNIT_PARAMETER,
            STORAGE_RESOLUTION_PARAMETER,
        ]
        .iter()
        .any(|parameter| line.carries(parameter))
    {
        return None;
    }
    Some(PutMetricDataCall {
        // The namespace is the one member the call requires beside the data, so a line naming
        // none is not an invocation.
        namespace: line.one(NAMESPACE_PARAMETER)?.to_string(),
        data,
        region: line.region(),
        output: match line.one(crate::aws_cli_options::OUTPUT_PARAMETER) {
            Some(written) => OutputFormat::of(written)?,
            None => OutputFormat::default(),
        },
    })
}

/// The dimensions a shorthand line named: "the `--dimensions` argument further expands on the
/// identity of a metric using a `Name=Value` pair, separated by commas" (the
/// `aws cloudwatch put-metric-data` command reference) — so `Result=ok,Stage=prod` is two
/// dimensions. A line naming none has none, and a pair this form does not spell is not an
/// invocation.
fn named_dimensions(written: Option<&str>) -> Option<Vec<(String, String)>> {
    let Some(written) = written else {
        return Some(Vec::new());
    };
    written
        .split(',')
        .map(|pair| {
            let (name, value) = pair.split_once('=')?;
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

/// The statistic set a shorthand line named, in the CLI's shorthand for a structure:
/// `SampleCount=3,Sum=12,Minimum=1,Maximum=8`. All four are required by the call, so a form
/// missing one — or carrying a key the structure does not have — is not an invocation.
fn named_statistics(written: &str) -> Option<StatisticSet> {
    let mut held: Vec<(&str, f64)> = Vec::new();
    for pair in written.split(',') {
        let (key, value) = pair.split_once('=')?;
        let key = STATISTIC_MEMBERS
            .iter()
            .find(|member| **member == key)
            .copied()?;
        if held.iter().any(|(named, _)| *named == key) {
            return None;
        }
        held.push((key, value.parse().ok()?));
    }
    let of = |member: &str| {
        held.iter()
            .find(|(named, _)| *named == member)
            .map(|(_, value)| *value)
    };
    Some(StatisticSet {
        sample_count: of("SampleCount")?,
        sum: of("Sum")?,
        minimum: of("Minimum")?,
        maximum: of("Maximum")?,
    })
}

/// The instant `written` names, read as the timestamp that member takes, and `None` for text that
/// is not one — which is a value the CLI refuses before it makes any call, so a line carrying it
/// is not an invocation this program performs (exactly as a `--value` that is not a number is
/// not).
fn stamped(written: &str) -> Option<::aws_sdk_cloudwatch::primitives::DateTime> {
    use ::aws_sdk_cloudwatch::primitives::{DateTime, DateTimeFormat};

    DateTime::from_str(written, DateTimeFormat::DateTime).ok()
}

/// The datums a `--metric-data` document carries, read as the member names `MetricDatum` spells.
///
/// The document is JSON, which is the form the CLI takes a list of structures in ("JSON syntax",
/// the `aws cloudwatch put-metric-data` command reference); one object alone is read as the one
/// datum it is. A document this program cannot read, or one carrying a member outside
/// [`DATUM_MEMBERS`], is not an invocation it performs.
fn datums_of(document: &str) -> Option<Vec<Datum>> {
    let read: ::serde_json::Value = ::serde_json::from_str(document).ok()?;
    let held = match read {
        ::serde_json::Value::Array(held) => held,
        object @ ::serde_json::Value::Object(_) => vec![object],
        _ => return None,
    };
    held.iter().map(datum_of).collect()
}

/// ONE datum of that document.
fn datum_of(held: &::serde_json::Value) -> Option<Datum> {
    let object = held.as_object()?;
    if !object
        .keys()
        .all(|member| DATUM_MEMBERS.contains(&member.as_str()))
    {
        return None;
    }
    let dimensions = match object.get("Dimensions") {
        None => Vec::new(),
        Some(held) => held
            .as_array()?
            .iter()
            .map(|dimension| {
                let dimension = dimension.as_object()?;
                if !dimension.keys().all(|member| member == "Name" || member == "Value") {
                    return None;
                }
                Some((
                    dimension.get("Name")?.as_str()?.to_string(),
                    dimension.get("Value")?.as_str()?.to_string(),
                ))
            })
            .collect::<Option<Vec<(String, String)>>>()?,
    };
    let statistic_values = match object.get("StatisticValues") {
        None => None,
        Some(held) => {
            let held = held.as_object()?;
            if !held
                .keys()
                .all(|member| STATISTIC_MEMBERS.contains(&member.as_str()))
            {
                return None;
            }
            let of = |member: &str| held.get(member).and_then(::serde_json::Value::as_f64);
            Some(StatisticSet {
                sample_count: of("SampleCount")?,
                sum: of("Sum")?,
                minimum: of("Minimum")?,
                maximum: of("Maximum")?,
            })
        }
    };
    Some(Datum {
        metric_name: object.get("MetricName")?.as_str()?.to_string(),
        dimensions,
        timestamp: match object.get("Timestamp") {
            None => None,
            Some(held) => Some(stamped(held.as_str()?)?),
        },
        value: match object.get("Value") {
            None => None,
            Some(held) => Some(held.as_f64()?),
        },
        statistic_values,
        unit: match object.get("Unit") {
            None => None,
            Some(held) => Some(held.as_str()?.to_string()),
        },
        storage_resolution: match object.get("StorageResolution") {
            None => None,
            Some(held) => Some(i32::try_from(held.as_i64()?).ok()?),
        },
    })
}

/// Publish the observations `call` names through `client`, and answer what the CLI prints for the
/// call — nothing, for an operation that answers no document of its own.
///
/// `client` is the in-process hop described in [the module docs](self): its transport is the
/// emulator's own callback, so the `send()` below dispatches to the emulated CloudWatch service
/// inside this process and reaches nothing off it.
///
/// The Region the line named ([`PutMetricDataCall::region`]) is the CALLER's to apply: it
/// addresses the client it hands over — which is to say which emulated endpoint of this process
/// that client is built for — exactly as the sibling groups' calls do.
///
/// `Err` carries what the CLI reports about a call the service refused — its own error code and
/// message, in the CLI's words for one ([`crate::aws_cli_options::service_answered`]) — and about
/// a datum this emulator could not build the request member for.
pub async fn published_metric_data(
    client: &::aws_sdk_cloudwatch::Client,
    call: &PutMetricDataCall,
) -> Result<String, String> {
    use ::aws_sdk_cloudwatch::types::{Dimension, MetricDatum, StandardUnit, StatisticSet};

    let mut request = client.put_metric_data().namespace(&call.namespace);
    for datum in &call.data {
        let mut built = MetricDatum::builder().metric_name(&datum.metric_name);
        for (name, value) in &datum.dimensions {
            built = built.dimensions(Dimension::builder().name(name).value(value).build());
        }
        if let Some(stamped) = datum.timestamp {
            built = built.timestamp(stamped);
        }
        if let Some(value) = datum.value {
            built = built.value(value);
        }
        if let Some(statistics) = &datum.statistic_values {
            built = built.statistic_values(
                StatisticSet::builder()
                    .sample_count(statistics.sample_count)
                    .sum(statistics.sum)
                    .minimum(statistics.minimum)
                    .maximum(statistics.maximum)
                    .build(),
            );
        }
        if let Some(unit) = &datum.unit {
            built = built.unit(StandardUnit::from(unit.as_str()));
        }
        if let Some(resolution) = datum.storage_resolution {
            built = built.storage_resolution(resolution);
        }
        request = request.metric_data(built.build());
    }
    request
        .send()
        .await
        .map_err(|error| {
            crate::aws_cli_options::service_answered(PUT_METRIC_DATA_OPERATION, &error)
        })
        .map(|_| String::new())
}

#[cfg(test)]
mod tests {
    use super::{requested_put_metric_data, Datum, PutMetricDataCall, StatisticSet};
    use crate::aws_cli_options::OutputFormat;

    /// The words of a command line after the program name, as a session hands them to a program.
    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(ToString::to_string).collect()
    }

    /// One call publishing `data` into `namespace` with no other option named.
    fn call(namespace: &str, data: Vec<Datum>) -> PutMetricDataCall {
        PutMetricDataCall {
            namespace: namespace.to_string(),
            data,
            region: None,
            output: OutputFormat::default(),
        }
    }

    /// One datum of `metric_name` carrying `value` and `dimensions`.
    fn datum(metric_name: &str, value: f64, dimensions: &[(&str, &str)]) -> Datum {
        Datum {
            metric_name: metric_name.to_string(),
            dimensions: dimensions
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            value: Some(value),
            ..Datum::default()
        }
    }

    /// RULE: `aws cloudwatch put-metric-data` over the parameters that command takes — its
    /// shorthand form and its datum document alike — is what this program performs of the
    /// CloudWatch group, and every other invocation is one it answers nothing for, so the session
    /// reports it as a command it could not run rather than as a call.
    ///
    /// The rows differ along what the rule generalises over: WHICH form names the observation
    /// (inline, a document, several datums in one document), how many dimensions it carries, which
    /// optional members it names, the two spellings of a value, and the forms that are not an
    /// invocation of this command at all.
    #[test]
    fn the_invocations_this_program_performs() {
        // (case, the arguments after `aws`, what it asks for)
        let rows: [(&str, &str, Option<PutMetricDataCall>); 14] = [
            (
                "the namespace, the metric and its value",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-name TraceWrites \
                 --value 1",
                Some(call("flint/allowed", vec![datum("TraceWrites", 1.0, &[])])),
            ),
            (
                "one dimension, in the pair form this command takes",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-name TraceWrites \
                 --value 1 --dimensions Result=ok",
                Some(call(
                    "flint/allowed",
                    vec![datum("TraceWrites", 1.0, &[("Result", "ok")])],
                )),
            ),
            (
                "two dimensions, separated by a comma",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-name TraceWrites \
                 --value 2 --dimensions Result=denied,Stage=prod",
                Some(call(
                    "flint/allowed",
                    vec![datum(
                        "TraceWrites",
                        2.0,
                        &[("Result", "denied"), ("Stage", "prod")],
                    )],
                )),
            ),
            (
                "values written with equals signs",
                "cloudwatch put-metric-data --namespace=flint/allowed --metric-name=Latency \
                 --value=15000.5 --unit=Milliseconds",
                Some(call(
                    "flint/allowed",
                    vec![Datum {
                        unit: Some("Milliseconds".to_string()),
                        ..datum("Latency", 15_000.5, &[])
                    }],
                )),
            ),
            (
                "a stamped observation at the high resolution",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-name Latency \
                 --value 3 --timestamp 2026-01-02T03:04:05Z --storage-resolution 1",
                Some(call(
                    "flint/allowed",
                    vec![Datum {
                        timestamp: super::stamped("2026-01-02T03:04:05Z"),
                        storage_resolution: Some(1),
                        ..datum("Latency", 3.0, &[])
                    }],
                )),
            ),
            (
                "an aggregated observation rather than a single value",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-name Latency \
                 --statistic-values SampleCount=3,Sum=12,Minimum=1,Maximum=8",
                Some(call(
                    "flint/allowed",
                    vec![Datum {
                        metric_name: "Latency".to_string(),
                        statistic_values: Some(StatisticSet {
                            sample_count: 3.0,
                            sum: 12.0,
                            minimum: 1.0,
                            maximum: 8.0,
                        }),
                        ..Datum::default()
                    }],
                )),
            ),
            (
                "the datum document form, with one datum",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-data \
                 [{\"MetricName\":\"TraceWrites\",\"Value\":1,\"Dimensions\":[{\"Name\":\"Result\",\"Value\":\"ok\"}]}]",
                Some(call(
                    "flint/allowed",
                    vec![datum("TraceWrites", 1.0, &[("Result", "ok")])],
                )),
            ),
            (
                "the datum document form, with two datums",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-data \
                 [{\"MetricName\":\"One\",\"Value\":1},{\"MetricName\":\"Two\",\"Value\":2,\"Unit\":\"Count\"}]",
                Some(call(
                    "flint/allowed",
                    vec![
                        datum("One", 1.0, &[]),
                        Datum {
                            unit: Some("Count".to_string()),
                            ..datum("Two", 2.0, &[])
                        },
                    ],
                )),
            ),
            (
                "another Region and the tab-separated format",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-name TraceWrites \
                 --value 1 --region eu-west-1 --output text",
                Some(PutMetricDataCall {
                    region: Some("eu-west-1".to_string()),
                    output: OutputFormat::Text,
                    ..call("flint/allowed", vec![datum("TraceWrites", 1.0, &[])])
                }),
            ),
            (
                "another command of the group",
                "cloudwatch get-metric-statistics --namespace flint/allowed",
                None,
            ),
            (
                "a line naming no namespace",
                "cloudwatch put-metric-data --metric-name TraceWrites --value 1",
                None,
            ),
            (
                "a line naming neither a datum document nor a metric",
                "cloudwatch put-metric-data --namespace flint/allowed",
                None,
            ),
            (
                "a line naming both forms at once",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-name TraceWrites \
                 --metric-data [{\"MetricName\":\"One\",\"Value\":1}]",
                None,
            ),
            (
                "a datum carrying a member the call does not have",
                "cloudwatch put-metric-data --namespace flint/allowed --metric-data \
                 [{\"MetricName\":\"One\",\"Values\":[1]}]",
                None,
            ),
        ];
        for (case, line, expected) in rows {
            let asked = requested_put_metric_data(&words(line));
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }

    /// RULE: a dimension pair this form does not spell, a parameter the command does not take, a
    /// value that is not the type the member takes, a datum naming no observation at all, and an
    /// output format this program does not print are each a line no call is read out of.
    #[test]
    fn the_forms_that_name_no_call() {
        // (case, the arguments after `aws`)
        let rows: [(&str, &str); 8] = [
            (
                "a line naming neither a value nor a statistic set",
                "cloudwatch put-metric-data --namespace flint --metric-name One --unit Count",
            ),
            (
                "a datum of the document form naming neither either",
                "cloudwatch put-metric-data --namespace flint --metric-data \
                 [{\"MetricName\":\"One\"}]",
            ),
            (
                "a timestamp that is not an instant",
                "cloudwatch put-metric-data --namespace flint --metric-name One --value 1 \
                 --timestamp yesterday",
            ),
            (
                "a dimension with no value in it",
                "cloudwatch put-metric-data --namespace flint --metric-name One --value 1 \
                 --dimensions Result",
            ),
            (
                "a statistic set missing one of its four members",
                "cloudwatch put-metric-data --namespace flint --metric-name One \
                 --statistic-values SampleCount=3,Sum=12,Minimum=1",
            ),
            (
                "a parameter this command does not take",
                "cloudwatch put-metric-data --namespace flint --metric-name One --value 1 \
                 --entity-metric-data file:///tmp/entity.json",
            ),
            (
                "an output format this program does not print",
                "cloudwatch put-metric-data --namespace flint --metric-name One --value 1 \
                 --output yaml",
            ),
            (
                "a value that is not a number",
                "cloudwatch put-metric-data --namespace flint --metric-name One --value high",
            ),
        ];
        for (case, line) in rows {
            let asked = requested_put_metric_data(&words(line));
            assert!(
                asked.is_none(),
                "case {case:?}: expected no call, got {asked:?}"
            );
        }
    }
}

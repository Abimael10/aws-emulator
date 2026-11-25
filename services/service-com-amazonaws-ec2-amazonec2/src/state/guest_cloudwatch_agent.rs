// SPDX-License-Identifier: CC0-1.0

//! The CLOUDWATCH AGENT in one instance's guest: the package it is installed as, the control
//! program that configures and starts it, the StatsD listener it opens and the series it
//! publishes.
//!
//! # Why a guest needs it
//!
//! Memory, filesystem and application measurements have no API that can derive them: "the
//! CloudWatch agent … collect[s] system-level metrics from Amazon EC2 instances … including
//! system-level metrics that aren't available with standard CloudWatch, such as
//! `mem_used_percent`", and the agent's StatsD listener is how an application inside a machine
//! publishes a counter of its own. The documented procedure is one Systems Manager Run Command
//! invocation: install the package, write a configuration naming the measurements and the
//! StatsD input, hand that configuration to `amazon-cloudwatch-agent-ctl` and start the agent.
//! A guest that holds no such package cannot be monitored that way at all — the install
//! reports success and nothing is ever published, so every alarm over an agent series stays
//! without data for ever.
//!
//! # What is real here
//!
//! * the PACKAGE ([`AGENT_PACKAGE`]) is one of the packages this guest can install
//!   ([`super::guest_agents::GUEST_AGENT_PACKAGES`]), with the build the guest's package
//!   database then reports for it ([`installed_agent_package`], which is what `rpm -q` answers
//!   out of) and the files it ships on the machine's storage ([`AGENT_SHIPPED_FILES`]);
//! * the CONFIGURATION is the caller's own JSON document, READ off the machine's storage where
//!   the invocation left it and parsed ([`AgentConfig::parse`]): the namespace, the dimensions
//!   and the measurements the agent publishes are the ones that document declares, and a
//!   measurement it does not declare is one nothing publishes;
//! * the LISTENER is the StatsD input that configuration declares, open while the agent is
//!   running: a datagram the machine's own programs send to it is received
//!   ([`AgentRuntime::received_datagram`]) and one sent while the agent is stopped is dropped,
//!   and what the kernel publishes about the machine's UDP sockets reports it
//!   ([`udp_socket_table`]);
//! * the SERIES are computed from the machine ([`GuestMeasurements`]) and from the datagrams
//!   the listener received, appended to the CloudWatch metric store by the publisher that owns
//!   this store's telemetry ([`super::instance_telemetry`]) — the same store `ListMetrics`,
//!   `GetMetricStatistics` and the alarm evaluation read, so the agent and its readers cannot
//!   drift.
//!
//! Nothing is published for a measurement this emulator takes none of, nothing is published
//! while the agent is not running, and nothing here starts a process, opens a socket or reaches
//! a host path.

use ::std::collections::BTreeMap;
use ::std::time::{Duration, SystemTime};

use ::service_common::guest_package_query::{
    rendered_package, rpm_architecture, PackageQueryInvocation, QueriedPackage, QUERY_PROGRAM,
};
use ::service_common::posix_shell_session::{
    program_option_value, EnvironmentPrograms, ProgramFiles, ProgramOutcome, ProgramStreams,
};

use super::guest_memory::{memory_measurement, GuestMemory, MEMORY_MEASUREMENTS};

/// The name the agent is installed under, which is also the name of the init-system unit it
/// ships ("amazon-cloudwatch-agent", the package name on the Amazon Linux distributions).
pub const AGENT_PACKAGE: &str = "amazon-cloudwatch-agent";

/// The build of the agent a guest of this emulator installs, as the agent's own version
/// numbering states one (`<major>.<minor>.<patch>`), and the package build of it.
///
/// One release, so what the guest's package database reports and what the agent's own control
/// program reports as its version are one fact.
pub const AGENT_VERSION: &str = "1.300057.1";

/// See [`AGENT_VERSION`]: the agent publishes one package per release, so the installed package
/// is the first build of its own version.
pub const AGENT_PACKAGE_RELEASE: &str = "1";

/// The directory the agent's package installs under.
pub const AGENT_DIRECTORY: &str = "/opt/aws/amazon-cloudwatch-agent";

/// The control program that package ships, which is what the documented procedure configures
/// and starts the agent with.
pub const AGENT_CONTROL_PROGRAM: &str = "amazon-cloudwatch-agent-ctl";

/// Where that program sits on the machine, which is the path the procedure invokes it by.
pub const AGENT_CONTROL_PATH: &str = "/opt/aws/amazon-cloudwatch-agent/bin/amazon-cloudwatch-agent-ctl";

/// The agent program itself, beside its control program.
pub const AGENT_BINARY_PATH: &str = "/opt/aws/amazon-cloudwatch-agent/bin/amazon-cloudwatch-agent";

/// Where the control program keeps the configuration it was handed: "the agent configuration
/// file" the agent runs with, which `fetch-config` copies the named document to and which a
/// start of the agent runs with.
pub const AGENT_APPLIED_CONFIG_PATH: &str =
    "/opt/aws/amazon-cloudwatch-agent/etc/amazon-cloudwatch-agent.json";

/// The other path a configuration of this agent is held at: the file the agent's own
/// configuration wizard writes, which earlier releases keep the configuration in.
///
/// A start reads the first of these the machine holds ([`held_configuration`]), so a script that
/// put the document where either release keeps it starts a configured agent.
pub const AGENT_WIZARD_CONFIG_PATH: &str = "/opt/aws/amazon-cloudwatch-agent/bin/config.json";

/// What the control program writes when it took a configuration: the line its own
/// `fetch-config` writes, naming where the configuration it fetched was saved.
const FETCHED_LINE: &str = "Successfully fetched the config and saved in";

/// ... and the two lines it writes while it reads that configuration.
const VALIDATION_LINES: &[&str] = &[
    "Start configuration validation...",
    "Configuration validation first phase succeeded",
];

/// The files the agent's package leaves on the machine, as `(path, contents)`.
///
/// Each one is the ARTIFACT's identity rather than its program text: this emulator performs
/// what the control program DOES ([`controlled`]) instead of interpreting a copy of its source,
/// so what the machine holds at these paths is a statement that the package is installed —
/// which is what the procedure's own `[ -x … ]` test asks.
pub const AGENT_SHIPPED_FILES: &[(&str, &str)] = &[
    (
        AGENT_CONTROL_PATH,
        "#!/bin/sh\n# amazon-cloudwatch-agent-ctl (amazon-cloudwatch-agent)\n",
    ),
    (
        AGENT_BINARY_PATH,
        "#!/bin/sh\n# amazon-cloudwatch-agent (amazon-cloudwatch-agent)\n",
    ),
];

/// The DIRECTORIES the agent's package leaves on the machine, which are the ones its own
/// layout has: where its programs sit, where its configuration goes (and the directory
/// `fetch-config` keeps each fetched document in), where it writes its logs and where it keeps
/// its state.
///
/// Installing the package makes them, because that is what installing it does — and a script
/// that then writes the agent's configuration under `etc` is writing into a directory the
/// machine HAS, which is what makes the documented procedure's own
/// `cat > /opt/aws/amazon-cloudwatch-agent/etc/amazon-cloudwatch-agent.json` a write that
/// happens.
pub const AGENT_DIRECTORIES: &[&str] = &[
    "/opt/aws/amazon-cloudwatch-agent/bin",
    "/opt/aws/amazon-cloudwatch-agent/etc",
    "/opt/aws/amazon-cloudwatch-agent/etc/amazon-cloudwatch-agent.d",
    "/opt/aws/amazon-cloudwatch-agent/logs",
    "/opt/aws/amazon-cloudwatch-agent/var",
];

/// The namespace the agent publishes its series under where its configuration names none:
/// "CWAgent" (the CloudWatch agent configuration reference's `namespace` default).
pub const DEFAULT_NAMESPACE: &str = "CWAgent";

/// How often the agent takes a measurement where its configuration states no interval: "60"
/// seconds (the same reference's `metrics_collection_interval` default).
pub const DEFAULT_COLLECTION_INTERVAL_SECS: u64 = 60;

/// The port the StatsD listener opens on where the configuration's `service_address` names
/// none: 8125, the StatsD default the same reference states.
pub const DEFAULT_STATSD_PORT: u16 = 8125;

/// The address the listener is opened on where `service_address` names only a port, which is
/// every address of the machine.
pub const ALL_ADDRESSES: &str = "0.0.0.0";

/// How many measurements ONE configuration may declare, and how many dimensions it may append.
///
/// The configuration is a caller's document, so what one of them can make this process hold and
/// publish is bounded here. The dimension bound is CloudWatch's own ("you can assign up to 30
/// dimensions to a metric", `MetricDatum$Dimensions`); the measurement bound is the emulator's,
/// far above the handful of measurements an agent configuration declares.
const MAX_MEASUREMENTS: usize = 64;

/// See [`MAX_MEASUREMENTS`].
const MAX_DIMENSIONS: usize = 30;

/// How many datagrams the listener holds between publishes, and how long one of them may be.
///
/// Both are the emulator's numbers: the datagrams are a caller's program's, so what one guest
/// can make this process hold is bounded here. A datagram past the length bound is not
/// received, and one arriving at a full listener is dropped — which is what a listener whose
/// buffer is full does with a datagram.
const MAX_HELD_DATAGRAMS: usize = 256;

/// See [`MAX_HELD_DATAGRAMS`].
const MAX_DATAGRAM_BYTES: usize = 8 * 1024;

/// How many metrics ONE datagram yields datapoints for.
///
/// The emulator's own number, far above the handful of metrics a StatsD client puts in one
/// datagram: the payload is a caller's program's text, so what one send can make this process
/// publish is bounded here as well as by its length.
const MAX_DATAGRAM_METRICS: usize = 64;

/// How many collections of one measurement ONE publish materialises.
///
/// A backstop, not a schedule: the agent's collections fall due at its own interval
/// ([`AgentConfig::collection_interval_secs`]), and this bounds the catch-up one call does for
/// a series nothing has read for a long time. What it does not reach is published by the next
/// one.
const MAX_COLLECTIONS_PER_PUBLISH: usize = 16;

/// The status the control program exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// The status it exits with when it could not: the configuration it was pointed at is not
/// there, or is not a configuration at all.
const FAILED: i32 = 1;

/// One package a guest's package database holds, as `rpm -q` reports it: the shared reading of
/// a query and of the answer ([`::service_common::guest_package_query`]), so the program this
/// machine answers a query with and the one the service that keeps a node's inventory installs
/// beside it cannot differ about the grammar or the rendering.
pub type InstalledGuestPackage = QueriedPackage;

/// The agent package this guest holds, or `None` when its programs never installed it.
#[must_use]
pub fn installed_agent_package(
    runtime: &super::guest_services::GuestRuntime,
    architecture: &str,
) -> Option<InstalledGuestPackage> {
    runtime
        .installed
        .contains(AGENT_PACKAGE)
        .then(|| QueriedPackage {
            name: AGENT_PACKAGE.to_string(),
            version: AGENT_VERSION.to_string(),
            release: AGENT_PACKAGE_RELEASE.to_string(),
            architecture: rpm_architecture(architecture),
        })
}

/// The program a guest queries its own package DATABASE with, answering out of what the machine
/// has installed at the instant the query runs.
///
/// It reads the services of the live session rather than a snapshot, because a script that
/// installs a package and then queries it is one script: `dnf install -y amazon-cloudwatch-agent`
/// followed by `rpm -q amazon-cloudwatch-agent` is the documented procedure's own check, and a
/// database read as it stood before the install would answer that the package is not there.
///
/// It answers only a query whose every named package this machine holds a build of
/// ([`PackageQueryInvocation::names_only`]): every other query — one about a package this store
/// publishes no build of, or about every installed package — is left to whoever holds the rest
/// of the database, which for a managed node is the service that keeps its inventory.
#[must_use]
pub fn package_query_programs(
    services: &super::guest_services::GuestServices,
    architecture: &str,
) -> Box<dyn EnvironmentPrograms> {
    Box::new(PackageDatabase {
        services: services.clone(),
        architecture: architecture.to_string(),
    })
}

/// The package database of one machine, as its own query program reads it.
///
/// Holds the session's services and the machine's architecture and nothing else — no store
/// guard, no engine and no host handle is reachable from a program.
#[derive(Debug)]
struct PackageDatabase {
    /// The services of this guest, which is what has the installed set.
    services: super::guest_services::GuestServices,
    /// The machine's architecture, which is what its packages were built for.
    architecture: String,
}

impl EnvironmentPrograms for PackageDatabase {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        _files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        // A program named by its absolute path is the same program, as a shell resolving a name
        // on `PATH` finds.
        if name.rsplit('/').next().unwrap_or(name) != QUERY_PROGRAM {
            return None;
        }
        let invocation = PackageQueryInvocation::read(arguments)?;
        let held: Vec<QueriedPackage> =
            installed_agent_package(&self.services.taken(), &self.architecture)
                .into_iter()
                .collect();
        if !invocation.names_only(&held) {
            return None;
        }
        Some(ProgramOutcome {
            status: SUCCESS,
            output: held
                .iter()
                .flat_map(|package| rendered_package(package, invocation.format()))
                .collect(),
        })
    }
}

/// ONE datagram the agent's StatsD listener received: what it carried and when it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedDatagram {
    /// The payload, as the sender wrote it.
    pub payload: String,
    /// The instant it arrived, as epoch seconds — which is the time stamp the datapoints it
    /// yields carry.
    pub at_secs: i64,
}

/// The agent as ONE guest holds it: the configuration it is running with, whether it is
/// running, the datagrams its listener has received and the collections it has already
/// published.
///
/// Kept on the instance's guest runtime ([`super::guest_services::GuestRuntime`]), so what one
/// invocation configured and started is what the next command sent to that guest finds, and
/// what the publisher reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentRuntime {
    /// The configuration the agent is running with, for a guest that has handed it one.
    pub config: Option<AgentConfig>,
    /// When the agent was started, for a guest whose agent is running.
    pub started_at: Option<SystemTime>,
    /// The datagrams the listener has received and nothing has published yet.
    pub received: Vec<ReceivedDatagram>,
    /// The last collection of each measurement that was published, as the epoch seconds it was
    /// taken at — which is what makes the next one fall due an interval later.
    pub collected: BTreeMap<String, i64>,
}

impl AgentRuntime {
    /// Whether the agent is running in this guest.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.started_at.is_some()
    }

    /// The address and port the StatsD listener is open on, for a running agent whose
    /// configuration declares a StatsD input — and `None` for every other guest, which has no
    /// listener at all.
    #[must_use]
    pub fn statsd_listener(&self) -> Option<(&str, u16)> {
        let statsd = self.config.as_ref()?.statsd.as_ref()?;
        self.is_running()
            .then_some((statsd.address.as_str(), statsd.port))
    }

    /// Take the datagram `payload` addressed to `port`, answering whether the listener
    /// RECEIVED it.
    ///
    /// `false` is a datagram nothing received: the agent is not running, its configuration
    /// declares no StatsD input, the listener is open on another port, or the datagram is past
    /// what one listener holds ([`MAX_HELD_DATAGRAMS`], [`MAX_DATAGRAM_BYTES`]). A send to a
    /// port nothing listens on still happened — that is the sender's answer, not this one.
    pub fn received_datagram(&mut self, port: u16, payload: &str, at: SystemTime) -> bool {
        if self.statsd_listener().is_none_or(|(_, open)| open != port) {
            return false;
        }
        if payload.len() > MAX_DATAGRAM_BYTES || self.received.len() >= MAX_HELD_DATAGRAMS {
            return false;
        }
        self.received.push(ReceivedDatagram {
            payload: payload.to_string(),
            at_secs: epoch_secs(at),
        });
        true
    }

    /// Stop this agent: a stopped agent listens on nothing and publishes nothing, and the
    /// datagrams nothing has published yet are dropped with the listener that held them.
    ///
    /// The configuration it was handed stays on the machine, which is what makes starting it
    /// again a start of the configured agent.
    pub fn stop(&mut self) {
        self.started_at = None;
        self.received.clear();
    }
}

/// One measurement an agent configuration declares: the series it is published as and how
/// often it is taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredMeasurement {
    /// The `MetricName` the series is published under, which is the measurement's own name.
    pub name: String,
    /// How often the agent takes it.
    pub interval_secs: u64,
}

/// The StatsD input one configuration declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsdInput {
    /// The address the listener is opened on.
    pub address: String,
    /// The port it listens on.
    pub port: u16,
}

/// One agent configuration, as the document the control program was handed declares it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentConfig {
    /// "The namespace to use for the metrics collected by the agent"
    /// (`metrics.namespace`), and [`DEFAULT_NAMESPACE`] where the document names none.
    pub namespace: String,
    /// The dimensions every series of this agent carries
    /// (`metrics.append_dimensions`), with the values the document's own
    /// `${aws:…}` references resolved against the machine.
    pub dimensions: BTreeMap<String, String>,
    /// The measurements it declares, in the document's own order.
    pub measurements: Vec<ConfiguredMeasurement>,
    /// The StatsD input it declares, for a document that declares one.
    pub statsd: Option<StatsdInput>,
}

/// The facts of the machine an agent configuration's `${aws:…}` references name.
///
/// "The `append_dimensions` field … `${aws:InstanceId}`, `${aws:ImageId}`,
/// `${aws:InstanceType}`" are resolved by the agent from the instance it runs on, which is why
/// they are the machine's own facts here; a reference this machine has no value for appends no
/// dimension, rather than one carrying the reference as written.
#[derive(Debug, Clone, Default)]
pub struct AgentHost {
    /// The instance the guest is of.
    pub instance_id: String,
    /// The image it was launched from, for an instance whose record carries one.
    pub image_id: Option<String>,
    /// Its instance type.
    pub instance_type: String,
}

/// The reference an agent configuration names each of those facts by.
const INSTANCE_ID_REFERENCE: &str = "${aws:InstanceId}";

/// See [`INSTANCE_ID_REFERENCE`].
const IMAGE_ID_REFERENCE: &str = "${aws:ImageId}";

/// See [`INSTANCE_ID_REFERENCE`].
const INSTANCE_TYPE_REFERENCE: &str = "${aws:InstanceType}";

impl AgentHost {
    /// `value` with the `${aws:…}` references it carries resolved, or `None` when it names one
    /// this machine has no value for.
    #[must_use]
    fn resolved(&self, value: &str) -> Option<String> {
        let mut resolved = value.to_string();
        for (reference, held) in [
            (INSTANCE_ID_REFERENCE, Some(self.instance_id.clone())),
            (IMAGE_ID_REFERENCE, self.image_id.clone()),
            (INSTANCE_TYPE_REFERENCE, Some(self.instance_type.clone())),
        ] {
            if !resolved.contains(reference) {
                continue;
            }
            let held = held.filter(|value| !value.is_empty())?;
            resolved = resolved.replace(reference, &held);
        }
        // A reference to anything else is one this machine resolves nothing for.
        (!resolved.contains("${")).then_some(resolved)
    }
}

/// The section of `metrics_collected` that declares the StatsD input rather than a measurement
/// the agent takes of the machine.
const STATSD_SECTION: &str = "statsd";

impl AgentConfig {
    /// The configuration the document `text` declares, resolved against `host` — or `None`
    /// when `text` is not a configuration document at all.
    ///
    /// Everything the agent publishes comes from the document: the namespace, the dimensions it
    /// appends and the measurements each section declares, each at the interval that section
    /// states or the agent's own ("`metrics_collection_interval` … can be overridden for
    /// individual metric types"). A measurement named without its section's prefix is the
    /// measurement of that section it names (`"mem": {"measurement": ["used_percent"]}` is
    /// `mem_used_percent`), which is how the configuration reference writes them.
    #[must_use]
    pub fn parse(text: &str, host: &AgentHost) -> Option<Self> {
        let document: ::serde_json::Value = ::serde_json::from_str(text).ok()?;
        let document = document.as_object()?;
        let agent_interval = document
            .get("agent")
            .and_then(::serde_json::Value::as_object)
            .and_then(|agent| agent.get("metrics_collection_interval"))
            .and_then(::serde_json::Value::as_u64);
        let metrics = document
            .get("metrics")
            .and_then(::serde_json::Value::as_object);
        let mut config = Self {
            namespace: metrics
                .and_then(|metrics| metrics.get("namespace"))
                .and_then(::serde_json::Value::as_str)
                .filter(|namespace| !namespace.is_empty())
                .unwrap_or(DEFAULT_NAMESPACE)
                .to_string(),
            ..Self::default()
        };
        if let Some(appended) = metrics
            .and_then(|metrics| metrics.get("append_dimensions"))
            .and_then(::serde_json::Value::as_object)
        {
            for (name, value) in appended.iter().take(MAX_DIMENSIONS) {
                let Some(value) = value.as_str().and_then(|value| host.resolved(value)) else {
                    continue;
                };
                config.dimensions.insert(name.clone(), value);
            }
        }
        let collected = metrics
            .and_then(|metrics| metrics.get("metrics_collected"))
            .and_then(::serde_json::Value::as_object);
        for (section, declared) in collected.into_iter().flatten() {
            let declared = declared.as_object();
            let interval = declared
                .and_then(|declared| declared.get("metrics_collection_interval"))
                .and_then(::serde_json::Value::as_u64)
                .or(agent_interval)
                .unwrap_or(DEFAULT_COLLECTION_INTERVAL_SECS)
                .max(1);
            if section == STATSD_SECTION {
                let address = declared
                    .and_then(|declared| declared.get("service_address"))
                    .and_then(::serde_json::Value::as_str)
                    .unwrap_or_default();
                config.statsd = Some(statsd_input(address));
                continue;
            }
            let measurements = declared
                .and_then(|declared| declared.get("measurement"))
                .and_then(::serde_json::Value::as_array);
            for measurement in measurements.into_iter().flatten() {
                if config.measurements.len() >= MAX_MEASUREMENTS {
                    break;
                }
                // A measurement is named as a string, or as an object naming it beside the
                // unit it is published in ("you can also specify … `name`").
                let Some(name) = measurement.as_str().or_else(|| {
                    measurement
                        .as_object()
                        .and_then(|named| named.get("name"))
                        .and_then(::serde_json::Value::as_str)
                }) else {
                    continue;
                };
                let name = qualified_measurement(section, name);
                if config
                    .measurements
                    .iter()
                    .any(|held| held.name == name)
                {
                    continue;
                }
                config.measurements.push(ConfiguredMeasurement {
                    name,
                    interval_secs: interval,
                });
            }
        }
        Some(config)
    }

    /// Whether this configuration declares the measurement `name` names.
    #[must_use]
    pub fn declares(&self, name: &str) -> bool {
        self.measurements
            .iter()
            .any(|measurement| measurement.name == name)
    }
}

/// The StatsD input `service_address` declares: the address and port it names (`":8125"`,
/// `"127.0.0.1:8125"`), with the defaults for the halves it leaves out.
fn statsd_input(service_address: &str) -> StatsdInput {
    let (address, port) = match service_address.rsplit_once(':') {
        Some((address, port)) => (address, port.parse().ok()),
        None => (service_address, None),
    };
    let address = address.trim();
    StatsdInput {
        address: if address.is_empty() {
            ALL_ADDRESSES.to_string()
        } else {
            address.to_string()
        },
        port: port.unwrap_or(DEFAULT_STATSD_PORT),
    }
}

/// The name the measurement `name` of `section` is published under: its own name where it
/// already carries the section's prefix, and the prefixed name otherwise.
fn qualified_measurement(section: &str, name: &str) -> String {
    let name = name.trim();
    let prefix = format!("{section}_");
    if name.starts_with(&prefix) {
        return name.to_string();
    }
    format!("{prefix}{name}")
}

/// What ONE invocation of the agent's control program asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlInvocation {
    /// The action `-a` names.
    pub action: ControlAction,
    /// The configuration `-c` names, for an invocation that named one.
    pub config: Option<ConfigSource>,
    /// Whether it asked for the agent to be started as well (`-s`).
    pub start: bool,
}

/// The action an invocation of the control program names, of the ones this guest performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    /// `fetch-config`: take the configuration `-c` names and run with it.
    FetchConfig,
    /// `start`: start the agent with the configuration it already has.
    Start,
    /// `stop`: stop it.
    Stop,
    /// `status`: report whether it is running and what it is configured with.
    Status,
}

/// Where the configuration an invocation names comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// `file:<path>`: a document on the machine's own storage.
    File(String),
    /// `default`: the configuration the agent already holds.
    Default,
}

/// The option the invocation names its action with, and the ones naming its mode, its
/// configuration and whether to start the agent.
const ACTION_OPTIONS: &[&str] = &["-a", "--action"];

/// See [`ACTION_OPTIONS`].
const MODE_OPTIONS: &[&str] = &["-m", "--mode"];

/// See [`ACTION_OPTIONS`].
const CONFIG_OPTIONS: &[&str] = &["-c", "--config"];

/// See [`ACTION_OPTIONS`].
const START_OPTION: &str = "-s";

impl ControlInvocation {
    /// The invocation `arguments` state, or `None` for one written in a form this program does
    /// not perform — an action it has none of, or an option carrying no value.
    #[must_use]
    pub fn read(arguments: &[String]) -> Option<Self> {
        let mut action: Option<ControlAction> = None;
        let mut config: Option<ConfigSource> = None;
        let mut start = false;
        let mut words = arguments.iter();
        while let Some(word) = words.next() {
            if word == START_OPTION {
                start = true;
                continue;
            }
            if let Some(value) = program_option_value(word, &mut words, ACTION_OPTIONS) {
                action = Some(match value?.as_str() {
                    "fetch-config" | "append-config" => ControlAction::FetchConfig,
                    "start" | "cond-restart" => ControlAction::Start,
                    "stop" | "prep-restart" => ControlAction::Stop,
                    "status" => ControlAction::Status,
                    // Every other action is one this guest's agent performs nothing of.
                    _ => return None,
                });
                continue;
            }
            if let Some(value) = program_option_value(word, &mut words, CONFIG_OPTIONS) {
                let value = value?;
                config = Some(match value.strip_prefix("file:") {
                    Some(path) => ConfigSource::File(path.to_string()),
                    None if value == "default" => ConfigSource::Default,
                    // A configuration held anywhere else — a parameter of another service —
                    // is not one this guest resolves.
                    None => return None,
                });
                continue;
            }
            // The MODE an invocation runs in (`-m ec2`) states where the agent is running,
            // which this machine already is; every other option changes how much the program
            // says about itself.
            let _ = program_option_value(word, &mut words, MODE_OPTIONS);
        }
        Some(Self {
            action: action?,
            config,
            start,
        })
    }
}

/// Perform `invocation` against the agent of this guest, answering what the control program
/// wrote and the status it exited with.
///
/// `installed` is whether the machine has the package at all: a control program of an agent
/// nothing installed is not a program of this guest, which is why that case is the caller's to
/// answer. Starting the agent needs a configuration — "you must … specify the configuration
/// file" before the agent runs — so a start with none is the failure the program reports, and a
/// `fetch-config` reads the document off the machine's own storage and refuses what it cannot
/// read or cannot parse.
pub fn controlled(
    invocation: &ControlInvocation,
    agent: &mut AgentRuntime,
    host: &AgentHost,
    files: &mut dyn ProgramFiles,
    now: SystemTime,
) -> ProgramStreams {
    let failed = |line: String| ProgramStreams {
        status: FAILED,
        output: Vec::new(),
        errors: vec![line],
    };
    let done = |output: Vec<String>| ProgramStreams {
        status: SUCCESS,
        output,
        errors: Vec::new(),
    };
    match invocation.action {
        ControlAction::Status => done(status_document(agent)),
        ControlAction::Stop => {
            agent.stop();
            publish_socket_table(agent, files);
            done(Vec::new())
        }
        ControlAction::FetchConfig | ControlAction::Start => {
            let mut written: Vec<String> = Vec::new();
            let starting = invocation.start || matches!(invocation.action, ControlAction::Start);
            if let Some(ConfigSource::File(path)) = &invocation.config {
                let Some(text) = files.read(path) else {
                    return failed(format!(
                        "{AGENT_CONTROL_PROGRAM}: no configuration at {path}"
                    ));
                };
                let Some(config) = AgentConfig::parse(&text, host) else {
                    return failed(format!(
                        "{AGENT_CONTROL_PROGRAM}: {path} is not a valid agent configuration"
                    ));
                };
                // The configuration the agent runs with is the one it was handed, kept where
                // the agent keeps its own — so a later start of it runs with that document.
                files.write(AGENT_APPLIED_CONFIG_PATH, &text);
                written.push(format!("{FETCHED_LINE} {AGENT_APPLIED_CONFIG_PATH}"));
                written.extend(VALIDATION_LINES.iter().map(|line| (*line).to_string()));
                if starting {
                    // The document this invocation also STARTS the agent with is the one it
                    // runs with from now on, whichever one it was running with before.
                    agent.config = Some(config);
                }
            }
            if starting && !started(agent, host, files, now) {
                return failed(format!(
                    "{AGENT_CONTROL_PROGRAM}: the agent has no configuration to run with"
                ));
            }
            done(written)
        }
    }
}

/// Start the agent of this guest with the configuration the MACHINE holds, answering whether it
/// came up.
///
/// The configuration an agent runs with is a FILE on the machine ([`held_configuration`]): the
/// control program's own `fetch-config` is what puts it there, so a start reads it back from
/// there — which is what makes a start after a fetch, a start of the unit the init system
/// brings up, and a start of a machine whose document a script wrote where the agent keeps it
/// one and the same start. An agent with no configuration anywhere does not come up: "you must
/// … specify the configuration file" before the agent runs.
///
/// Starting an agent that is already running changes nothing about when it started, which is
/// what makes a second start of a running agent no restart of its collection.
pub fn started(
    agent: &mut AgentRuntime,
    host: &AgentHost,
    files: &mut dyn ProgramFiles,
    now: SystemTime,
) -> bool {
    if agent.config.is_none() {
        agent.config = held_configuration(files, host);
    }
    if agent.config.is_none() {
        return false;
    }
    if !agent.is_running() {
        agent.started_at = Some(now);
    }
    publish_socket_table(agent, files);
    true
}

/// The configuration the machine HOLDS for this agent: the document at the path the control
/// program saves one to, else the one its configuration wizard writes — and `None` when the
/// machine holds no configuration document at all, or holds nothing that is one.
fn held_configuration(files: &dyn ProgramFiles, host: &AgentHost) -> Option<AgentConfig> {
    [AGENT_APPLIED_CONFIG_PATH, AGENT_WIZARD_CONFIG_PATH]
        .into_iter()
        .filter_map(|path| files.read(path))
        .find_map(|text| AgentConfig::parse(&text, host))
}

/// What `-a status` writes: the agent's own status document, as the one line the control
/// program writes it on.
///
/// The members are the ones the control program reports — whether it is running, when it
/// started, whether it has been configured and which build it is — each read off this guest's
/// agent, in the order it writes them.
fn status_document(agent: &AgentRuntime) -> Vec<String> {
    let members = [
        (
            "status",
            if agent.is_running() { "running" } else { "stopped" }.to_string(),
        ),
        (
            "starttime",
            agent
                .started_at
                .map(super::iso8601_utc)
                .unwrap_or_default(),
        ),
        (
            "configstatus",
            if agent.config.is_some() {
                "configured"
            } else {
                "not configured"
            }
            .to_string(),
        ),
        ("version", AGENT_VERSION.to_string()),
    ];
    // ONE line, in the order the control program writes those members and with no space around
    // the separators: what a script reads the status out of is the document itself
    // (`… | grep -o '"status":"[a-z]*"'`), so the form it is written in is the compact form that
    // reading expects. Every value is rendered as the JSON string it is, so one carrying a
    // character JSON escapes is still a document.
    let rendered: Vec<String> = members
        .into_iter()
        .map(|(member, value)| format!("\"{member}\":{}", ::serde_json::Value::from(value)))
        .collect();
    vec![format!("{{{}}}", rendered.join(","))]
}

/// The path the kernel publishes the machine's UDP sockets at, where a guest reads which port
/// something in it is listening on, and the IPv6 half of the same table.
pub const GUEST_NET_UDP_PATH: &str = "/proc/net/udp";

/// See [`GUEST_NET_UDP_PATH`].
pub const GUEST_NET_UDP6_PATH: &str = "/proc/net/udp6";

/// The header the kernel writes above that table.
const NET_UDP_HEADER: &str =
    "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops";

/// The socket state a listening datagram socket is in, as the table states one: `07`, the
/// `TCP_CLOSE` the kernel reports for every UDP socket.
const NET_UDP_STATE: &str = "07";

/// What the kernel publishes at [`GUEST_NET_UDP_PATH`] for `agent`: the header, and one row per
/// UDP socket something in this machine is listening on.
///
/// The row's fields are the kernel's own, so a guest's `grep ':1FBD' /proc/net/udp` finds the
/// listener it finds on a real instance: the local address as the four address bytes in network
/// order and the port, both in upper-case hexadecimal.
#[must_use]
pub fn udp_socket_table(agent: &AgentRuntime) -> String {
    let mut table = format!("{NET_UDP_HEADER}\n");
    if let Some((address, port)) = agent.statsd_listener() {
        table.push_str(&format!(
            "{:>5}: {}:{port:04X} 00000000:0000 {NET_UDP_STATE} 00000000:00000000 00:00000000 00000000     0        0 0 2 0000000000000000 0\n",
            0,
            hexadecimal_address(address),
        ));
    }
    table
}

/// The IPv6 half of that table, which carries the header alone: the listener a guest of this
/// emulator opens is an IPv4 socket.
#[must_use]
pub fn udp6_socket_table() -> String {
    format!("{NET_UDP_HEADER}\n")
}

/// The dotted-quad `address` as the table writes one: its four bytes, least significant first,
/// in upper-case hexadecimal — and the all-addresses row for anything that is not a
/// dotted quad.
fn hexadecimal_address(address: &str) -> String {
    let mut bytes = [0_u8; 4];
    let mut parts = address.split('.');
    for byte in &mut bytes {
        let Some(part) = parts.next().and_then(|part| part.parse::<u8>().ok()) else {
            return "00000000".to_string();
        };
        *byte = part;
    }
    if parts.next().is_some() {
        return "00000000".to_string();
    }
    format!(
        "{:02X}{:02X}{:02X}{:02X}",
        bytes[3], bytes[2], bytes[1], bytes[0]
    )
}

/// Write the machine's UDP socket tables as they stand now, which is what the kernel publishes
/// about them.
fn publish_socket_table(agent: &AgentRuntime, files: &mut dyn ProgramFiles) {
    publish_socket_tables(&udp_socket_table(agent), files);
}

/// Write `table` as what the kernel publishes about the machine's UDP sockets, beside the IPv6
/// half of the same table.
///
/// The ONE place those paths are written, so every program that starts or stops a listener of
/// this machine publishes one table rather than two readings of it.
pub fn publish_socket_tables(table: &str, files: &mut dyn ProgramFiles) {
    files.write(GUEST_NET_UDP_PATH, table);
    files.write(GUEST_NET_UDP6_PATH, &udp6_socket_table());
}

/// What the agent can MEASURE of one machine: the readings every `metrics_collected` section
/// this emulator derives comes out of.
///
/// A section this emulator takes no reading of has no entry here, and a measurement it holds no
/// reading for is not published at all — the agent publishes what it measured, and a number
/// nothing measured would be indistinguishable from one it did.
#[derive(Debug, Clone, Copy, Default)]
pub struct GuestMeasurements {
    /// The machine's memory ([`super::guest_memory`]), for a machine whose memory size this
    /// emulator holds.
    pub memory: Option<GuestMemory>,
    /// The machine's root filesystem, for a machine whose root volume this store holds a size
    /// for.
    pub filesystem: Option<GuestFilesystem>,
}

/// The root FILESYSTEM of one machine, which the `disk` measurements below are taken of
/// ([`super::guest_filesystem`], which holds it because the guest's own programs report the
/// same filesystem): re-exported here so a reader of the measurements has the reading they are
/// taken of to hand.
pub use super::guest_filesystem::GuestFilesystem;

/// The measurements of a FILESYSTEM the agent takes, as the names it publishes each under with
/// the unit of each (the CloudWatch agent configuration reference's `disk` section).
pub const FILESYSTEM_MEASUREMENTS: &[(&str, &str)] = &[
    ("disk_total", super::instance_telemetry::BYTES_UNIT),
    ("disk_used", super::instance_telemetry::BYTES_UNIT),
    ("disk_free", super::instance_telemetry::BYTES_UNIT),
    ("disk_used_percent", super::instance_telemetry::PERCENT_UNIT),
];

impl GuestMeasurements {
    /// What the measurement `name` reads on this machine, with the unit it is published in —
    /// and `None` for a measurement this emulator takes no reading of, which nothing publishes.
    #[must_use]
    pub fn reading(&self, name: &str) -> Option<(f64, &'static str)> {
        if MEMORY_MEASUREMENTS
            .iter()
            .any(|(measurement, _)| *measurement == name)
        {
            return memory_measurement(name, self.memory?);
        }
        let unit = FILESYSTEM_MEASUREMENTS
            .iter()
            .find(|(measurement, _)| *measurement == name)
            .map(|(_, unit)| *unit)?;
        let filesystem = self.filesystem?;
        #[allow(clippy::cast_precision_loss)]
        let value = match name {
            "disk_total" => filesystem.total_bytes as f64,
            "disk_used" => filesystem.used_bytes as f64,
            "disk_free" => filesystem.free_bytes() as f64,
            "disk_used_percent" => filesystem.used_percent(),
            _ => return None,
        };
        Some((value, unit))
    }
}

/// ONE datapoint the agent publishes: the series it belongs to, what was measured and when.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSample {
    /// The namespace the configuration declares.
    pub namespace: String,
    /// The `MetricName` the series is published under.
    pub metric_name: String,
    /// The dimensions the series carries.
    pub dimensions: BTreeMap<String, String>,
    /// The value measured.
    pub value: f64,
    /// The unit it is published in, for a measurement that has one.
    pub unit: Option<String>,
    /// The instant the measurement was taken, as epoch seconds — which is the time stamp the
    /// datapoint carries.
    pub at_secs: i64,
}

/// Every datapoint the agent of this guest owes at `now`, taking the datagrams it has received
/// and recording the collections it publishes as taken.
///
/// Two kinds of datapoint come out of a running agent, and both of them are things that already
/// happened:
///
/// * a COLLECTION of each configured measurement, which the agent takes when it starts and
///   every interval after that — so a collection whose instant has passed was owed at that
///   instant whether or not anybody called, and is published stamped with it;
/// * the STATSD datagrams its listener received, each parsed into the datapoints it carries
///   and stamped with the instant it arrived.
///
/// An agent that is not running owes nothing: nothing collects, and a datagram sent to a
/// listener that is not open was never received ([`AgentRuntime::received_datagram`]).
pub fn due_samples(
    agent: &mut AgentRuntime,
    measurements: &GuestMeasurements,
    now: SystemTime,
) -> Vec<AgentSample> {
    let Some(started_at) = agent.started_at else {
        return Vec::new();
    };
    let Some(config) = agent.config.clone() else {
        return Vec::new();
    };
    let now_secs = epoch_secs(now);
    let mut samples: Vec<AgentSample> = Vec::new();
    for measurement in &config.measurements {
        let Some((value, unit)) = measurements.reading(&measurement.name) else {
            continue;
        };
        let interval = i64::try_from(measurement.interval_secs.max(1)).unwrap_or(1);
        let started = epoch_secs(started_at);
        let mut taken = agent.collected.get(&measurement.name).copied();
        for _ in 0..MAX_COLLECTIONS_PER_PUBLISH {
            // The agent takes its first reading as it starts, and one every interval after.
            let due = taken.map_or(started, |taken| taken.saturating_add(interval));
            if due > now_secs {
                break;
            }
            samples.push(AgentSample {
                namespace: config.namespace.clone(),
                metric_name: measurement.name.clone(),
                dimensions: config.dimensions.clone(),
                value,
                unit: Some(unit.to_string()),
                at_secs: due,
            });
            taken = Some(due);
        }
        if let Some(taken) = taken {
            agent.collected.insert(measurement.name.clone(), taken);
        }
    }
    for datagram in std::mem::take(&mut agent.received) {
        for line in datagram.payload.lines().take(MAX_DATAGRAM_METRICS) {
            samples.extend(statsd_samples(line, &config, datagram.at_secs));
        }
    }
    samples
}

/// The datapoints one line of a StatsD datagram carries: `<name>:<value>|<type>` with the
/// optional sample rate and tags the protocol allows after it
/// (`name:1|c|@0.1|#tag:value,other:value`).
///
/// A line that is not a StatsD metric carries nothing, and neither does one whose value is not
/// a number — a listener drops what it cannot parse rather than publishing a datapoint for it.
/// The tags a line carries are dimensions of the series beside the ones the configuration
/// appends, which is how a StatsD client dimensions its own metric.
fn statsd_samples(line: &str, config: &AgentConfig, at_secs: i64) -> Vec<AgentSample> {
    let line = line.trim();
    let Some((name, rest)) = line.split_once(':') else {
        return Vec::new();
    };
    let name = name.trim();
    if name.is_empty() {
        return Vec::new();
    }
    let mut fields = rest.split('|');
    let Some(value) = fields
        .next()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite())
    else {
        return Vec::new();
    };
    let mut dimensions = config.dimensions.clone();
    for field in fields {
        let Some(tags) = field.trim().strip_prefix('#') else {
            continue;
        };
        for tag in tags.split(',') {
            if dimensions.len() >= MAX_DIMENSIONS {
                break;
            }
            let (tag, tag_value) = tag.split_once(':').unwrap_or((tag, ""));
            let tag = tag.trim();
            if tag.is_empty() {
                continue;
            }
            dimensions.insert(tag.to_string(), tag_value.trim().to_string());
        }
    }
    vec![AgentSample {
        namespace: config.namespace.clone(),
        metric_name: name.to_string(),
        dimensions,
        // A StatsD value carries no unit of its own, so the datapoint is published with none —
        // which is a unit like any other in the metric store.
        unit: None,
        value,
        at_secs,
    }]
}

/// `at` as the epoch seconds a datapoint is stamped in.
fn epoch_secs(at: SystemTime) -> i64 {
    i64::try_from(
        at.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs(),
    )
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree of its own for a case, so the control program can be asked what it reads and
    /// writes.
    #[derive(Debug, Default)]
    struct Tree(BTreeMap<String, String>);

    impl ProgramFiles for Tree {
        fn read(&self, path: &str) -> Option<String> {
            self.0.get(path).cloned()
        }

        fn write(&mut self, path: &str, contents: &str) -> bool {
            self.0.insert(path.to_string(), contents.to_string());
            true
        }

        fn remove(&mut self, path: &str) -> bool {
            self.0.remove(path).is_some()
        }
    }

    /// The machine the configurations below are resolved against.
    fn host() -> AgentHost {
        AgentHost {
            instance_id: "i-0123456789abcdef0".to_string(),
            image_id: Some("ami-00000000000000001".to_string()),
            instance_type: "t3.micro".to_string(),
        }
    }

    /// A configuration declaring `section` with `measurement`, a StatsD input and the
    /// instance-id dimension.
    fn configuration(interval: u64) -> String {
        format!(
            r#"{{
              "agent": {{"metrics_collection_interval": {interval}}},
              "metrics": {{
                "namespace": "Custom/Agent",
                "append_dimensions": {{"InstanceId": "{INSTANCE_ID_REFERENCE}"}},
                "metrics_collected": {{
                  "mem": {{"measurement": ["mem_used_percent", "used"]}},
                  "statsd": {{"service_address": ":8125"}}
                }}
              }}
            }}"#
        )
    }

    /// RULE: everything the agent publishes comes from the configuration it was handed — the
    /// namespace, the appended dimensions, the measurements and the StatsD input — and a
    /// document that is not a configuration configures nothing.
    #[test]
    fn the_configuration_is_read_off_the_document() {
        let config = AgentConfig::parse(&configuration(10), &host()).expect("a configuration");
        assert_eq!(
            config.namespace, "Custom/Agent",
            "expected the declared namespace, got {config:?}"
        );
        assert_eq!(
            config.dimensions.get("InstanceId").map(String::as_str),
            Some("i-0123456789abcdef0"),
            "expected the instance's own id appended, got {config:?}"
        );
        let names: Vec<&str> = config
            .measurements
            .iter()
            .map(|measurement| measurement.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["mem_used_percent", "mem_used"],
            "expected the declared measurements, qualified by their section, got {names:?}"
        );
        assert!(
            config
                .measurements
                .iter()
                .all(|measurement| measurement.interval_secs == 10),
            "expected the declared interval, got {config:?}"
        );
        assert_eq!(
            config.statsd,
            Some(StatsdInput {
                address: ALL_ADDRESSES.to_string(),
                port: DEFAULT_STATSD_PORT,
            }),
            "expected the declared StatsD input, got {config:?}"
        );
        // (the document, whether it is a configuration at all)
        let rows: &[(&str, bool)] = &[
            ("{}", true),
            (r#"{"metrics": {}}"#, true),
            ("not json at all", false),
            ("[1, 2, 3]", false),
            ("", false),
        ];
        for (document, parses) in rows {
            let parsed = AgentConfig::parse(document, &host()).is_some();
            assert!(
                parsed == *parses,
                "case {document:?}: expected parses={parses}, got {parsed}"
            );
        }
        // A document naming no namespace and no interval runs with the agent's own defaults.
        let bare = AgentConfig::parse(
            r#"{"metrics": {"metrics_collected": {"disk": {"measurement": ["used_percent"]}}}}"#,
            &host(),
        )
        .expect("a configuration");
        assert_eq!(
            (
                bare.namespace.as_str(),
                bare.measurements.first().map(|held| held.interval_secs)
            ),
            (DEFAULT_NAMESPACE, Some(DEFAULT_COLLECTION_INTERVAL_SECS)),
            "expected the agent's own defaults, got {bare:?}"
        );
        assert!(
            bare.declares("disk_used_percent"),
            "expected the section's prefix to qualify the measurement, got {bare:?}"
        );
    }

    /// RULE: a StatsD address names the port the listener opens on, whichever halves it
    /// states.
    #[test]
    fn the_listener_opens_where_the_address_says() {
        // (the declared service_address, the address, the port)
        let rows: &[(&str, &str, u16)] = &[
            (":8125", ALL_ADDRESSES, 8125),
            (":9000", ALL_ADDRESSES, 9000),
            ("127.0.0.1:8125", "127.0.0.1", 8125),
            ("udp4://127.0.0.1:8130", "udp4://127.0.0.1", 8130),
            ("", ALL_ADDRESSES, DEFAULT_STATSD_PORT),
            (":not-a-port", ALL_ADDRESSES, DEFAULT_STATSD_PORT),
        ];
        for (declared, address, port) in rows {
            let input = statsd_input(declared);
            assert_eq!(
                (input.address.as_str(), input.port),
                (*address, *port),
                "case {declared:?}: expected {address}:{port}, got {input:?}"
            );
        }
    }

    /// RULE: a datagram is received only while the listener is open on the port it was sent
    /// to.
    #[test]
    fn only_a_running_agents_listener_receives() {
        let mut agent = AgentRuntime::default();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert!(
            !agent.received_datagram(8125, "calls:1|c", now),
            "an agent nothing configured has no listener"
        );
        agent.config = Some(AgentConfig::parse(&configuration(10), &host()).expect("a config"));
        assert!(
            !agent.received_datagram(8125, "calls:1|c", now),
            "a configured agent that is not running has no listener either"
        );
        agent.started_at = Some(now);
        assert!(
            agent.received_datagram(8125, "calls:1|c", now),
            "a running agent's listener receives what is sent to its port"
        );
        assert!(
            !agent.received_datagram(9999, "calls:1|c", now),
            "a datagram to another port reaches no listener"
        );
        agent.stop();
        assert!(
            agent.received.is_empty() && !agent.is_running(),
            "stopping the agent closes the listener, got {agent:?}"
        );
    }

    /// RULE: the kernel's socket table reports exactly the listener the running agent has.
    #[test]
    fn the_socket_table_reports_the_open_listener() {
        let mut agent = AgentRuntime::default();
        let stopped = udp_socket_table(&agent);
        assert_eq!(
            stopped.lines().count(),
            1,
            "expected the header alone for a machine with no listener, got {stopped:?}"
        );
        agent.config = Some(AgentConfig::parse(&configuration(10), &host()).expect("a config"));
        agent.started_at = Some(SystemTime::UNIX_EPOCH);
        let running = udp_socket_table(&agent);
        assert!(
            running.lines().any(|line| line.contains(":1FBD")),
            "expected the listener's port in hexadecimal, got {running:?}"
        );
        // (the address the listener is open on, the hexadecimal the table writes)
        let rows: &[(&str, &str)] = &[
            ("0.0.0.0", "00000000"),
            ("127.0.0.1", "0100007F"),
            ("10.0.1.5", "0501000A"),
            ("not-an-address", "00000000"),
        ];
        for (address, expected) in rows {
            let written = hexadecimal_address(address);
            assert_eq!(
                written, *expected,
                "case {address:?}: expected {expected}, got {written}"
            );
        }
    }

    /// RULE: `fetch-config` takes the document the machine holds, a start needs one, and the
    /// status reports what the agent is.
    #[test]
    fn the_control_program_configures_and_starts_the_agent() {
        let mut tree = Tree::default();
        tree.write("/tmp/agent.json", &configuration(10));
        let mut agent = AgentRuntime::default();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        // A start with no configuration at all is the failure the program reports.
        let invocation = ControlInvocation::read(&words(&["-a", "start", "-m", "ec2"]))
            .expect("the invocation is one this agent performs");
        let answered = controlled(&invocation, &mut agent, &host(), &mut tree, now);
        assert_eq!(
            answered.status, FAILED,
            "expected a start with no configuration to fail, got {answered:?}"
        );
        // ... and a fetch of a document the machine does not hold is the same.
        let invocation = ControlInvocation::read(&words(&[
            "-a",
            "fetch-config",
            "-m",
            "ec2",
            "-c",
            "file:/tmp/absent.json",
            "-s",
        ]))
        .expect("the invocation is one this agent performs");
        let answered = controlled(&invocation, &mut agent, &host(), &mut tree, now);
        assert_eq!(
            answered.status, FAILED,
            "expected a fetch of an absent document to fail, got {answered:?}"
        );
        assert!(
            !agent.is_running(),
            "expected the agent not to be running, got {agent:?}"
        );
        // The documented invocation: fetch the configuration and start.
        let invocation = ControlInvocation::read(&words(&[
            "-a",
            "fetch-config",
            "-m",
            "ec2",
            "-c",
            "file:/tmp/agent.json",
            "-s",
        ]))
        .expect("the invocation is one this agent performs");
        let answered = controlled(&invocation, &mut agent, &host(), &mut tree, now);
        assert_eq!(
            answered.status, SUCCESS,
            "expected the documented invocation to succeed, got {answered:?}"
        );
        assert_eq!(
            agent.started_at,
            Some(now),
            "expected the agent started, got {agent:?}"
        );
        assert_eq!(
            tree.read(AGENT_APPLIED_CONFIG_PATH).as_deref(),
            Some(configuration(10).as_str()),
            "expected the configuration kept where the agent keeps its own"
        );
        assert!(
            tree.read(GUEST_NET_UDP_PATH)
                .is_some_and(|table| table.contains(":1FBD")),
            "expected the kernel's socket table to report the listener, got {:?}",
            tree.read(GUEST_NET_UDP_PATH)
        );
        let invocation = ControlInvocation::read(&words(&["-a", "status", "-m", "ec2"]))
            .expect("the invocation is one this agent performs");
        let reported = controlled(&invocation, &mut agent, &host(), &mut tree, now)
            .output
            .join("\n");
        for expected in ["\"status\":\"running\"", "\"configstatus\":\"configured\""] {
            assert!(
                reported.contains(expected),
                "expected the status to report {expected}, got {reported:?}"
            );
        }
        // Stopping it closes the listener, and the table says so.
        let invocation = ControlInvocation::read(&words(&["-a", "stop", "-m", "ec2"]))
            .expect("the invocation is one this agent performs");
        controlled(&invocation, &mut agent, &host(), &mut tree, now);
        assert!(
            tree.read(GUEST_NET_UDP_PATH)
                .is_some_and(|table| !table.contains(":1FBD")),
            "expected no listener in the table once the agent is stopped"
        );
        // An action this agent has none of is a form the program performs nothing of.
        assert!(
            ControlInvocation::read(&words(&["-a", "remove-config"])).is_none(),
            "an action this agent has none of is performed by nothing"
        );
        assert!(
            ControlInvocation::read(&words(&["-m", "ec2"])).is_none(),
            "an invocation naming no action is performed by nothing"
        );
    }

    /// The words of an invocation, as the session hands them over.
    fn words(written: &[&str]) -> Vec<String> {
        written.iter().map(|word| (*word).to_string()).collect()
    }

    /// RULE: a running agent owes one collection of each declared measurement per interval,
    /// stamped at the instant it was taken, and the datagrams its listener received — and a
    /// measurement the configuration does not declare is published by nothing.
    #[test]
    fn the_agent_publishes_what_its_configuration_declares() {
        let mut agent = AgentRuntime::default();
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        agent.config = Some(AgentConfig::parse(&configuration(10), &host()).expect("a config"));
        agent.started_at = Some(started);
        let measurements = GuestMeasurements {
            memory: super::super::guest_memory::guest_memory(Some(1_024)),
            filesystem: GuestFilesystem::of(Some(8), 0),
        };
        // Nothing is owed before the agent has been running for an interval beyond its first
        // collection.
        let first = due_samples(&mut agent, &measurements, started);
        let names: Vec<&str> = first
            .iter()
            .map(|sample| sample.metric_name.as_str())
            .collect();
        assert_eq!(
            names,
            ["mem_used_percent", "mem_used"],
            "expected one collection of each declared measurement at the start, got {names:?}"
        );
        assert!(
            first
                .iter()
                .all(|sample| sample.namespace == "Custom/Agent"
                    && sample.dimensions.get("InstanceId").map(String::as_str)
                        == Some("i-0123456789abcdef0")),
            "expected the configured namespace and dimensions, got {first:?}"
        );
        assert!(
            due_samples(&mut agent, &measurements, started).is_empty(),
            "expected no second collection in the same instant"
        );
        // Three intervals later, the collections of those intervals are owed.
        let later = started + Duration::from_secs(30);
        let caught_up = due_samples(&mut agent, &measurements, later);
        let used_percent: Vec<i64> = caught_up
            .iter()
            .filter(|sample| sample.metric_name == "mem_used_percent")
            .map(|sample| sample.at_secs)
            .collect();
        assert_eq!(
            used_percent,
            [
                epoch_secs(started) + 10,
                epoch_secs(started) + 20,
                epoch_secs(started) + 30
            ],
            "expected one collection per interval, stamped at its own instant, got {caught_up:?}"
        );
        // A datagram the listener received is published as the datapoint it carries, under the
        // name it was sent with and the dimensions its tags and the configuration state.
        assert!(
            agent.received_datagram(8125, "orders.placed:42|c|#stage:test", later),
            "a running agent's listener receives the datagram"
        );
        let published = due_samples(&mut agent, &measurements, later);
        let statsd: Vec<&AgentSample> = published
            .iter()
            .filter(|sample| sample.metric_name == "orders.placed")
            .collect();
        assert_eq!(
            statsd.len(),
            1,
            "expected the received value published once, got {published:?}"
        );
        let sample = statsd[0];
        assert!(
            (sample.value - 42.0).abs() < f64::EPSILON
                && sample.dimensions.get("stage").map(String::as_str) == Some("test")
                && sample.dimensions.contains_key("InstanceId"),
            "expected the value and its dimensions, got {sample:?}"
        );
        assert!(
            due_samples(&mut agent, &measurements, later)
                .iter()
                .all(|sample| sample.metric_name != "orders.placed"),
            "expected a received datagram to be published exactly once"
        );
        // Nothing the configuration does not declare is ever published.
        let config = agent.config.clone().expect("a configuration");
        assert!(
            !config.declares("disk_used_percent") && !config.declares("mem_free"),
            "expected the undeclared measurements to be undeclared, got {config:?}"
        );
        // ... and an agent that is not running owes nothing at all.
        agent.stop();
        assert!(
            due_samples(&mut agent, &measurements, later + Duration::from_secs(600)).is_empty(),
            "expected a stopped agent to publish nothing"
        );
    }

    /// RULE: a line that is not a StatsD metric carries no datapoint.
    #[test]
    fn a_line_that_is_no_metric_publishes_nothing() {
        let config = AgentConfig::parse(&configuration(10), &host()).expect("a config");
        // (the line, how many datapoints it carries)
        let rows: &[(&str, usize)] = &[
            ("calls:1|c", 1),
            ("calls:1.5|g", 1),
            ("calls:-2|g", 1),
            ("calls:12|ms|@0.5", 1),
            ("calls:not-a-number|c", 0),
            (":5|c", 0),
            ("calls", 0),
            ("", 0),
        ];
        for (line, expected) in rows {
            let carried = statsd_samples(line, &config, 0).len();
            assert_eq!(
                carried, *expected,
                "case {line:?}: expected {expected} datapoint(s), got {carried}"
            );
        }
    }
}

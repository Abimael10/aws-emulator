// SPDX-License-Identifier: CC0-1.0

//! What a cluster ADMITS a `spark-submit` step to: the containers the submission asks for,
//! the largest container the cluster's own capacity can allocate, and the outcome that
//! follows.
//!
//! One concern of this service's state, beside [`super::spark_application`] (WHICH
//! application a step submits) and [`super::spark_step_run`] (RUNNING one whose code this
//! emulator holds an engine for). What this module answers is the half of a submission's
//! outcome that is decided BEFORE any application code runs, and that follows from state
//! this emulator really holds: the cluster's instance types, its own configuration and the
//! resources the command line asks for.
//!
//! # The rule
//!
//! A Spark application submitted to a cluster runs its executors, and in cluster deploy mode
//! its driver, inside containers the cluster's resource manager allocates, so a submission is
//! ADMITTED only while every container it asks for fits the largest one that cluster can
//! allocate; `spark-submit` refuses one that does not before it submits anything, and the
//! step carrying it therefore ends in that refusal. In CLIENT deploy mode the driver runs in
//! the submitting process — on the node the step runs on — rather than in a container, so
//! only the small application master the client mode asks for is held to that ceiling, which
//! is why the same oversized driver is refused in one deploy mode and admitted in the other.
//! The executors are asked for in both deploy modes, so their own container is held to the
//! ceiling either way.
//!
//! The figures are Spark's own: what the command line asks for ([`SparkSubmitRequest`]),
//! else what the cluster's configuration sets, else the documented default of the property
//! ([`DEFAULT_DRIVER_MEMORY_MIB`] and the constants beside it). The ceiling is the memory a
//! node of the cluster offers containers ([`yarn_node_memory_mib`]), taken over the
//! collections that run them.
//!
//! # What a step this answers for ENDS in
//!
//! A submission the cluster does not admit ends the step in the modelled failure state
//! carrying the refusal `spark-submit` answered with ([`submission_refusal`]) — a failure of
//! the submitted work, so the step's `ActionOnFailure` applies to it.
//!
//! A submission the cluster ADMITS ends the step in what the application it submitted
//! answered, where this emulator holds an engine and bytes for that application: a Python
//! application in Amazon S3 is RUN and the step ends in what the run came to
//! ([`super::spark_step_run`]). For an admitted submission whose application file is the
//! cluster NODE's own — the one its release installed, or its bootstrap put there — the
//! cluster's admission IS what this service can answer for, so the step ends `COMPLETED`
//! ("for a step to be considered complete, the main function must exit with a zero exit
//! code", `AddJobFlowSteps` doc) and nothing of a run is reported for it: no console stream,
//! no exit status and no written object ([`StepOutcome::log`] stays `None`), so nothing here
//! is a stand-in for output no engine produced. An application file in Amazon S3 is answered
//! for by the path that READS that store instead, because an object the store holds nothing
//! under is an application that is not there.
//!
//! A cluster whose release ships no Spark has no `spark-submit` on its nodes to admit
//! anything, and a cluster whose capacity this service cannot size answers no ceiling; a
//! step on either keeps the end every submission this emulator accounts for nothing of
//! reaches ([`STEP_NOT_EXECUTED_REASON`]).

use super::*;

/// `spark.submit.deployMode` — "whether to launch the driver program locally (client) or on
/// one of the worker machines inside the cluster (cluster)" (`spark-submit --help`) — and the
/// option that sets it on the command line.
pub const CLUSTER_DEPLOY_MODE: &str = "cluster";
/// See [`CLUSTER_DEPLOY_MODE`]. This is the mode a submission that names none runs in: "the
/// default is client" (`spark-submit --help`).
pub const CLIENT_DEPLOY_MODE: &str = "client";

/// The `spark-submit` options this reading takes a value from, each beside the Spark property
/// it sets.
///
/// SOURCE: `spark-submit --help`'s own option list, which states each option's property.
const DEPLOY_MODE_OPTION: &str = "--deploy-mode";
const DRIVER_MEMORY_OPTION: &str = "--driver-memory";
const EXECUTOR_MEMORY_OPTION: &str = "--executor-memory";

/// The Spark properties this admission reads, by the name a `--conf` or a cluster
/// configuration sets them under.
///
/// SOURCE: the Spark configuration documentation (`spark.driver.memory`,
/// `spark.executor.memory`, `spark.driver.memoryOverhead`, `spark.executor.memoryOverhead`,
/// `spark.yarn.am.memory`, `spark.executor.pyspark.memory`, `spark.memory.offHeap.enabled`
/// and `spark.memory.offHeap.size`), plus the deprecated spelling of the executor overhead
/// that is still accepted (`spark.yarn.executor.memoryOverhead`).
pub const DEPLOY_MODE_PROPERTY: &str = "spark.submit.deployMode";
pub const DRIVER_MEMORY_PROPERTY: &str = "spark.driver.memory";
pub const DRIVER_OVERHEAD_PROPERTY: &str = "spark.driver.memoryOverhead";
pub const EXECUTOR_MEMORY_PROPERTY: &str = "spark.executor.memory";
pub const EXECUTOR_OVERHEAD_PROPERTY: &str = "spark.executor.memoryOverhead";
pub const LEGACY_EXECUTOR_OVERHEAD_PROPERTY: &str = "spark.yarn.executor.memoryOverhead";
pub const AM_MEMORY_PROPERTY: &str = "spark.yarn.am.memory";
pub const AM_OVERHEAD_PROPERTY: &str = "spark.yarn.am.memoryOverhead";
pub const PYSPARK_MEMORY_PROPERTY: &str = "spark.executor.pyspark.memory";
pub const OFF_HEAP_ENABLED_PROPERTY: &str = "spark.memory.offHeap.enabled";
pub const OFF_HEAP_SIZE_PROPERTY: &str = "spark.memory.offHeap.size";

/// The option that sets one Spark property on the command line, in either form the command
/// line spells it (`--conf k=v` and its short name).
const CONF_OPTION: &str = "--conf";
const CONF_SHORT_OPTION: &str = "-c";

/// `spark.driver.memory`'s documented default: "1g" (Spark configuration documentation).
pub const DEFAULT_DRIVER_MEMORY_MIB: i64 = 1024;
/// `spark.executor.memory`'s documented default: "1g" (same source).
pub const DEFAULT_EXECUTOR_MEMORY_MIB: i64 = 1024;
/// `spark.yarn.am.memory`'s documented default: "512m" (same source). This is the memory the
/// application master asks for in CLIENT deploy mode, where it launches executors and the
/// driver runs outside the cluster.
pub const DEFAULT_AM_MEMORY_MIB: i64 = 512;

/// The floor of a container's memory overhead: `spark.driver.memoryOverhead` and
/// `spark.executor.memoryOverhead` default to "driverMemory * spark.driver.memoryOverheadFactor,
/// with minimum of 384" (Spark configuration documentation).
pub const MIN_MEMORY_OVERHEAD_MIB: i64 = 384;
/// The factor in that same default, as a percentage: `spark.driver.memoryOverheadFactor` and
/// `spark.executor.memoryOverheadFactor` default to 0.10 (same source).
pub const MEMORY_OVERHEAD_PERCENT: i64 = 10;

/// The largest size this reading answers, in MiB: a pebibyte, orders of magnitude above the
/// memory of any instance type Amazon EC2 offers.
///
/// CHOSEN, not a Spark or Amazon EMR limit: it is the bound on what a caller's own word can
/// make the admission arithmetic carry, so no size read off a command line or a configuration
/// can take a sum of them beyond what the unit holds. A size ABOVE it is taken as this
/// ceiling rather than discarded, because a container larger than any cluster can allocate is
/// refused either way and discarding it would fall back to the property's default and admit
/// it.
pub const MAX_MEMORY_MIB: i64 = 1024 * 1024 * 1024;

/// What a cluster reserves OUT of a node's memory for the operating system and its own
/// daemons, as a percentage of that node's memory, and the ceiling on that reserve.
///
/// SOURCE: Amazon EMR's published per-instance-type task configuration defaults, which state
/// `yarn.nodemanager.resource.memory-mb` for every instance type. Those figures are the
/// node's memory less a reserve that is a quarter of it up to 8 GiB and 8 GiB from there on —
/// an `m5.xlarge`'s 16 GiB node offers 12288 MB, an `m5.2xlarge`'s 32 GiB offers 24576 MB and
/// an `m5.8xlarge`'s 128 GiB offers 122880 MB — so the two figures below state that whole
/// table rather than a sample of it.
const NODE_RESERVED_PERCENT: i64 = 25;
const MAX_NODE_RESERVED_MIB: i64 = 8 * 1024;

/// How many words of a `spark-submit` command line this reading walks: the same bound the
/// reading of WHICH application it submits uses ([`super::spark_application`]).
const MAX_SUBMIT_WORDS: usize = 512;

/// The words a POSIX shell reads as an operator between commands rather than as an argument.
///
/// A command line carrying one is more than the one `spark-submit` it starts with, so the
/// resources that line asks for are not the whole of what it does and this module answers for
/// none of it.
const SHELL_OPERATORS: &[&str] = &["&&", "||", ";", "|", "&", ">", ">>", "<"];

/// What a `spark-submit` command line ASKS its cluster for, read off that command line once
/// when the step was submitted.
///
/// Each member is what the COMMAND LINE stated, RESOLVED here rather than later: a size is
/// read into the unit the ceiling is stated in once, when the step is submitted, because the
/// command line never changes — so answering the admission of a whole queue costs integer
/// arithmetic per step and no parsing at all. `None` is a property the command line stated
/// nothing for, or stated a value Spark cannot read: the cluster's own configuration and the
/// property's documented default are what fill those in, resolved against the cluster
/// ([`ClusterSparkDefaults`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SparkSubmitRequest {
    /// Where the driver runs ([`CLUSTER_DEPLOY_MODE`], [`CLIENT_DEPLOY_MODE`]).
    pub deploy_mode: Option<String>,
    /// `spark.driver.memory`, in MiB.
    pub driver_memory: Option<i64>,
    /// `spark.driver.memoryOverhead`.
    pub driver_overhead: Option<i64>,
    /// `spark.executor.memory`.
    pub executor_memory: Option<i64>,
    /// `spark.executor.memoryOverhead`, under either of the two names it is set by.
    pub executor_overhead: Option<i64>,
    /// `spark.executor.pyspark.memory`, which an executor asks for beside its own.
    pub pyspark_memory: Option<i64>,
    /// `spark.memory.offHeap.enabled` and `spark.memory.offHeap.size`: off-heap memory counts
    /// towards an executor's container only while it is enabled.
    pub off_heap_enabled: Option<bool>,
    pub off_heap_size: Option<i64>,
    /// The APPLICATION the command line names — its first positional word — and `None` for a
    /// command line that names none at all, which is no submission the cluster could admit.
    pub application: Option<String>,
}

impl SparkSubmitRequest {
    /// The deploy mode this submission runs in: the one it named, folded to the spelling the
    /// property uses, or the documented default for a submission that named none.
    #[must_use]
    pub fn deploy_mode(&self) -> &str {
        match self.deploy_mode.as_deref() {
            Some(named) if named.trim().eq_ignore_ascii_case(CLUSTER_DEPLOY_MODE) => {
                CLUSTER_DEPLOY_MODE
            }
            _ => CLIENT_DEPLOY_MODE,
        }
    }
}

/// The `spark-submit` a step's command line IS, as its words, or `None` when the step submits
/// no single `spark-submit` of its own.
///
/// The words are the ones the reading of which application a step submits walks
/// ([`spark_submit_words`]), minus the one case that reading admits and this one must not: a
/// command line carrying a shell operator ([`SHELL_OPERATORS`]) is a line that does more than
/// the `spark-submit` it starts with, and the resources of that one command say nothing about
/// what the rest of the line does.
#[must_use]
pub fn submitted_spark_submit(jar: &str, args: &[String]) -> Option<Vec<String>> {
    let words = spark_submit_words(jar, args)?;
    words
        .iter()
        .all(|word| !SHELL_OPERATORS.contains(&word.as_str()))
        .then_some(words)
}

/// What the `spark-submit` a step submits asks its cluster for, or `None` when the step
/// submits none ([`submitted_spark_submit`]).
///
/// The command's own grammar is what is walked — `spark-submit [options] <application>
/// [application arguments]` — so the reading stops at the application: a word after it is the
/// APPLICATION's own argument and sets nothing of Spark's. An option carries its value in the
/// next word or after an `=` in the same word, and `--conf`/`-c` carries a `property=value`
/// pair, which is how every property a dedicated option does not have is set.
#[must_use]
pub fn submitted_spark_request(jar: &str, args: &[String]) -> Option<SparkSubmitRequest> {
    let words = submitted_spark_submit(jar, args)?;
    let mut request = SparkSubmitRequest::default();
    let mut rest = words.iter().take(MAX_SUBMIT_WORDS).skip(1);
    while let Some(word) = rest.next() {
        if !word.starts_with('-') {
            // The application: every word from here on is its own.
            request.application = Some(word.clone());
            break;
        }
        let (option, inline) = match word.split_once('=') {
            Some((option, value)) => (option, Some(value.to_string())),
            None => (word.as_str(), None),
        };
        // The value of an option that takes one stands in the same word after an `=` or in the
        // word after it; a flag that takes none is skipped as the option word it is.
        match option {
            DEPLOY_MODE_OPTION => {
                request.deploy_mode = inline.or_else(|| rest.next().cloned());
            }
            DRIVER_MEMORY_OPTION => {
                request.driver_memory = inline
                    .or_else(|| rest.next().cloned())
                    .and_then(|size| memory_mib(&size));
            }
            EXECUTOR_MEMORY_OPTION => {
                request.executor_memory = inline
                    .or_else(|| rest.next().cloned())
                    .and_then(|size| memory_mib(&size));
            }
            CONF_OPTION | CONF_SHORT_OPTION => {
                if let Some(pair) = inline.or_else(|| rest.next().cloned()) {
                    if let Some((property, setting)) = pair.split_once('=') {
                        request.set_property(property, setting);
                    }
                }
            }
            // Every other option is one this admission reads nothing from. One that takes a
            // value still consumes it, so that value is never mistaken for the application.
            other if inline.is_none() && VALUE_OPTIONS.contains(&other) => {
                rest.next();
            }
            _ => {}
        }
    }
    Some(request)
}

impl SparkSubmitRequest {
    /// Record the Spark property `property` the command line set, for the properties this
    /// admission reads, in the unit each is read in.
    ///
    /// A size Spark cannot read sets nothing, so the property falls through to what the
    /// cluster's own configuration sets and then to its documented default, exactly as a
    /// property the command line never mentioned does.
    fn set_property(&mut self, property: &str, value: &str) {
        match property {
            DEPLOY_MODE_PROPERTY => self.deploy_mode = Some(value.to_string()),
            DRIVER_MEMORY_PROPERTY => self.driver_memory = memory_mib(value),
            DRIVER_OVERHEAD_PROPERTY => self.driver_overhead = memory_mib(value),
            EXECUTOR_MEMORY_PROPERTY => self.executor_memory = memory_mib(value),
            EXECUTOR_OVERHEAD_PROPERTY | LEGACY_EXECUTOR_OVERHEAD_PROPERTY => {
                self.executor_overhead = memory_mib(value);
            }
            PYSPARK_MEMORY_PROPERTY => self.pyspark_memory = memory_mib(value),
            OFF_HEAP_ENABLED_PROPERTY => self.off_heap_enabled = Some(states_true(value)),
            OFF_HEAP_SIZE_PROPERTY => {
                self.off_heap_size = memory_mib_in(value, BareUnit::Bytes);
            }
            _ => {}
        }
    }
}

/// Whether a Spark property's value turns a switch ON, as Spark reads a boolean property.
fn states_true(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("true")
}

/// A size as Spark reads one, in MiB, or `None` for a value that is no size at all.
///
/// SOURCE: Spark's own byte-string form, as its configuration documentation states it — "1g",
/// "512m", "1024" — where the unit is a suffix in binary multiples and a value with no suffix
/// is in the unit the PROPERTY is stated in, MiB for every memory property read here but the
/// off-heap size, which is read as bytes ([`BareUnit`]). That last part is why the form is
/// read against the property rather than by a general byte-size parser: the same digits mean
/// MiB to one property and bytes to another.
///
/// Every size answered is at most [`MAX_MEMORY_MIB`], so a figure a caller's own word put
/// here cannot carry this arithmetic away.
#[must_use]
pub fn memory_mib(value: &str) -> Option<i64> {
    memory_mib_in(value, BareUnit::Mebibytes)
}

/// The unit a size with NO suffix carries: the one its own property is stated in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BareUnit {
    /// Every memory property read here but the off-heap size.
    Mebibytes,
    /// `spark.memory.offHeap.size`, which Spark reads as a byte string with no implied unit.
    Bytes,
}

/// See [`memory_mib`]: a size in MiB, where a value with no suffix is in `bare`.
fn memory_mib_in(value: &str, bare: BareUnit) -> Option<i64> {
    const MIB: i64 = 1024 * 1024;
    let trimmed = value.trim().to_ascii_lowercase();
    let digits_end = trimmed
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, suffix) = trimmed.split_at(digits_end);
    if digits.is_empty() {
        return None;
    }
    // Every character of `digits` is an ASCII digit, so the one way the parse fails is a
    // value too large for the unit it is stated in: such a size is taken AS the ceiling,
    // which is still larger than any cluster can allocate and is refused for it.
    let amount: i64 = digits.parse().unwrap_or(MAX_MEMORY_MIB);
    // Each unit is written as its own letter or as that letter with a `b`.
    let mib = match suffix {
        "" => match bare {
            BareUnit::Mebibytes => amount,
            BareUnit::Bytes => amount / MIB,
        },
        "m" | "mb" => amount,
        // A size in bytes or in KiB rounds DOWN to the whole MiB the ceiling is stated in.
        "b" => amount / MIB,
        "k" | "kb" => amount / 1024,
        "g" | "gb" => amount.saturating_mul(1024),
        "t" | "tb" => amount.saturating_mul(MIB),
        "p" | "pb" => amount.saturating_mul(MIB * 1024),
        _ => return None,
    };
    Some(mib.min(MAX_MEMORY_MIB))
}

/// The memory, in MiB, one node of `instance_type` offers the containers its cluster runs, or
/// `None` for a value this service's catalog sizes no node under.
///
/// A node keeps part of its memory for the operating system and the cluster's own daemons and
/// offers the rest ([`NODE_RESERVED_PERCENT`], [`MAX_NODE_RESERVED_MIB`]), which is also the
/// largest single container it can hold.
#[must_use]
pub fn yarn_node_memory_mib(instance_type: &str) -> Option<i64> {
    let memory = instance_type_memory_mib(instance_type)?;
    let reserved = memory
        .saturating_mul(NODE_RESERVED_PERCENT)
        .saturating_div(100)
        .min(MAX_NODE_RESERVED_MIB);
    Some(memory.saturating_sub(reserved).max(0))
}

/// The largest container the cluster `cluster` can allocate, in MiB, or `None` for a cluster
/// whose capacity this service cannot size.
///
/// A container runs on a node that takes work: the cluster's CORE and TASK collections. A
/// cluster that has none of either runs its work on its primary node, so that node's own
/// offer is the ceiling — which is the whole of a single-node cluster.
#[must_use]
pub fn cluster_max_container_memory_mib(cluster: &ClusterRecord) -> Option<i64> {
    let offers = |roles: &[&str]| -> Option<i64> {
        planned_launches(&cluster.groups, &cluster.fleets)
            .into_iter()
            .filter(|launch| roles.contains(&launch.role.as_str()))
            .filter_map(|launch| yarn_node_memory_mib(&launch.instance_type))
            .max()
    };
    offers(&[instance_group_type::CORE, instance_group_type::TASK])
        .or_else(|| offers(&[instance_group_type::MASTER]))
}

/// `FailureDetails$Reason` of a step whose submission its cluster could not admit: the class
/// of the failure, beside `spark-submit`'s own refusal as the message.
pub const SUBMISSION_REFUSED_REASON: &str =
    "The cluster could not allocate a container of the size the submission asked for.";

/// How `spark-submit` refuses a submission whose APPLICATION MASTER does not fit: the driver
/// in cluster deploy mode, and the launcher the client deploy mode asks for.
///
/// Spark's own wording for that refusal, whose fixed text is everything but the three
/// figures: the memory the master asks for, the overhead asked for beside it and the ceiling
/// it was held to.
#[must_use]
pub fn am_memory_above_threshold_message(memory: i64, overhead: i64, maximum: i64) -> String {
    format!(
        "Required AM memory ({memory}+{overhead} MB) is above the max threshold ({maximum} MB) \
         of this cluster! Please increase the value of 'yarn.scheduler.maximum-allocation-mb'."
    )
}

/// How it refuses one whose EXECUTORS do not fit, in the same form: the memory an executor
/// asks for, the off-heap and overhead and Python memory asked for beside it, and the ceiling.
#[must_use]
pub fn executor_memory_above_threshold_message(
    memory: i64,
    off_heap: i64,
    overhead: i64,
    pyspark: i64,
    maximum: i64,
) -> String {
    format!(
        "Required executor memory ({memory} MB), offHeap memory ({off_heap}) MB, overhead \
         ({overhead} MB), and PySpark memory ({pyspark} MB) is above the max threshold \
         ({maximum} MB) of this cluster! Please check the values of \
         'yarn.scheduler.maximum-allocation-mb' and/or 'yarn.nodemanager.resource.memory-mb'."
    )
}

/// The container memory a submission asks for, resolved against the cluster it was submitted
/// to: each figure is what the command line asked for, else what the cluster's own
/// configuration sets, else the property's documented default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubmittedContainers {
    /// The application master's own memory and the overhead beside it: the driver's in cluster
    /// deploy mode, and the launcher's in client deploy mode.
    master_memory: i64,
    master_overhead: i64,
    /// One executor's memory and everything asked for beside it.
    executor_memory: i64,
    executor_off_heap: i64,
    executor_overhead: i64,
    executor_pyspark: i64,
}

/// The overhead a container of `memory` carries when nothing set it: the documented default,
/// a share of the container's own memory with a floor under it.
///
/// The share is computed in the saturating domain, so no size — every one of them bounded by
/// [`MAX_MEMORY_MIB`] already — can carry it beyond what the unit holds.
fn default_overhead_mib(memory: i64) -> i64 {
    memory
        .saturating_mul(MEMORY_OVERHEAD_PERCENT)
        .saturating_div(100)
        .max(MIN_MEMORY_OVERHEAD_MIB)
}

/// What a CLUSTER's own configuration sets of the properties this admission reads, resolved
/// ONCE per read.
///
/// These are the figures that fill in what a submission did not state
/// ([`SparkSubmitRequest`]), and they belong to the cluster rather than to any one step — so
/// the configuration tree is walked a fixed number of times per request however many steps
/// that request answers for, rather than once per property per step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterSparkDefaults {
    driver_memory: Option<i64>,
    driver_overhead: Option<i64>,
    am_memory: Option<i64>,
    am_overhead: Option<i64>,
    executor_memory: Option<i64>,
    executor_overhead: Option<i64>,
    pyspark_memory: Option<i64>,
    off_heap_enabled: bool,
    off_heap_size: Option<i64>,
}

impl ClusterSparkDefaults {
    /// What `configurations` sets of each of them.
    #[must_use]
    pub fn of(configurations: &[Configuration]) -> Self {
        let size = |property| {
            configuration_property(configurations, property).and_then(|value| memory_mib(value))
        };
        Self {
            driver_memory: size(DRIVER_MEMORY_PROPERTY),
            driver_overhead: size(DRIVER_OVERHEAD_PROPERTY),
            am_memory: size(AM_MEMORY_PROPERTY),
            am_overhead: size(AM_OVERHEAD_PROPERTY),
            executor_memory: size(EXECUTOR_MEMORY_PROPERTY),
            // The deprecated spelling sets the same property, and the current one wins where
            // the configuration carries both.
            executor_overhead: size(EXECUTOR_OVERHEAD_PROPERTY)
                .or_else(|| size(LEGACY_EXECUTOR_OVERHEAD_PROPERTY)),
            pyspark_memory: size(PYSPARK_MEMORY_PROPERTY),
            off_heap_enabled: configuration_property(configurations, OFF_HEAP_ENABLED_PROPERTY)
                .is_some_and(|value| states_true(value)),
            // The off-heap size is the one memory property here whose bare value is BYTES
            // rather than MiB ([`BareUnit`]).
            off_heap_size: configuration_property(configurations, OFF_HEAP_SIZE_PROPERTY)
                .and_then(|value| memory_mib_in(value, BareUnit::Bytes)),
        }
    }
}

/// One memory property of a submission, in MiB: what the command line asked for, else what the
/// cluster's configuration sets, else the property's documented default.
fn memory_asked(asked: Option<i64>, configured: Option<i64>, default: i64) -> i64 {
    asked.or(configured).unwrap_or(default)
}

impl SubmittedContainers {
    /// What `request` asks for of a cluster whose own configuration resolved to `defaults`.
    ///
    /// Integer arithmetic over figures both sides already resolved: nothing here parses a
    /// value or walks a configuration, so answering the admission of a whole queue costs a
    /// fixed amount of work per step.
    fn of(defaults: &ClusterSparkDefaults, request: &SparkSubmitRequest) -> Self {
        // The driver is the application master only in cluster deploy mode; in client deploy
        // mode the master is the launcher the client mode asks for and the driver runs in the
        // submitting process instead.
        let (master_memory, master_overhead) = if request.deploy_mode() == CLUSTER_DEPLOY_MODE {
            let memory = memory_asked(
                request.driver_memory,
                defaults.driver_memory,
                DEFAULT_DRIVER_MEMORY_MIB,
            );
            (
                memory,
                memory_asked(
                    request.driver_overhead,
                    defaults.driver_overhead,
                    default_overhead_mib(memory),
                ),
            )
        } else {
            let memory = memory_asked(None, defaults.am_memory, DEFAULT_AM_MEMORY_MIB);
            (
                memory,
                memory_asked(None, defaults.am_overhead, default_overhead_mib(memory)),
            )
        };
        let executor_memory = memory_asked(
            request.executor_memory,
            defaults.executor_memory,
            DEFAULT_EXECUTOR_MEMORY_MIB,
        );
        // Off-heap memory counts towards the container only while it is enabled, and the
        // Python memory only where something asked for it.
        let off_heap_enabled = request
            .off_heap_enabled
            .unwrap_or(defaults.off_heap_enabled);
        Self {
            master_memory,
            master_overhead,
            executor_memory,
            executor_off_heap: if off_heap_enabled {
                memory_asked(request.off_heap_size, defaults.off_heap_size, 0)
            } else {
                0
            },
            executor_overhead: memory_asked(
                request.executor_overhead,
                defaults.executor_overhead,
                default_overhead_mib(executor_memory),
            ),
            executor_pyspark: memory_asked(request.pyspark_memory, defaults.pyspark_memory, 0),
        }
    }
}

/// How the cluster `cluster` refuses the submission `request`, or `None` when it admits it.
///
/// Both containers are held to the largest the cluster can allocate
/// ([`cluster_max_container_memory_mib`]): the EXECUTOR's, whichever deploy mode the
/// submission runs in, and the application MASTER's, which is the driver itself in cluster
/// deploy mode. The executor is judged first, which is the order `spark-submit` judges them
/// in, so a submission that gets both wrong is refused for its executors.
///
/// A cluster whose capacity this service cannot size refuses nothing: there is no ceiling to
/// hold the submission to, so no admission is answered either way ([`resolve_due_submissions`]).
#[must_use]
pub fn submission_refusal(
    cluster: &ClusterRecord,
    request: &SparkSubmitRequest,
) -> Option<StepFailure> {
    let maximum = cluster_max_container_memory_mib(cluster)?;
    refusal_against(
        maximum,
        &ClusterSparkDefaults::of(&cluster.configurations),
        request,
    )
}

/// See [`submission_refusal`]: how a cluster whose largest container is `maximum` and whose own
/// configuration resolved to `defaults` refuses `request`.
///
/// Both are passed in rather than re-derived, so a read that answers the admission of a whole
/// queue sizes that cluster's nodes and walks its configuration once for all of them.
fn refusal_against(
    maximum: i64,
    defaults: &ClusterSparkDefaults,
    request: &SparkSubmitRequest,
) -> Option<StepFailure> {
    let asked = SubmittedContainers::of(defaults, request);
    // Every term is bounded by [`MAX_MEMORY_MIB`] and the totals are summed in the saturating
    // domain, so a size a caller asked for can only make a container LARGER than the ceiling
    // — which is refused — and never wrap round to one that fits.
    let executor = asked
        .executor_memory
        .saturating_add(asked.executor_off_heap)
        .saturating_add(asked.executor_overhead)
        .saturating_add(asked.executor_pyspark);
    let master = asked.master_memory.saturating_add(asked.master_overhead);
    let refusal = |message: String| StepFailure {
        reason: SUBMISSION_REFUSED_REASON.to_string(),
        message,
    };
    if executor > maximum {
        return Some(refusal(executor_memory_above_threshold_message(
            asked.executor_memory,
            asked.executor_off_heap,
            asked.executor_overhead,
            asked.executor_pyspark,
            maximum,
        )));
    }
    if master > maximum {
        return Some(refusal(am_memory_above_threshold_message(
            asked.master_memory,
            asked.master_overhead,
            maximum,
        )));
    }
    None
}

impl ClusterRecord {
    /// Whether a `spark-submit` submitted to this cluster reaches a Spark to submit it: the
    /// cluster's own release is what installs one, so a release that ships no Spark has no
    /// `spark-submit` on its nodes ([`release_application_version`]).
    fn ships_spark(&self) -> bool {
        self.release_label
            .as_deref()
            .and_then(|label| release_application_version(label, SPARK_APPLICATION_NAME))
            .is_some()
    }
}

/// Answer the ADMISSION of every submission of this cluster that has none yet, and record
/// what it decides.
///
/// A submission the cluster does not admit ends its step in that refusal
/// ([`submission_refusal`]). One it DOES admit ends the step in what its application
/// answered, so a step whose application this emulator runs is left to that run
/// ([`super::spark_step_run`]) and every other admitted submission ends `COMPLETED` — see
/// this module's header for what is and is not reported of it.
///
/// An admission is a function of the submission and of the capacity the cluster holds, so it
/// is answered ONCE and then recorded ([`StepRecord::submission_admitted`]), before the step's
/// own end comes due: two reads of a settled step report the same thing, exactly as they do
/// for a statement resolved against the cluster's metastore ([`run_due_steps`]), and a step
/// whose admission has been answered costs a later read nothing at all. Nothing else about a
/// step changes because it was read.
///
/// What ONE call costs is therefore bounded by the steps whose admission is still unanswered,
/// each at a fixed amount of integer arithmetic: the cluster's nodes are sized and its
/// configuration walked ONCE for the whole queue, and each submission's own figures were
/// resolved when the step was submitted ([`SparkSubmitRequest`]).
pub fn resolve_due_submissions(cluster: &mut ClusterRecord) {
    if !cluster.ships_spark() {
        return;
    }
    let unanswered: Vec<usize> = cluster
        .steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.awaits_admission())
        .map(|(index, _)| index)
        .collect();
    if unanswered.is_empty() {
        return;
    }
    // The cluster's nodes are sized ONCE for the whole queue, and its configuration walked
    // once. A cluster whose capacity this service cannot size answers no ceiling, so no
    // admission can be answered either: such a step keeps the end every submission this
    // service accounts for nothing of reaches.
    let Some(maximum) = cluster_max_container_memory_mib(cluster) else {
        return;
    };
    let defaults = ClusterSparkDefaults::of(&cluster.configurations);
    // What the admission ANSWERED for each of those steps: the end a refused submission puts
    // its step in, and `None` for one the cluster admitted whose end another resolver answers.
    let answered: Vec<(usize, Option<StepOutcome>)> = unanswered
        .into_iter()
        .filter_map(|index| {
            let step = cluster.steps.get(index)?;
            let request = step.submission.as_ref()?;
            // A submission that names no application at all submits nothing for the cluster
            // to admit, so no admission is answered for it.
            let application = request.application.as_deref()?;
            if let Some(failure) = refusal_against(maximum, &defaults, request) {
                return Some((
                    index,
                    Some(StepOutcome {
                        state: step_state::FAILED.to_string(),
                        failure: Some(failure),
                        // The submission was refused before anything ran it, so nothing wrote
                        // a console stream and nothing exited with a status.
                        log: None,
                    }),
                ));
            }
            // The cluster admits it, so what the step ends in is what the work it submitted
            // answered — and for the classes this service answers for itself, that answer is
            // recorded by the path that holds it rather than here: an application this
            // service RUNS ([`super::spark_step_run`]) and a statement resolved against or
            // applied to the cluster's metastore ([`run_due_steps`]).
            //
            // An application file in Amazon S3 is an object this emulator's own object store
            // either holds or does not, and a step is not reported COMPLETED for an
            // application that is not there: such a submission is left to the reader that
            // holds those bytes. The admission is the whole of what this service can answer
            // for a submission whose application file is the cluster NODE's own — the one its
            // release installed or its bootstrap put there — which this emulator holds
            // nothing of either way.
            let answered_elsewhere = step.application.is_some()
                || step.work.as_ref().is_some_and(|work| work.accountable)
                || s3_object(application).is_some();
            Some((
                index,
                (!answered_elsewhere).then(|| StepOutcome {
                    state: step_state::COMPLETED.to_string(),
                    failure: None,
                    // Nothing of an application RAN here, so no console stream and no exit
                    // status of one is reported.
                    log: None,
                }),
            ))
        })
        .collect();
    for (index, outcome) in answered {
        if let Some(step) = cluster.steps.get_mut(index) {
            // The admission is answered for this step from here on, whether or not it is what
            // the step ends in, so no later read answers it again.
            step.submission_admitted = true;
            if outcome.is_some() {
                step.outcome = outcome;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A size as Spark reads one. The rows span the suffixes the form carries, the bare value
    /// the property's own unit applies to, the ceiling a size above it is taken as, and a
    /// value that is no size at all.
    #[test]
    fn a_memory_setting_is_read_in_the_unit_it_names() {
        let cases: &[(&str, Option<i64>)] = &[
            ("1024", Some(1024)),
            ("512m", Some(512)),
            ("512M", Some(512)),
            ("2g", Some(2048)),
            ("2G", Some(2048)),
            ("2gb", Some(2048)),
            ("1t", Some(1024 * 1024)),
            ("2097152k", Some(2048)),
            ("4194304b", Some(4)),
            (" 4g ", Some(4096)),
            // No word of a caller's own carries a figure past the ceiling.
            ("9223372036854775807", Some(MAX_MEMORY_MIB)),
            ("99999999999999999999999", Some(MAX_MEMORY_MIB)),
            ("4096p", Some(MAX_MEMORY_MIB)),
            ("", None),
            ("g", None),
            ("2x", None),
        ];
        for (value, expected) in cases {
            let read = memory_mib(value);
            assert_eq!(
                read, *expected,
                "case {value:?}: expected {expected:?}, got {read:?}"
            );
        }
        // The one property whose BARE value is bytes rather than MiB.
        let bytes: &[(&str, Option<i64>)] = &[
            ("4194304", Some(4)),
            ("1024", Some(0)),
            ("4m", Some(4)),
            ("2g", Some(2048)),
        ];
        for (value, expected) in bytes {
            let read = memory_mib_in(value, BareUnit::Bytes);
            assert_eq!(
                read, *expected,
                "case {value:?} as bytes: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// A submission no arithmetic of this admission can carry away: every figure a caller's
    /// own words can ask for is bounded, so the container is larger than any ceiling and the
    /// submission is refused rather than wrapping round into one that fits.
    #[test]
    fn a_submission_asking_for_an_impossible_size_is_refused() {
        let line = |words: &[&str]| -> Vec<String> {
            words.iter().map(|word| (*word).to_string()).collect()
        };
        let absurd = i64::MAX.to_string();
        for words in [
            line(&[
                "spark-submit",
                "--deploy-mode",
                "cluster",
                "--driver-memory",
                &absurd,
                "s3://bucket/job.py",
            ]),
            line(&[
                "spark-submit",
                "--executor-memory",
                &absurd,
                "--conf",
                "spark.memory.offHeap.enabled=true",
                "--conf",
                &format!("spark.memory.offHeap.size={absurd}"),
                "--conf",
                &format!("spark.executor.memoryOverhead={absurd}"),
                "--conf",
                &format!("spark.executor.pyspark.memory={absurd}"),
                "s3://bucket/job.py",
            ]),
        ] {
            let request = submitted_spark_request(COMMAND_RUNNER_JAR, &words)
                .expect("a spark-submit states a request");
            let refusal = refusal_against(12 * 1024, &ClusterSparkDefaults::of(&[]), &request);
            assert!(
                refusal.is_some(),
                "case {words:?}: a container past every ceiling is refused, got {refusal:?}"
            );
        }
    }

    /// The memory a node offers containers, against Amazon EMR's published
    /// `yarn.nodemanager.resource.memory-mb` for those instance types.
    #[test]
    fn a_node_offers_its_memory_less_the_reserve() {
        let cases: &[(&str, Option<i64>)] = &[
            ("m5.large", Some(6 * 1024)),
            ("m5.xlarge", Some(12 * 1024)),
            ("m5.2xlarge", Some(24 * 1024)),
            ("m5.4xlarge", Some(56 * 1024)),
            ("m5.8xlarge", Some(120 * 1024)),
            ("c5.xlarge", Some(6 * 1024)),
            ("r5.xlarge", Some(24 * 1024)),
            ("not-an-instance-type", None),
        ];
        for (instance_type, expected) in cases {
            let offered = yarn_node_memory_mib(instance_type);
            assert_eq!(
                offered, *expected,
                "case {instance_type:?}: expected {expected:?}, got {offered:?}"
            );
        }
    }

    /// What a command line asks for, read off its own grammar. The rows span the forms an
    /// option is written in, a property set through `--conf`, the words after the application
    /// (which set nothing of Spark's), and a line that is more than one `spark-submit`.
    #[test]
    fn a_submission_states_what_it_asks_the_cluster_for() {
        let line = |words: &[&str]| -> Vec<String> {
            words.iter().map(|word| (*word).to_string()).collect()
        };
        let read = |words: &[&str]| submitted_spark_request(COMMAND_RUNNER_JAR, &line(words));

        let separate = read(&[
            "spark-submit",
            "--deploy-mode",
            "cluster",
            "--driver-memory",
            "2G",
            "--executor-memory",
            "4G",
            "s3://bucket/job.py",
            "--driver-memory",
            "99G",
        ])
        .expect("a spark-submit states a request");
        assert_eq!(
            (
                separate.deploy_mode(),
                separate.driver_memory,
                separate.executor_memory,
                separate.application.as_deref(),
            ),
            (
                CLUSTER_DEPLOY_MODE,
                Some(2 * 1024),
                Some(4 * 1024),
                Some("s3://bucket/job.py"),
            ),
            "case \"options before the application\": got {separate:?}"
        );

        let inline = read(&[
            "spark-submit",
            "--deploy-mode=cluster",
            "--conf",
            "spark.driver.memory=6g",
            "--conf=spark.yarn.executor.memoryOverhead=1G",
            "--class",
            "org.example.Main",
            "/usr/lib/spark/examples/jars/spark-examples.jar",
        ])
        .expect("a spark-submit states a request");
        assert_eq!(
            (
                inline.deploy_mode(),
                inline.driver_memory,
                inline.executor_overhead,
                inline.application.as_deref(),
            ),
            (
                CLUSTER_DEPLOY_MODE,
                Some(6 * 1024),
                Some(1024),
                Some("/usr/lib/spark/examples/jars/spark-examples.jar"),
            ),
            "case \"properties set through conf\": got {inline:?}"
        );

        let defaulted = read(&["spark-submit", "s3://bucket/job.py"])
            .expect("a spark-submit states a request");
        assert_eq!(
            (defaulted.deploy_mode(), defaulted.driver_memory),
            (CLIENT_DEPLOY_MODE, None),
            "case \"a bare submit\": got {defaulted:?}"
        );

        for words in [
            line(&["spark-submit", "s3://bucket/job.py", "&&", "false"]),
            line(&["hadoop", "fs", "-ls", "s3://bucket/"]),
        ] {
            let read = submitted_spark_request(COMMAND_RUNNER_JAR, &words);
            assert!(
                read.is_none(),
                "case {words:?}: a line that is not one spark-submit states no request, got {read:?}"
            );
        }
    }
}

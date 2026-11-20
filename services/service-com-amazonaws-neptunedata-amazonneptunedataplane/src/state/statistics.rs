// SPDX-License-Identifier: CC0-1.0

//! The DFE statistics this engine holds for a graph, and what the statistics endpoints
//! answer from them.
//!
//! One [`StatisticsRecord`] is the whole statistics state of one dataset: the generation
//! mode a management call set on it, and the latest generation run — the characteristic
//! sets a run computed from the graph's own data. A dataset the engine keeps no record for
//! has no statistics to report, which is what the modelled
//! [`StatisticsNotAvailableException`] states: a management call is what starts a record
//! (`refresh` adds a run, `enableAutoCompute`/`disableAutoCompute` set the mode), a read
//! answers from it, and a delete removes it.
//!
//! A run's counts are REAL: [`characteristic_sets`] reads the stored graph and counts the
//! distinct predicate signatures in it, so what a statistics read reports follows from what
//! the graph holds, and a run is a snapshot — later writes to the graph do not change the
//! counts an earlier run reported, only what the next run computes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ops::types::{
    BadRequestException, IllegalArgumentException, StatisticsNotAvailableException,
};

use super::PropertyGraph;

/// The `status` a statistics endpoint reports for a request it served. The member is "the
/// HTTP return code of the request. If the request succeeded, the code is 200", which the
/// statistics endpoints state as the HTTP status line of the answer — the same way the
/// other Neptune endpoints in this crate report theirs.
pub const STATISTICS_REQUEST_SERVED: &str = "200 OK";

/// The `statisticsId` reported when no generation run has produced statistics:
/// `Statistics$statisticsId`'s own documentation states it — "A value of -1 indicates that
/// no statistics have been generated".
pub const NO_STATISTICS_ID: &str = "-1";

/// Whether automatic statistics generation is on for a record the engine has just started
/// keeping. The engine generates statistics unless it is told not to, which is what the
/// `disableAutoCompute` mode is for.
pub const AUTO_COMPUTE_DEFAULT: bool = true;

/// What a request for statistics the engine does not have is told. The refusal names no
/// value the caller sent, because the request carries none.
pub const STATISTICS_NOT_AVAILABLE_MESSAGE: &str = "Statistics are not available.";

/// The `disableAutoCompute` mode, by its wire spelling: automatic generation off.
pub const MODE_DISABLE_AUTO_COMPUTE: &str = "disableAutoCompute";

/// The `enableAutoCompute` mode: automatic generation on.
pub const MODE_ENABLE_AUTO_COMPUTE: &str = "enableAutoCompute";

/// The `refresh` mode, which "manually triggers DFE statistics generation".
pub const MODE_REFRESH: &str = "refresh";

/// The `StatisticsNotAvailableException` payload for a read of statistics this engine does
/// not hold. `code` carries the HTTP status, which is what the member's own documentation
/// says it is.
pub fn statistics_not_available() -> StatisticsNotAvailableException {
    StatisticsNotAvailableException {
        detailed_message: STATISTICS_NOT_AVAILABLE_MESSAGE.to_string(),
        request_id: super::new_request_id(),
    }
}

/// The statistics generation mode a management call asks for, as the model's
/// `StatisticsAutoGenerationMode` declares them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatisticsMode {
    /// Turn automatic statistics generation off.
    DisableAutoCompute,
    /// Turn automatic statistics generation on.
    EnableAutoCompute,
    /// Generate statistics now.
    Refresh,
}

impl StatisticsMode {
    /// The mode `value` spells, or `None` for a value the member does not accept.
    ///
    /// The boundary validator refuses a value outside the modelled enum before a handler
    /// runs, so `None` is reachable only if that set ever widens; a caller gets it back as
    /// a refusal naming the modes rather than a guess at which one was meant.
    #[must_use]
    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            MODE_DISABLE_AUTO_COMPUTE => Some(Self::DisableAutoCompute),
            MODE_ENABLE_AUTO_COMPUTE => Some(Self::EnableAutoCompute),
            MODE_REFRESH => Some(Self::Refresh),
            _ => None,
        }
    }
}

/// The characteristic-set counts one generation run produced, which is what
/// `StatisticsSummary` reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatisticsSignatures {
    /// "The total number of signatures across all characteristic sets" — one signature per
    /// distinct set of predicates the graph's nodes carry.
    pub signature_count: i32,
    /// "The total number of characteristic-set instances" — the nodes those sets were
    /// formed from.
    pub instance_count: i32,
    /// "The total number of unique predicates" in the graph.
    pub predicate_count: i32,
}

/// One statistics generation run this engine performed.
#[derive(Debug, Clone)]
pub struct StatisticsGeneration {
    /// The id the run is reported by — `Statistics$statisticsId` and the id a `refresh`
    /// hands back.
    pub statistics_id: String,
    /// When the run computed its counts, which is what `Statistics$date` reports ("The UTC
    /// time at which DFE statistics have most recently been generated").
    pub generated_at: SystemTime,
    /// What the run counted in the graph.
    pub signatures: StatisticsSignatures,
}

/// The statistics state of one dataset: the mode it is generated in, and the latest run.
#[derive(Debug, Clone)]
pub struct StatisticsRecord {
    /// Whether automatic statistics generation is enabled, which is what
    /// `Statistics$autoCompute` reports and what the two autocompute modes set.
    pub auto_compute: bool,
    /// The latest generation run, or `None` while no run has produced statistics.
    pub latest: Option<StatisticsGeneration>,
}

impl Default for StatisticsRecord {
    fn default() -> Self {
        Self {
            auto_compute: AUTO_COMPUTE_DEFAULT,
            latest: None,
        }
    }
}

impl StatisticsRecord {
    /// Whether generated statistics are in use, which is what `Statistics$active` reports:
    /// a record with no run behind it has none for the DFE to use.
    ///
    /// It is the RUN that decides this rather than [`Self::auto_compute`]: the two members
    /// are reported side by side, so tying this one to the mode would report the mode
    /// twice and leave "are there statistics in use" unanswerable. It is also what the
    /// delete reports about what it left behind.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.latest.is_some()
    }

    /// The id of the current generation run, or [`NO_STATISTICS_ID`] when no run has
    /// produced statistics.
    #[must_use]
    pub fn statistics_id(&self) -> String {
        self.latest
            .as_ref()
            .map_or_else(|| NO_STATISTICS_ID.to_string(), |run| run.statistics_id.clone())
    }

    /// Record a generation run that counted `signatures` at `now`, and report its id.
    pub fn record_generation(
        &mut self,
        signatures: StatisticsSignatures,
        now: SystemTime,
    ) -> String {
        let statistics_id = new_statistics_id(now);
        self.latest = Some(StatisticsGeneration {
            statistics_id: statistics_id.clone(),
            generated_at: now,
            signatures,
        });
        statistics_id
    }
}

/// The id of the latest generation run this process minted an id for, which is what keeps
/// the ids below apart.
static LATEST_STATISTICS_ID: AtomicU64 = AtomicU64::new(0);

/// A fresh id naming ONE statistics generation run: the run's own instant in epoch
/// milliseconds, which is the form a Neptune statistics id carries.
///
/// Never an id an earlier run already took — two runs within one millisecond are still two
/// runs, and a run that follows a delete is not the run the delete removed — so the id is
/// taken past the latest one whenever the clock does not already put it there. The id names
/// a run for correlation, is derived from nothing the caller sent, and is not a credential.
pub fn new_statistics_id(now: SystemTime) -> String {
    let millis = now.duration_since(UNIX_EPOCH).map_or(0, |since_epoch| {
        u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
    });
    let mut latest = LATEST_STATISTICS_ID.load(Ordering::Relaxed);
    loop {
        let minted = millis.max(latest.saturating_add(1));
        match LATEST_STATISTICS_ID.compare_exchange_weak(
            latest,
            minted,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return minted.to_string(),
            Err(current) => latest = current,
        }
    }
}

/// Count the characteristic sets in `graph`, the way a generation run does.
///
/// A node's PREDICATES are the property keys set on it together with the types of the
/// relationships leaving it — the two things the model's `NodeStructure` describes a node's
/// structure by (`nodeProperties` and `distinctOutgoingEdgeLabels`). A characteristic set is
/// one distinct such set of predicates, every node carrying predicates is one instance of
/// the set it matches, and a predicate is unique by name across the whole graph. A node
/// carrying no predicates belongs to no characteristic set, so it is not an instance of one.
#[must_use]
pub fn characteristic_sets(graph: &PropertyGraph) -> StatisticsSignatures {
    // Every relationship type leaving a node is one of that node's predicates, so the types
    // are collected by the node they leave before the sets are formed.
    let mut outgoing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for id in graph.relationship_ids() {
        if let Some(relationship) = graph.relationship(&id) {
            outgoing
                .entry(relationship.from_node.clone())
                .or_default()
                .insert(relationship.rel_type.clone());
        }
    }

    let mut sets: BTreeSet<BTreeSet<String>> = BTreeSet::new();
    let mut predicates: BTreeSet<String> = BTreeSet::new();
    let mut instances: usize = 0;
    for id in graph.node_ids() {
        let Some(node) = graph.node(&id) else {
            continue;
        };
        let mut set: BTreeSet<String> = node.properties.keys().cloned().collect();
        if let Some(types) = outgoing.get(&id) {
            set.extend(types.iter().cloned());
        }
        if set.is_empty() {
            continue;
        }
        predicates.extend(set.iter().cloned());
        instances = instances.saturating_add(1);
        sets.insert(set);
    }

    StatisticsSignatures {
        signature_count: counted(sets.len()),
        instance_count: counted(instances),
        predicate_count: counted(predicates.len()),
    }
}

/// A count as a modelled `Integer` reports it. The graph's own ceilings keep every count
/// this engine can produce far inside the range, so the saturation below is unreachable and
/// is here so a count can never wrap.
fn counted(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// The `BadRequestException` payload for a generation run this engine did not finish, so
/// there is no run to report. `BadRequestException` — "Raised when a request is submitted
/// that cannot be processed" — is the modelled error that states what happened without
/// blaming a member of the request. `reason` is the engine's own account of the
/// interruption, escaped so it cannot forge a line in anything that renders the message.
pub fn statistics_generation_interrupted(reason: &str) -> BadRequestException {
    BadRequestException {
        detailed_message: format!(
            "Statistics generation did not finish: {}",
            super::echoed(reason)
        ),
        request_id: super::new_request_id(),
    }
}

/// The `IllegalArgumentException` payload for a statistics generation mode this service
/// does not support, naming the value that was wrong and the modes the member accepts.
pub fn unsupported_statistics_mode(mode: &str) -> IllegalArgumentException {
    super::unsupported_member_value(
        "mode",
        mode,
        crate::ops::types::STATISTICS_AUTO_GENERATION_MODE_VALUES,
    )
}

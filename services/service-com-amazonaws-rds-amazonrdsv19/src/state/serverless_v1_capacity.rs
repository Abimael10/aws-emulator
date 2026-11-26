// SPDX-License-Identifier: CC0-1.0

//! The CAPACITY an Aurora Serverless v1 DB cluster is running on: the Aurora capacity units
//! (ACUs) it holds now, the capacity a scaling the clock still owes will take it to, and the
//! rules a request to set it is held to.
//!
//! A cluster in the [`SERVERLESS_V1_ENGINE_MODE`] engine mode has no instance class to read
//! its compute off — "`EngineMode` … `serverless` … only applies for Aurora Serverless v1 DB
//! clusters" — so its compute is a NUMBER on the cluster itself, which is what
//! `DBCluster$Capacity` reports ("the current capacity of an Aurora Serverless v1 DB
//! cluster") and what `ModifyCurrentDBClusterCapacity` sets. Both readings live here, beside
//! the capacity values each Aurora engine publishes, so the operation that sets the capacity
//! and the reports that publish it cannot drift from each other.
//!
//! A cluster in any other engine mode has no such capacity at all: the Aurora Serverless v2
//! capacity of a `provisioned` cluster is a RANGE its members scale inside
//! ([`super::serverless_capacity`]), reported as a CloudWatch series rather than as this
//! member, and a Multi-AZ DB cluster runs on the instance class it was created with. So the
//! member is reported for a Serverless v1 cluster and for one only, and the operation that
//! sets it refuses every other cluster on the cluster's TYPE
//! ([`CAPACITY_NOT_MODIFIABLE_MESSAGE`]).
//!
//! # The scaling the clock owes
//!
//! Setting the capacity is not instantaneous on the real service: the response carries the
//! capacity the cluster is on (`CurrentCapacity`) beside "the capacity that the DB cluster
//! scales to next" (`PendingCapacity`), which are two different numbers while the scaling
//! runs. So a call records the capacity asked for with the instant the cluster reaches it
//! ([`PendingServerlessV1Capacity`]) and the clock materialises it
//! ([`ServerlessV1Capacity::settle`], driven from the cluster record's own settle), exactly
//! as every status transition of this store is materialised: nothing sleeps, and a caller
//! reading the cluster straight after the call still finds it on the capacity it was on.

use super::*;

/// The `EngineMode` an Aurora Serverless v1 DB cluster runs in, as
/// `CreateDBClusterMessage$EngineMode` publishes it: the mode that "only applies for Aurora
/// Serverless v1 DB clusters".
pub const SERVERLESS_V1_ENGINE_MODE: &str = "serverless";

/// Whether a cluster running `engine` in `engine_mode` is an Aurora Serverless v1 DB cluster.
///
/// Serverless v1 is an AURORA capability ([`is_aurora`]) and it is the engine MODE that makes
/// an Aurora cluster one — "Aurora Serverless v2 DB clusters use the `provisioned` engine
/// mode" — so the mode alone decides it for an Aurora cluster, and no cluster of another
/// engine is one. The mode is compared case-insensitively, the way every other engine-mode
/// reading of this crate compares it.
pub fn is_serverless_v1_cluster(engine: &str, engine_mode: &str) -> bool {
    is_aurora(engine) && engine_mode.eq_ignore_ascii_case(SERVERLESS_V1_ENGINE_MODE)
}

/// What a call that acts on an Aurora Serverless v1 cluster's capacity refuses a cluster that
/// is not one with.
///
/// The refusal states which clusters can have their capacity modified at all, which is what
/// tells a caller that no amount of waiting will change the answer and that a `provisioned`
/// cluster's serverless capacity is rescaled through
/// `ModifyDBCluster$ServerlessV2ScalingConfiguration` instead. The sentence is FIXED: neither
/// the identifier the request named, nor the capacity it asked for, nor the cluster's engine
/// appears in it, so the text a caller matches on is the same for every cluster refused.
pub const CAPACITY_NOT_MODIFIABLE_MESSAGE: &str =
    "Only Aurora Serverless clusters can have their capacity modified.";

/// The capacity values an AURORA MYSQL Serverless v1 cluster accepts, as
/// `ModifyCurrentDBClusterCapacityMessage$Capacity` publishes them: "For Aurora MySQL, valid
/// capacity values are 1, 2, 4, 8, 16, 32, 64, 128, and 256."
pub const AURORA_MYSQL_SERVERLESS_V1_CAPACITIES: [i32; 9] = [1, 2, 4, 8, 16, 32, 64, 128, 256];

/// The capacity values an AURORA POSTGRESQL Serverless v1 cluster accepts, from the same
/// member: "For Aurora PostgreSQL, valid capacity values are 2, 4, 8, 16, 32, 64, 192, and
/// 384."
pub const AURORA_POSTGRESQL_SERVERLESS_V1_CAPACITIES: [i32; 8] = [2, 4, 8, 16, 32, 64, 192, 384];

/// The capacity values a Serverless v1 cluster of `engine` accepts: the list that engine
/// publishes, which differs between the two Aurora engines.
///
/// A PostgreSQL-compatible Aurora engine takes the PostgreSQL list
/// ([`POSTGRES_COMPATIBLE_ENGINE_SUFFIX`], the suffix every such engine's name ends with);
/// every other Aurora engine takes the MySQL one, which is the list the fault's own
/// documentation quotes.
pub fn serverless_v1_capacities(engine: &str) -> &'static [i32] {
    if engine
        .to_ascii_lowercase()
        .ends_with(POSTGRES_COMPATIBLE_ENGINE_SUFFIX)
    {
        return &AURORA_POSTGRESQL_SERVERLESS_V1_CAPACITIES;
    }
    &AURORA_MYSQL_SERVERLESS_V1_CAPACITIES
}

/// Refuse a `Capacity` that is none of the values the cluster's engine publishes, with the
/// sentence `InvalidDBClusterCapacityFault` is documented as: "`Capacity` isn't a valid
/// Aurora Serverless DB cluster capacity. Valid capacity values are 2, 4, 8, …, and 256."
///
/// The values the sentence lists are the ones that engine accepts
/// ([`serverless_v1_capacities`]) rather than one engine's list for both, written as the
/// documentation writes them: comma-separated with `and` before the last.
pub fn check_serverless_v1_capacity(engine: &str, capacity: i32) -> Result<(), String> {
    let accepted = serverless_v1_capacities(engine);
    if accepted.contains(&capacity) {
        return Ok(());
    }
    Err(format!(
        "Capacity isn't a valid Aurora Serverless DB cluster capacity. Valid capacity values \
         are {}.",
        comma_and(accepted)
    ))
}

/// `[2, 4, 8]` as `2, 4, and 8` — the way the capacity documentation lists the values, so a
/// list of any length reads as that sentence does.
fn comma_and(values: &[i32]) -> String {
    match values {
        [] => String::new(),
        [only] => only.to_string(),
        [first, second] => format!("{first} and {second}"),
        [leading @ .., last] => format!(
            "{}, and {last}",
            leading
                .iter()
                .map(i32::to_string)
                .collect::<Vec<String>>()
                .join(", ")
        ),
    }
}

/// How long `ModifyCurrentDBClusterCapacity` looks for a scaling point before the timeout
/// action is enforced, when the request names no `SecondsBeforeTimeout`: "The default is
/// 300."
pub const DEFAULT_SECONDS_BEFORE_TIMEOUT: i32 = 300;

/// The range that member accepts: "Specify a value between 10 and 600 seconds."
pub const SECONDS_BEFORE_TIMEOUT_RANGE: std::ops::RangeInclusive<i32> = 10..=600;

/// Refuse a `SecondsBeforeTimeout` outside the range the member publishes, naming the member,
/// the value rejected and the range it accepts — the form this front door words a numeric
/// member's own domain in.
pub fn check_seconds_before_timeout(seconds: i32) -> Result<(), String> {
    if SECONDS_BEFORE_TIMEOUT_RANGE.contains(&seconds) {
        return Ok(());
    }
    Err(format!(
        "invalid value {seconds} for SecondsBeforeTimeout: must be a value between {} and {} \
         seconds",
        SECONDS_BEFORE_TIMEOUT_RANGE.start(),
        SECONDS_BEFORE_TIMEOUT_RANGE.end()
    ))
}

/// The `TimeoutAction` values the operation publishes: "either `ForceApplyCapacityChange` or
/// `RollbackCapacityChange`", the first of which is the default — "`ForceApplyCapacityChange`,
/// the default, sets the capacity to the specified value as soon as possible".
pub const TIMEOUT_ACTIONS: [&str; 2] = ["ForceApplyCapacityChange", "RollbackCapacityChange"];

/// The action a request that names none asks for; see [`TIMEOUT_ACTIONS`].
pub const DEFAULT_TIMEOUT_ACTION: &str = TIMEOUT_ACTIONS[0];

/// Refuse a `TimeoutAction` outside the two values the member publishes, naming the member,
/// the value rejected and what it does accept.
pub fn check_timeout_action(timeout_action: &str) -> Result<(), String> {
    if TIMEOUT_ACTIONS.contains(&timeout_action) {
        return Ok(());
    }
    Err(format!(
        "invalid value {timeout_action} for TimeoutAction: must be one of {}",
        TIMEOUT_ACTIONS.join(" | ")
    ))
}

/// The capacity a scaling the clock still owes will take a cluster to, and when.
#[derive(Debug, Clone)]
pub struct PendingServerlessV1Capacity {
    /// `DBClusterCapacityInfo$PendingCapacity`: "the capacity that the DB cluster scales to
    /// next", in ACUs.
    pub capacity: i32,
    /// The instant the cluster is ON that capacity, stored rather than waited for.
    pub due_at: Instant,
}

/// The Aurora Serverless v1 capacity of one DB cluster: what it is running on, and the
/// scaling the clock still owes it.
#[derive(Debug, Clone)]
pub struct ServerlessV1Capacity {
    /// The capacity the cluster is running on now, in ACUs.
    current: i32,
    /// The scaling a call set in motion, until the clock makes it due.
    pending: Option<PendingServerlessV1Capacity>,
}

impl ServerlessV1Capacity {
    /// A cluster that is running on `capacity` with nothing owed.
    fn running_on(capacity: i32) -> Self {
        Self {
            current: capacity,
            pending: None,
        }
    }

    /// Apply the scaling if its due instant has passed. Called from the cluster record's own
    /// settle, so the capacity a report publishes is the one the clock has already taken the
    /// cluster to — never one a read moved.
    pub(super) fn settle(&mut self, now: Instant) {
        if self.pending.as_ref().is_some_and(|owed| now >= owed.due_at) {
            let owed = self.pending.take().expect("checked just above");
            self.current = owed.capacity;
        }
    }
}

impl DbClusterRecord {
    /// Whether this cluster is an Aurora Serverless v1 one; see
    /// [`is_serverless_v1_cluster`].
    pub fn is_serverless_v1(&self) -> bool {
        is_serverless_v1_cluster(&self.engine, &self.engine_mode)
    }

    /// `DBCluster$Capacity`: the capacity the cluster is running on, in ACUs, and `None` for
    /// a cluster that is not an Aurora Serverless v1 one — a provisioned cluster has no such
    /// capacity, so reporting a number for it would claim compute it does not run on.
    pub fn reported_capacity(&self) -> Option<i32> {
        if !self.is_serverless_v1() {
            return None;
        }
        Some(
            self.serverless_v1_capacity
                .as_ref()
                .map_or_else(|| self.serverless_v1_floor(), |held| held.current),
        )
    }

    /// `DBClusterCapacityInfo$PendingCapacity`: "the capacity that the DB cluster scales to
    /// next" — the capacity a scaling the clock still owes will take it to, and the capacity
    /// it is already on where nothing is owed. `None` for a cluster that is not a Serverless
    /// v1 one, as above.
    pub fn pending_capacity(&self) -> Option<i32> {
        let current = self.reported_capacity()?;
        Some(
            self.serverless_v1_capacity
                .as_ref()
                .and_then(|held| held.pending.as_ref())
                .map_or(current, |owed| owed.capacity),
        )
    }

    /// The capacity a Serverless v1 cluster runs on before any call has set one: the floor its
    /// own scaling configuration names (`ScalingConfigurationInfo$MinCapacity`, "the minimum
    /// capacity for an Aurora DB cluster in `serverless` DB engine mode"), and the smallest
    /// capacity its engine publishes where the cluster carries no scaling configuration at
    /// all — a running cluster holds at least the capacity it scales down to.
    fn serverless_v1_floor(&self) -> i32 {
        self.scaling_configuration
            .as_ref()
            .and_then(|configured| configured.min_capacity)
            .unwrap_or_else(|| {
                serverless_v1_capacities(&self.engine)
                    .first()
                    .copied()
                    // Every engine's published list carries at least one capacity; a list
                    // with none would leave the cluster running on no compute at all, which
                    // the smallest capacity any Aurora engine publishes stands in for.
                    .unwrap_or(1)
            })
    }

    /// Set the capacity of this Serverless v1 cluster to `capacity`, and answer the
    /// `(CurrentCapacity, PendingCapacity)` the call reports.
    ///
    /// The capacity the cluster is ON does not move here: "the capacity that the DB cluster
    /// scales to next" is what the request asked for, and the cluster reaches it when the
    /// clock makes the scaling due ([`ServerlessV1Capacity::settle`]), so a caller that reads
    /// the cluster straight afterwards still finds it on the capacity it was running on.
    pub fn scale_serverless_v1_capacity(&mut self, capacity: i32) -> (i32, i32) {
        let current = self
            .reported_capacity()
            .unwrap_or_else(|| self.serverless_v1_floor());
        let held = self
            .serverless_v1_capacity
            .get_or_insert_with(|| ServerlessV1Capacity::running_on(current));
        held.pending = Some(PendingServerlessV1Capacity {
            capacity,
            due_at: Instant::now() + STATUS_TRANSITION_DELAY,
        });
        (current, capacity)
    }
}

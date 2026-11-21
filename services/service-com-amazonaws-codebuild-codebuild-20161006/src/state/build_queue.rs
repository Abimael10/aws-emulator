// SPDX-License-Identifier: CC0-1.0

//! WHEN a build leaves the queue, what it occupies while it runs, and what it reports
//! while it is still running.
//!
//! # The rules this module implements
//!
//! A build does not start because it was asked for: it starts when a machine can run it.
//! A project whose environment names a compute fleet runs on that fleet's reserved
//! capacity, and `Fleet$baseCapacity` is "the initial number of machines allocated to the
//! compute fleet, which defines the number of builds that can run in parallel" — so a
//! build of such a project runs only while fewer builds of that fleet are running than the
//! fleet has machines. A build that finds every machine taken is an OVERFLOW build, and
//! what happens to it is the fleet's own setting (`Fleet$overflowBehavior`): with `QUEUE`
//! "your overflow builds need to wait on the existing fleet instance to become available",
//! so the build stays in its `QUEUED` phase — `buildStatus` `IN_PROGRESS` — until a
//! machine frees up, and with `ON_DEMAND` "your overflow builds run on CodeBuild
//! on-demand", so the build starts at once, off the fleet.
//!
//! A build does not wait forever: `Build$queuedTimeoutInMinutes` is "the number of minutes
//! a build is allowed to be queued before it times out". A build still waiting for a
//! machine when that window runs out is ENDED there — `buildComplete` true, `buildStatus`
//! `FAILED`, `currentPhase` `COMPLETED` — with the `QUEUED` phase itself carrying
//! `phaseStatus` `TIMED_OUT` (`StatusType`, documented on `BuildPhase$phaseStatus` as "the
//! build phase timed out"). The scalar `TIMED_OUT` is the EXECUTION timeout's outcome on
//! the phase that was running, which is [`super::buildspec`]'s to report; the queue window
//! fails the build. Which of the two a build gets is decided by what the pass that looks at
//! it FINDS: a build this pass can place leaves the queue, whatever its wait says and however
//! long ago it was started, and one it cannot place and whose wait has run out ends there.
//!
//! A build that got a machine occupies it for as long as it runs, and how long it runs is
//! how long its own commands take: the session performs them in process and spends none of
//! the host's clock on them, so the time they take is the time they STATE they take
//! ([`super::build_shell`], [`super::buildspec::BuildClock`]). That stated time is the
//! window the build REPORTS — `startTime`..`endTime`, which its phases partition — and it is
//! the window the machine is accounted as taken for: a one-machine fleet therefore runs one
//! build per such window, and the build behind it waits exactly as long as the record says
//! the build in front of it took.
//!
//! # The one span this emulator compresses
//!
//! A build's run is performed in process the instant the build is admitted, so its outcome
//! is known at once; what a client has to wait for is only the moment it may be READ. That
//! wait is compressed ([`BUILD_CLOCK_SCALE`]): until it is over the build reports the phases
//! it has reached and the one it is in, still `IN_PROGRESS` ([`ExecutingRun`]), which is what
//! makes the phase a running build sits in observable at all, and at the end of it the record
//! the run produced is what every reader sees. Nothing else is compressed: not the window the
//! build reports, not the capacity that window consumes, and not the wait a build is given in
//! the queue.
//!
//! The two views can therefore be read apart: a build whose commands stated minutes reports
//! itself finished, with the `endTime` those commands stated, while the machine it ran on is
//! still accounted as taken for the rest of that window — so a build behind it is still
//! `QUEUED`, and may run out of its own wait, behind a build that already reports as
//! finished. That is the cost of not making every client wait out a span the emulator has
//! already computed; what a client reads about any ONE build is the record that build's own
//! run produced.
//!
//! # Why a reporting operation drives it
//!
//! Nothing here sleeps: every instant is stored and compared against the clock, and the
//! transitions the clock owes are materialised by whoever reads the store next
//! ([`super::build_run::settle_due_builds`], which calls [`DueBuilds`]'s producer before any
//! reader takes the lock) — the same rule every other settle in this crate follows.

use super::*;

/// How far the wait before a running build's OUTCOME may be read is compressed against the
/// wall clock: one stated minute per second of real time.
///
/// The same factor this crate compresses a sandbox's documented idle wait by
/// ([`SANDBOX_IDLE_CLOCK_SCALE`]), and for the same reason: a build's commands may state any
/// time up to the longest execution timeout a build can be given
/// ([`super::build_shell::MAX_SLEEP_SECONDS`], 36 hours), and a client that reads a build it
/// started — and every test of one — would wait exactly that long for an outcome this
/// emulator already knows, because the run is performed in process the instant the build is
/// admitted.
///
/// What this scales is ONLY that wait. The window the build reports is the one its own
/// commands occupied, the machine it ran on is accounted as taken for that window
/// ([`occupancy_end_millis`]), and the wait a build is given in the queue is the documented
/// number of minutes it states.
pub const BUILD_CLOCK_SCALE: i64 = SANDBOX_IDLE_CLOCK_SCALE;

/// The longest a build's outcome stays unreadable, whatever its own commands state they
/// took.
///
/// THIS EMULATOR'S OWN CEILING, not an AWS limit, for the reason [`BUILD_CLOCK_SCALE`]
/// gives: past it the record the run already produced is what every reader sees. It is
/// deliberately not zero — a caller that polls a started build observes the phase it is in —
/// and it is inside the band this project fixes for a span a caller may have to wait on, so
/// no caller waits long for an outcome that is already known.
pub const MAX_BUILD_EXECUTION_MILLIS: i64 = 5_000;

/// The shortest wait in the queue a build can be given, from the model: a
/// `queuedTimeoutInMinutes` member targets `TimeOut`, which is `@range(min: 5, max: 480)`,
/// so no request this service accepts carries less.
pub const MIN_QUEUED_TIMEOUT_MINUTES: i32 = 5;

/// How long a build whose wait in the queue is `minutes` is held before that wait runs out.
///
/// The documented wait itself, unscaled: `Build$queuedTimeoutInMinutes` is "the number of
/// minutes a build is allowed to be queued before it times out", so a client that bounds its
/// queue wait with it gets the bound it asked for.
#[must_use]
pub fn queue_window_millis(minutes: i32) -> i64 {
    i64::from(minutes.max(0)).saturating_mul(MILLIS_PER_MINUTE)
}

/// The `FleetOverflowBehavior` value that makes an overflow build WAIT for a machine of the
/// fleet: "for overflow behavior QUEUE, your overflow builds need to wait on the existing
/// fleet instance to become available".
///
/// The other value, `ON_DEMAND`, runs the overflow build off the fleet ("your overflow
/// builds run on CodeBuild on-demand"), and so does a fleet that records no overflow
/// behaviour at all: holding a build is what `QUEUE` asks for, and the model documents no
/// default for the member.
pub const FLEET_OVERFLOW_QUEUE: &str = "QUEUE";

/// `BuildPhaseType` for the phase a build reports for having been submitted
/// (`smithy_query(query="shape",shape="BuildPhaseType")`).
const SUBMITTED_PHASE: &str = "SUBMITTED";

/// `BuildPhaseType` for the phase a build waits for a machine in — the phase its queue
/// window applies to.
const QUEUED_PHASE: &str = "QUEUED";

/// `BuildPhaseType` for the terminal marker phase a build that has ended reports last, and
/// the `currentPhase` of such a build.
const COMPLETED_PHASE: &str = "COMPLETED";

/// The `StatusType` a phase that ran out of time reports, documented on
/// `BuildPhase$phaseStatus` as "the build phase timed out" — what the `QUEUED` phase of a
/// build whose queue window ran out carries.
const TIMED_OUT_PHASE_STATUS: &str = "TIMED_OUT";

/// What one build's run came to, held while the build is still EXECUTING.
///
/// A build's run is performed in process at the instant the build is admitted onto a
/// machine, which answers the whole run at once; the build itself takes the time its
/// commands state they take, on the compressed clock this emulator runs a build's spans on
/// ([`BUILD_CLOCK_SCALE`]). This is that answer, waiting for the clock to reach the instant
/// the build gives its machine back — until then the build reports the phases it has reached
/// ([`reached_phases`]) and is still `IN_PROGRESS`.
#[derive(Debug, Clone)]
pub struct ExecutingRun {
    /// The record the build reports once it has finished running.
    pub finished: Build,
    /// Epoch millis the build began executing at, which is where its first running phase
    /// begins.
    pub started_millis: i64,
    /// Epoch millis the build's own phases say its last phase ended at — the instant the
    /// record itself reports, which is what the phases the build has reached are measured
    /// against.
    pub phases_end_millis: i64,
    /// Epoch millis the build stops occupying its machine at, which is the instant that
    /// record becomes the one every reader sees.
    pub ends_millis: i64,
}

impl ExecutingRun {
    /// How far through its own phases this build stands at `now_millis`: the instant on the
    /// build's OWN clock that the compressed span elapsed so far corresponds to.
    ///
    /// The build's phases partition `started_millis..phases_end_millis` and the emulator
    /// holds the machine for `started_millis..ends_millis`, so a reader that is a third of
    /// the way through the hold is a third of the way through the phases — which is what
    /// makes the phase a build reports it is in move through them in order rather than
    /// jumping from the first to the record.
    fn reported_millis(&self, now_millis: i64) -> i64 {
        let held = self.ends_millis.saturating_sub(self.started_millis);
        let elapsed = now_millis.saturating_sub(self.started_millis).clamp(0, held.max(0));
        let stated = self.phases_end_millis.saturating_sub(self.started_millis);
        if held <= 0 {
            return self.phases_end_millis;
        }
        self.started_millis
            .saturating_add(i64::try_from(i128::from(stated) * i128::from(elapsed) / i128::from(held)).unwrap_or(stated))
    }
}

/// What the clock owes this store's builds at one instant: the runs it has just admitted
/// onto a machine, and the builds it has just ENDED.
///
/// Produced under the store's lock
/// ([`ComAmazonawsCodebuildCodebuild20161006State::due_builds`]) and acted on outside it:
/// a run reaches another service, and so does the metering of a build that ended.
#[derive(Debug, Default)]
pub struct DueBuilds {
    /// The builds admitted onto a machine, for the operation that is about to perform
    /// their runs ([`super::build_run::settle_due_builds`]).
    pub runs: Vec<build_run::ClaimedRun>,
    /// The builds this pass ended — a run whose last phase's instant has arrived, or a
    /// build whose queue window ran out — for the metrics CodeBuild publishes about a build
    /// that ended ([`super::build_metrics`]).
    pub completed: Vec<CompletedBuild>,
}

/// Epoch millis the machine a build ran on stops being accounted as taken by it: the instant
/// the build's own record says it ENDED, which is the end of the window its commands
/// occupied — `None` while the build has not ended at all, which is a machine still taken.
///
/// The window a build reports is the window it ran for (`Build$startTime`..`Build$endTime`,
/// which its phases partition), so that window is what its machine was busy for. A record
/// that ended and states no instant for it ended where it started, so it holds nothing.
fn occupancy_end_millis(stored: &StoredBuild) -> Option<i64> {
    if !stored.build.build_complete {
        return None;
    }
    Some(
        stored
            .build
            .end_time
            .as_ref()
            .and_then(|at| at.to_millis().ok())
            .unwrap_or(stored.start_millis),
    )
}

/// Whether `stored` is accounted as holding a machine of its fleet at `now_millis`: a build
/// that was admitted onto one of them — rather than onto on-demand overflow capacity, which
/// takes no machine of the fleet — and whose own window has not closed yet
/// ([`occupancy_end_millis`]).
///
/// A build is therefore admitted onto a machine only once the window of the build that had it
/// has closed, which is what keeps the windows of a one-machine fleet's builds from
/// overlapping.
fn occupies_machine(stored: &StoredBuild, now_millis: i64) -> bool {
    stored.on_fleet_machine && occupancy_end_millis(stored).is_none_or(|end| end > now_millis)
}

/// The compute fleet `build` runs on, as the ARN its own environment names — `None` for a
/// build that runs on CodeBuild's on-demand capacity.
///
/// Read off the build's record rather than off its project: `StartBuild`'s `fleetOverride`
/// "overrides the one defined in the build project", so the fleet a build runs on is the
/// one its own `environment.fleet` states (`ProjectEnvironment$fleet` ->
/// `ProjectFleet$fleetArn`).
pub fn build_fleet_arn(build: &Build) -> Option<&str> {
    build
        .environment
        .as_ref()?
        .fleet
        .as_ref()?
        .fleet_arn
        .as_deref()
        .filter(|arn| !arn.is_empty())
}

/// How many builds of `fleet` may run at the same time — `None` for a fleet that states no
/// capacity at all, which constrains nothing.
///
/// `Fleet$baseCapacity` is "the initial number of machines allocated to the compute fleet,
/// which defines the number of builds that can run in parallel", and a fleet that
/// auto-scales grows past it up to `ScalingConfigurationOutput$maxCapacity`, "the maximum
/// number of instances in the fleet when auto-scaling" — so the ceiling is the larger of
/// the two, and never below the one machine a fleet needs to run anything
/// ([`MIN_FLEET_CAPACITY`]).
fn fleet_build_capacity(fleet: &Fleet) -> Option<i64> {
    let base = fleet.base_capacity?;
    let scaled = fleet
        .scaling_configuration
        .as_ref()
        .and_then(|scaling| scaling.max_capacity)
        .unwrap_or(base);
    Some(i64::from(base.max(scaled).max(MIN_FLEET_CAPACITY)))
}

/// The stored fleet `arn` names, or `None` when this account holds no fleet with that ARN.
///
/// Resolved through the one resolver every fleet-addressing operation uses
/// ([`fleet_key_in`]), so an ARN for another partition, Region, account, resource type or
/// fleet id names no fleet of this account.
fn fleet_for_arn<'a>(
    fleets: &'a BTreeMap<String, StoredFleet>,
    arn: &str,
) -> Option<&'a StoredFleet> {
    fleets.get(&fleet_key_in(fleets, arn)?)
}

/// The queue window `build` was given, in minutes: the value its own record states —
/// `StartBuild`'s `queuedTimeoutInMinutesOverride` applied over the project's
/// `queuedTimeoutInMinutes` — and the service default for a record that states none, which
/// is the value `CreateProject` stores when the caller omits one
/// ([`DEFAULT_QUEUED_TIMEOUT_MINUTES`]).
///
/// Never below the shortest wait the model lets a caller ask for
/// ([`MIN_QUEUED_TIMEOUT_MINUTES`]): a record carrying less than that was not written by a
/// request this service accepted, and a wait of no minutes at all would end a build in the
/// queue the moment it joined it.
fn queued_timeout_minutes(build: &Build) -> i32 {
    build
        .queued_timeout_in_minutes
        .unwrap_or(DEFAULT_QUEUED_TIMEOUT_MINUTES)
        .max(MIN_QUEUED_TIMEOUT_MINUTES)
}

/// Epoch millis the wait in the queue of a build started at `start_millis` with the wait
/// `minutes` runs out at.
///
/// The window is counted from the instant the build was submitted, which is where its
/// `QUEUED` phase begins: `Build$queuedTimeoutInMinutes` is "the number of minutes a build
/// is allowed to be queued before it times out", so the whole of that phase is the wait. How
/// long the emulator lets it run for is [`queue_window_millis`].
fn queue_deadline_millis(start_millis: i64, minutes: i32) -> i64 {
    start_millis.saturating_add(queue_window_millis(minutes))
}

/// The phases a build whose QUEUE window ran out reports: it was submitted, it waited for a
/// machine until the window closed, and it ended there.
///
/// The `QUEUED` phase is the one that ran out of time, so it is the phase carrying
/// `TIMED_OUT` and the context that says what happened; `COMPLETED` is the terminal marker
/// every ended build reports last, which has no outcome of its own.
fn queue_timed_out_phases(start_millis: i64, deadline_millis: i64, minutes: i32) -> Vec<BuildPhase> {
    vec![
        finished_phase(
            SUBMITTED_PHASE,
            BUILD_STATUS_SUCCEEDED,
            start_millis,
            start_millis,
            None,
        ),
        finished_phase(
            QUEUED_PHASE,
            TIMED_OUT_PHASE_STATUS,
            start_millis,
            deadline_millis,
            Some(vec![PhaseContext {
                status_code: None,
                message: Some(queue_timed_out_message(minutes)),
            }]),
        ),
        open_phase(COMPLETED_PHASE, deadline_millis),
    ]
}

/// What the `QUEUED` phase of a build whose queue window ran out reports as the reason: no
/// machine took the build inside the wait it was given, so CodeBuild ended it.
///
/// `minutes` is that wait, which is the one variable part of the sentence
/// (`Build$queuedTimeoutInMinutes`, "the number of minutes a build is allowed to be queued
/// before it times out"). AWS publishes no sentence for the condition, so this states it in
/// this service's own terms, and classifies it with no status code: the phase's own
/// `TIMED_OUT` status is what says which condition it was.
fn queue_timed_out_message(minutes: i32) -> String {
    format!(
        "The build was not started within the queue timeout of {minutes} minutes, so AWS \
         CodeBuild ended it"
    )
}

/// The phases of a build that is still running: the ones whose window has CLOSED by
/// `now_millis`, and then the one the build is IN, which carries no outcome yet — the way
/// CodeBuild reports the phase a build currently sits in.
///
/// `phases` are the windows the run answered, in the order the build reached them, so this
/// is a prefix of them: a phase whose window has not opened yet is one the build has not
/// reached, and nothing is reported for it.
fn reached_phases(phases: &[BuildPhase], now_millis: i64) -> Vec<BuildPhase> {
    let mut reached = Vec::new();
    for phase in phases {
        let Some(started) = phase.start_time.as_ref().and_then(|at| at.to_millis().ok()) else {
            break;
        };
        if started > now_millis {
            break;
        }
        let ended = phase.end_time.as_ref().and_then(|at| at.to_millis().ok());
        if ended.is_some_and(|ended| ended <= now_millis) {
            reached.push(phase.clone());
            continue;
        }
        if let Some(phase_type) = phase.phase_type.as_deref() {
            reached.push(open_phase(phase_type, started));
        }
        break;
    }
    reached
}

impl ComAmazonawsCodebuildCodebuild20161006State {
    /// Read a build as it stands.
    ///
    /// It makes no write: the transition out of `IN_PROGRESS` is the RUN of the build's
    /// buildspec, which is performed by the operation that reads it
    /// ([`build_run::settle_due_builds`], called before this one takes the lock) rather
    /// than here — a run reads a caller-supplied specification and puts the build's output
    /// in the object store, and neither can happen inside a critical section every other
    /// request of this service shares.
    pub fn settled_build(&mut self, id: &str, now_millis: i64) -> Option<&StoredBuild> {
        let _ = now_millis;
        self.builds.get(id)
    }

    /// What the clock owes this store's builds at `now_millis`: materialise where every
    /// running build now stands, admit the builds a machine is free for, and end the ones
    /// whose queue window ran out.
    ///
    /// The three steps are one pass under one lock, in that order, because each decides the
    /// next: a build whose run has finished frees its machine, which is what the admission
    /// of the build behind it turns on, and a build the admission could not place is the
    /// only kind of build a queue window can run out on. A build is marked as admitted
    /// under this same lock, so a run is performed once however many calls arrive together:
    /// the others see a build that is still `IN_PROGRESS`, which is what it is until its
    /// run has happened.
    pub fn due_builds(&mut self, now_millis: i64) -> DueBuilds {
        // Which caches may be restored is a question about the PROJECT — a cache reset
        // after the last save discarded it — and it is answered before the builds are
        // taken mutably.
        let cache_restorable: BTreeMap<String, bool> = self
            .projects
            .iter()
            .map(|(name, stored)| {
                let restorable = match (stored.cache_invalidated_millis, stored.cache_saved_millis)
                {
                    (Some(reset), Some(saved)) => saved >= reset,
                    (Some(_), None) => false,
                    _ => true,
                };
                (name.clone(), restorable)
            })
            .collect();
        let Self { builds, fleets, .. } = self;
        let mut due = DueBuilds::default();

        // Where every running build now stands: one whose outcome has become readable
        // reports the record its run produced, and one still running reports the phase it is
        // in.
        for (id, stored) in builds.iter_mut() {
            let project_name = build_project_name(id, &stored.build);
            if let Some(completed) = stored.materialise_execution(now_millis, &project_name) {
                due.completed.push(completed);
            }
        }

        // The machines of each fleet that are taken: one per build of that fleet whose own
        // window has not closed ([`occupies_machine`]). A build admitted as OVERFLOW runs on
        // CodeBuild's on-demand capacity rather than on the fleet, so it takes no machine of
        // it.
        let mut taken: BTreeMap<String, i64> = BTreeMap::new();
        for stored in builds.values() {
            if !occupies_machine(stored, now_millis) {
                continue;
            }
            if let Some(arn) = build_fleet_arn(&stored.build) {
                *taken.entry(arn.to_string()).or_insert(0) += 1;
            }
        }

        // The builds waiting for capacity, oldest first: a queue is served in the order
        // the builds joined it, so the build that has waited longest is placed first.
        let mut waiting: Vec<(i64, String)> = builds
            .iter()
            .filter(|(_, stored)| {
                !stored.build.build_complete
                    && !stored.run_claimed
                    && now_millis >= stored.settle_due_millis
            })
            .map(|(id, stored)| (stored.start_millis, id.clone()))
            .collect();
        waiting.sort();

        for (_, id) in waiting {
            let Some(stored) = builds.get(&id) else {
                continue;
            };
            let fleet_arn = build_fleet_arn(&stored.build).map(str::to_string);
            let placement = place_on_fleet(fleets, &mut taken, fleet_arn.as_deref());
            let Some(stored) = builds.get_mut(&id) else {
                continue;
            };
            let project_name = build_project_name(&id, &stored.build);
            if placement == Placement::Held {
                // No machine of the fleet took this build and the fleet asked for its
                // overflow builds to wait, so the build is still in its `QUEUED` phase —
                // until the wait it was given runs out.
                if now_millis >= stored.queue_deadline_millis() {
                    due.completed.push(stored.end_in_queue(&project_name));
                }
                continue;
            }
            stored.run_claimed = true;
            // Which capacity took it: a machine of its own fleet is one fewer that fleet has
            // free until this build's window closes, and an on-demand placement takes none.
            stored.on_fleet_machine = placement == Placement::FleetMachine;
            due.runs.push(build_run::ClaimedRun {
                id: id.clone(),
                build: stored.build.clone(),
                project_name: project_name.clone(),
                artifacts: stored.artifacts.clone(),
                secondary_artifacts: stored.secondary_artifacts.clone(),
                start_millis: stored.start_millis,
                // The instant the build left the queue, which is where it begins
                // EXECUTING: this pass placed it, so this is where its wait for capacity
                // ended — however long that wait was.
                due_millis: now_millis.max(stored.settle_due_millis),
                cache_restorable: cache_restorable
                    .get(&project_name)
                    .copied()
                    .unwrap_or(true),
            });
        }
        due
    }

    /// Record what one claimed run answered ([`StoredBuild::record_run`]), the report runs it
    /// produced ([`build_reports::record_build_reports`]), and the instant the cache it saved
    /// was written at, which is what a later build restores against the project's last cache
    /// reset.
    ///
    /// Answers the build this call COMPLETED, for the metrics CodeBuild publishes about a
    /// build that ended ([`build_metrics`]) — `None` for a build that is still running the
    /// time its own commands state they take, and for one whose record had already reached
    /// its outcome, so one build is metered once however many runs are recorded for it.
    pub fn record_build_run(
        &mut self,
        claimed: &build_run::ClaimedRun,
        region: &str,
        recorded: build_run::RecordedRun,
    ) -> Option<CompletedBuild> {
        // The reports the build produced go in first: they are what its own commands wrote,
        // read in the format its buildspec declared, and the build REPORTS the ARN of each
        // one that was written ([`Build::report_arns`]).
        let report_arns =
            build_reports::record_build_reports(self, claimed, region, &recorded.reports);
        // The instant the build's last phase ended at, which is the instant the build ended
        // at ([`buildspec::BuildRun::ended_millis`]) — read before the run is recorded, and
        // what the metrics this build is metered by are stamped with.
        let ended_millis = recorded.run.ended_millis;
        let mut completed = None;
        if let Some(stored) = self.builds.get_mut(&claimed.id) {
            let was_complete = stored.build.build_complete;
            stored.record_run(
                recorded.run,
                recorded.artifacts,
                &recorded.secondary_artifacts,
                report_arns,
                claimed.due_millis,
                now_millis(),
            );
            if !was_complete && stored.build.build_complete {
                completed = Some(stored.completed_build(&claimed.project_name, ended_millis));
            }
        }
        if let Some(saved) = recorded.cache_saved_millis {
            if let Some(project) = self.projects.get_mut(&claimed.project_name) {
                project.cache_saved_millis = Some(saved);
            }
        }
        completed
    }
}

/// Where a build waiting for a machine can be placed now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// A machine of the build's own fleet took it, which is now one fewer machine the fleet
    /// has free.
    FleetMachine,
    /// It runs on CodeBuild's on-demand capacity, which this emulator does not ration: the
    /// build names no fleet, names one this account holds no record of or one that states no
    /// capacity, or is an OVERFLOW build of a fleet that runs its overflow on demand. It
    /// takes no machine of any fleet.
    OnDemand,
    /// Every machine of its fleet is taken and the fleet asked for its overflow builds to
    /// wait for one ([`FLEET_OVERFLOW_QUEUE`]), so the build stays in the queue.
    Held,
}

/// Where a build pinned to `fleet_arn` can be placed now, taking the machine it runs on when
/// that is one of the fleet's.
///
/// A build that names no fleet runs on CodeBuild's on-demand capacity, which this emulator
/// does not ration, and so does a build whose fleet this account holds no record of or whose
/// fleet states no capacity — there is nothing to account against. A build of a fleet whose
/// machines are all taken is an OVERFLOW build: it runs on on-demand capacity unless the
/// fleet asked for its overflow builds to wait.
fn place_on_fleet(
    fleets: &BTreeMap<String, StoredFleet>,
    taken: &mut BTreeMap<String, i64>,
    fleet_arn: Option<&str>,
) -> Placement {
    let Some(arn) = fleet_arn else {
        return Placement::OnDemand;
    };
    let Some(stored) = fleet_for_arn(fleets, arn) else {
        return Placement::OnDemand;
    };
    let Some(capacity) = fleet_build_capacity(&stored.fleet) else {
        return Placement::OnDemand;
    };
    let used = taken.get(arn).copied().unwrap_or(0);
    if used < capacity {
        taken.insert(arn.to_string(), used + 1);
        return Placement::FleetMachine;
    }
    if stored.fleet.overflow_behavior.as_deref() == Some(FLEET_OVERFLOW_QUEUE) {
        Placement::Held
    } else {
        Placement::OnDemand
    }
}

/// The project a build belongs to: the one its record states, or the one the id the store
/// keys it by carries — a build is keyed by `<projectName>:<uuid>`, so the project is known
/// from the key even for a record that states none.
fn build_project_name(id: &str, build: &Build) -> String {
    build
        .project_name
        .clone()
        .or_else(|| project_name_from_resource_id(id).map(str::to_string))
        .unwrap_or_default()
}

impl StoredBuild {
    /// Record what running this build answered: the status and phases it ended with, the
    /// variables it exported, and what it reports for the artifacts it put at their
    /// destinations.
    ///
    /// The ONE place a finished build's record is written. A build's run is performed by
    /// the operation that read it ([`build_run::settle_due_builds`]) and never under this
    /// store's lock: the run reads a caller-supplied buildspec and puts the build's output
    /// in the object store, and neither belongs in a critical section every other request
    /// of this service shares.
    ///
    /// The run answers at once and the BUILD takes the time its own commands state they
    /// take, so that record becomes the one readers see at the instant this emulator holds
    /// the build's machine until ([`ExecutingRun`]); a build whose commands stated no time at
    /// all has already reached it. `admitted_millis` is the instant the build began
    /// executing, which that instant is counted from on the compressed clock a build's spans
    /// run on ([`BUILD_CLOCK_SCALE`]), capped at [`MAX_BUILD_EXECUTION_MILLIS`].
    ///
    /// Idempotent: a build that is already complete keeps the outcome it reached, so a run
    /// recorded for a build something else has meanwhile finished — a `StopBuild` that
    /// stopped it — changes nothing.
    pub fn record_run(
        &mut self,
        run: BuildRun,
        artifacts: Option<BuildArtifacts>,
        secondary_artifacts: &[(String, BuildArtifacts)],
        report_arns: Vec<String>,
        admitted_millis: i64,
        now_millis: i64,
    ) {
        if self.build.build_complete {
            return;
        }
        let phases_end_millis = run.ended_millis.max(admitted_millis);
        let ends_millis = admitted_millis.saturating_add(
            (phases_end_millis.saturating_sub(admitted_millis) / BUILD_CLOCK_SCALE)
                .min(MAX_BUILD_EXECUTION_MILLIS),
        );
        let mut finished = self.build.clone();
        record_finished_build(
            &mut finished,
            run,
            artifacts,
            secondary_artifacts,
            report_arns,
        );
        if now_millis >= ends_millis {
            self.build = finished;
            return;
        }
        self.executing = Some(ExecutingRun {
            finished,
            started_millis: admitted_millis,
            phases_end_millis,
            ends_millis,
        });
        self.report_reached_phases(now_millis);
    }

    /// Materialise where this build's run now stands: the record the run produced once the
    /// instant its last phase ends at has arrived, and the phases it has reached until
    /// then.
    ///
    /// Answers the build this call ENDED, for the metrics a build that ended is metered by
    /// — `None` for a build that is still running, one that is not running at all, and one
    /// something else has already ended (a `StopBuild`, whose outcome stands: the run it
    /// was holding is dropped rather than overwriting it).
    fn materialise_execution(
        &mut self,
        now_millis: i64,
        project_name: &str,
    ) -> Option<CompletedBuild> {
        if self.build.build_complete {
            self.executing = None;
            return None;
        }
        let ends_millis = self.executing.as_ref()?.ends_millis;
        if now_millis < ends_millis {
            self.report_reached_phases(now_millis);
            return None;
        }
        let finished = self.executing.take()?.finished;
        self.build = finished;
        // The instant the record itself reports the build ended at, which is the window the
        // metrics this build is metered by cover.
        let end_millis = self
            .build
            .end_time
            .as_ref()
            .and_then(|at| at.to_millis().ok())
            .unwrap_or(ends_millis);
        Some(self.completed_build(project_name, end_millis))
    }

    /// Report the phases this running build has reached ([`reached_phases`]) and the phase
    /// it is in, leaving everything else about the record as it stands: the build is still
    /// `IN_PROGRESS` and has not ended.
    fn report_reached_phases(&mut self, now_millis: i64) {
        let Some(executing) = self.executing.as_ref() else {
            return;
        };
        let phases = reached_phases(
            executing.finished.phases.as_deref().unwrap_or_default(),
            // Measured on the build's OWN clock, which is where its phase windows lie.
            executing.reported_millis(now_millis),
        );
        if phases.is_empty() {
            return;
        }
        self.build.current_phase = phases
            .last()
            .and_then(|phase| phase.phase_type.clone())
            .or_else(|| self.build.current_phase.clone());
        self.build.phases = Some(phases);
    }

    /// Epoch millis the wait this build was given in the queue runs out at.
    ///
    /// Its own `queuedTimeoutInMinutes`, counted from the instant it was submitted — the
    /// documented minutes themselves, which is the one span of a build's life this emulator
    /// does not compress ([`queue_window_millis`]).
    fn queue_deadline_millis(&self) -> i64 {
        queue_deadline_millis(self.start_millis, queued_timeout_minutes(&self.build))
    }

    /// End this build because the wait it was given in the queue ran out: `buildComplete`
    /// true, `buildStatus` `FAILED` and `currentPhase` `COMPLETED`, with the `QUEUED` phase
    /// carrying `TIMED_OUT` ([`queue_timed_out_phases`]).
    ///
    /// Asked only of a build that was still waiting for a machine when its wait ran out,
    /// which is the only kind of build the wait can run out on: a build a machine was
    /// available for inside its wait started instead. Answers the build this ended, for the
    /// metrics it is metered by.
    fn end_in_queue(&mut self, project_name: &str) -> CompletedBuild {
        let minutes = queued_timeout_minutes(&self.build);
        let deadline_millis = self.queue_deadline_millis();
        self.build.build_status = Some(BUILD_STATUS_FAILED.to_string());
        self.build.current_phase = Some(COMPLETED_PHASE.to_string());
        self.build.build_complete = true;
        // The build ended WHERE THE WAIT RAN OUT, not when a call noticed: the wait it was
        // given is what ran out.
        self.build.end_time = Some(datetime(deadline_millis));
        self.build.phases = Some(queue_timed_out_phases(
            self.start_millis,
            deadline_millis,
            minutes,
        ));
        self.completed_build(project_name, deadline_millis)
    }

    /// What this build, now that it has ENDED, is metered by ([`build_metrics`]): the
    /// project its series are dimensioned on, the status it ended in, and the instants it
    /// started and ended at.
    ///
    /// `project_name` is the project the build belongs to, as the caller resolved it — off
    /// the record, or off the id the store keys the build by. `end_millis` is the instant
    /// the call that ended the build recorded as its `endTime`, so the duration published
    /// is the span the record itself reports.
    #[must_use]
    pub fn completed_build(&self, project_name: &str, end_millis: i64) -> CompletedBuild {
        CompletedBuild {
            project_name: project_name.to_string(),
            status: self.build.build_status.clone().unwrap_or_default(),
            start_millis: self.start_millis,
            end_millis,
        }
    }
}

/// Write what a finished run answered onto `build`: the status and phases it ended with,
/// the variables it exported, and what it reports for the artifacts it put at their
/// destinations.
///
/// The record a build reports once it has finished running, whether that is the instant the
/// run was performed or the later instant its own commands stated it ends at
/// ([`StoredBuild::record_run`]).
fn record_finished_build(
    build: &mut Build,
    run: BuildRun,
    artifacts: Option<BuildArtifacts>,
    secondary_artifacts: &[(String, BuildArtifacts)],
    report_arns: Vec<String>,
) {
    // "An array of the ARNs associated with this build's reports": the runs this build
    // minted for the groups its buildspec declared
    // ([`build_reports::record_build_reports`]). A build that declared none, or whose
    // declarations minted nothing, is associated with no report and states none.
    if !report_arns.is_empty() {
        build.report_arns = Some(report_arns);
    }
    build.build_status = Some(run.status);
    build.current_phase = Some(COMPLETED_PHASE.to_string());
    build.build_complete = true;
    // The build ENDED where its last phase ended: the phases a build ran occupy the time
    // their own commands took ([`buildspec::BuildClock`]), so the build's own window is
    // the one every phase window it reports lies inside.
    build.end_time = Some(datetime(run.ended_millis));
    build.phases = Some(run.phases);
    build.exported_environment_variables = run.exported_variables;
    // Once the environment has been provisioned, the build reports the environment it
    // RAN IN rather than only the one it was started with: CodeBuild starts a build
    // container with its own entrypoint — the agent that runs the buildspec — rather
    // than the entrypoint of the image, so a build that got an environment reports
    // that. A build whose provisioning failed never had one, so it reports none.
    if run.environment_provisioned {
        if let Some(environment) = build.environment.as_mut() {
            environment.custom_entrypoint = Some(true);
        }
    }
    if let Some(artifacts) = artifacts {
        build.artifacts = Some(artifacts);
    }
    for (identifier, uploaded) in secondary_artifacts {
        for reported in build.secondary_artifacts.iter_mut().flatten() {
            if reported.artifact_identifier.as_deref() == Some(identifier.as_str()) {
                *reported = uploaded.clone();
            }
        }
    }
}

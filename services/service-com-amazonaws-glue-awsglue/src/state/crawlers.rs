// SPDX-License-Identifier: CC0-1.0

//! The stored crawler record: what a create writes, what an update merges over it, and the
//! state a write leaves the crawler's schedule in.

use super::*;

/// The schedule state a write that STATES a crawler's schedule leaves it in: a cron
/// expression the write puts on the crawler is in force the moment the write returns, and
/// a crawler left with no expression has no schedule running for it at all.
///
/// One place, so `CreateCrawler`, `UpdateCrawler` and `UpdateCrawlerSchedule` cannot
/// disagree about the state the schedule they wrote is in.
pub(super) fn stated_schedule_state(expression: Option<&str>) -> String {
    if expression.is_some() {
        SCHEDULE_STATE_SCHEDULED.to_string()
    } else {
        SCHEDULE_STATE_NOT_SCHEDULED.to_string()
    }
}

/// `RecrawlPolicy$RecrawlBehavior` a crawler whose write stated no policy carries: of the
/// modelled `RecrawlBehavior` (`CRAWL_EVERYTHING | CRAWL_NEW_FOLDERS_ONLY |
/// CRAWL_EVENT_MODE`), the one that crawls "the entire dataset again", which is what a
/// crawler with no policy of its own does.
pub const RECRAWL_BEHAVIOR_CRAWL_EVERYTHING: &str = "CRAWL_EVERYTHING";

/// `LineageConfiguration$CrawlerLineageSettings` a crawler whose write stated no
/// configuration carries: of the modelled `CrawlerLineageSettings` (`ENABLE | DISABLE`), the
/// one that leaves data lineage OFF, which is where a crawler nothing enabled it for sits.
pub const CRAWLER_LINEAGE_SETTINGS_DISABLE: &str = "DISABLE";

/// `LakeFormationConfiguration$AccountId` a crawler whose write named none carries:
/// "required for cross account crawls. For same account crawls as the target data, this can
/// be left as null" — a crawler that names no other account crawls its own, which the
/// configuration states as the empty account rather than as a member that is not there.
pub const LAKE_FORMATION_SAME_ACCOUNT: &str = "";

/// The `SchemaChangePolicy` a crawler REPORTS, from the one its write stated: every member
/// the write left unset filled in with the behaviour the service applies to a crawler that
/// states none ([`UPDATE_BEHAVIOR_UPDATE_IN_DATABASE`],
/// [`DELETE_BEHAVIOR_DEPRECATE_IN_DATABASE`]) — the very behaviours a crawl of such a
/// crawler follows ([`CrawlPlan::of`]), so what a read reports and what a crawl does cannot
/// disagree.
pub fn stated_schema_change_policy(
    stated: Option<&SchemaChangePolicy>,
) -> SchemaChangePolicy {
    SchemaChangePolicy {
        update_behavior: Some(
            stated
                .and_then(|policy| policy.update_behavior.clone())
                .unwrap_or_else(|| UPDATE_BEHAVIOR_UPDATE_IN_DATABASE.to_string()),
        ),
        delete_behavior: Some(
            stated
                .and_then(|policy| policy.delete_behavior.clone())
                .unwrap_or_else(|| DELETE_BEHAVIOR_DEPRECATE_IN_DATABASE.to_string()),
        ),
    }
}

/// The `RecrawlPolicy` a crawler REPORTS, from the one its write stated: the behaviour the
/// write named, else [`RECRAWL_BEHAVIOR_CRAWL_EVERYTHING`].
fn stated_recrawl_policy(stated: Option<&RecrawlPolicy>) -> RecrawlPolicy {
    RecrawlPolicy {
        recrawl_behavior: Some(
            stated
                .and_then(|policy| policy.recrawl_behavior.clone())
                .unwrap_or_else(|| RECRAWL_BEHAVIOR_CRAWL_EVERYTHING.to_string()),
        ),
    }
}

/// The `LineageConfiguration` a crawler REPORTS, from the one its write stated: the setting
/// the write named, else [`CRAWLER_LINEAGE_SETTINGS_DISABLE`].
fn stated_lineage_configuration(stated: Option<&LineageConfiguration>) -> LineageConfiguration {
    LineageConfiguration {
        crawler_lineage_settings: Some(
            stated
                .and_then(|configuration| configuration.crawler_lineage_settings.clone())
                .unwrap_or_else(|| CRAWLER_LINEAGE_SETTINGS_DISABLE.to_string()),
        ),
    }
}

/// The `LakeFormationConfiguration` a crawler REPORTS, from the one its write stated: the
/// members the write named, else the configuration of a crawler that uses its own IAM role's
/// credentials on its own account's data — "specifies whether to use Lake Formation
/// credentials for the crawler instead of the IAM role credentials" is off, and the account
/// is [`LAKE_FORMATION_SAME_ACCOUNT`].
fn stated_lake_formation_configuration(
    stated: Option<&LakeFormationConfiguration>,
) -> LakeFormationConfiguration {
    LakeFormationConfiguration {
        use_lake_formation_credentials: Some(
            stated
                .and_then(|configuration| configuration.use_lake_formation_credentials)
                .unwrap_or(false),
        ),
        account_id: Some(
            stated
                .and_then(|configuration| configuration.account_id.clone())
                .unwrap_or_else(|| LAKE_FORMATION_SAME_ACCOUNT.to_string()),
        ),
    }
}

/// The `CrawlerTargets` a crawler REPORTS, from the ones its write stated: every target list
/// the write did not name reported as the EMPTY list rather than as a member that is not
/// there, and every Amazon S3 target's `Exclusions` likewise — a crawler that names no
/// exclusion crawls everything under its path, which is the empty exclusion list, not an
/// unstated one.
///
/// The set of lists is the modelled `CrawlerTargets` member list in full
/// (`smithy_query shape=CrawlerTargets`), so a list the model adds later is reported the
/// moment this crate's shape carries it.
fn stated_targets(stated: &CrawlerTargets) -> CrawlerTargets {
    CrawlerTargets {
        s3_targets: Some(
            stated
                .s3_targets
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|target| S3Target {
                    exclusions: Some(target.exclusions.unwrap_or_default()),
                    ..target
                })
                .collect(),
        ),
        jdbc_targets: Some(stated.jdbc_targets.clone().unwrap_or_default()),
        mongo_db_targets: Some(stated.mongo_db_targets.clone().unwrap_or_default()),
        dynamo_db_targets: Some(stated.dynamo_db_targets.clone().unwrap_or_default()),
        catalog_targets: Some(stated.catalog_targets.clone().unwrap_or_default()),
        delta_targets: Some(stated.delta_targets.clone().unwrap_or_default()),
        iceberg_targets: Some(stated.iceberg_targets.clone().unwrap_or_default()),
        hudi_targets: Some(stated.hudi_targets.clone().unwrap_or_default()),
    }
}

/// A stored crawler.
///
/// Every member of the modelled `Crawler` shape that a create can fill is held here, so
/// a later `GetCrawler` reports what the create stored and nothing else. The members only
/// a CRAWL produces (`LastCrawl`, `CrawlElapsedTime`) are held here too, written by the
/// crawl that produced them: a crawler nothing has crawled reports neither.
///
/// The sub-structures of a crawler's definition that the service DEFAULTS — its policies,
/// its configurations and its target lists — are held exactly as the write stated them,
/// including unstated; the value each of them is IN FORCE with is what a read reports
/// ([`stated_schema_change_policy`] and the three beside it), which is the same value a
/// crawl of the crawler follows.
#[derive(Debug, Clone)]
pub struct CrawlerEntry {
    /// The crawler's name, which is also its key in [`ComAmazonawsGlueAwsglueState::crawlers`].
    pub name: String,
    pub role: String,
    pub targets: CrawlerTargets,
    pub database_name: Option<String>,
    pub description: Option<String>,
    /// The custom classifiers associated with the crawler; empty when the create named
    /// none, which is the accurate report for a crawler that has no custom classifier.
    pub classifiers: Vec<String>,
    pub table_prefix: Option<String>,
    pub schema_change_policy: Option<SchemaChangePolicy>,
    pub recrawl_policy: Option<RecrawlPolicy>,
    pub lineage_configuration: Option<LineageConfiguration>,
    pub lake_formation_configuration: Option<LakeFormationConfiguration>,
    pub configuration: Option<String>,
    pub crawler_security_configuration: Option<String>,
    /// The cron expression the create supplied, if any. Reported inside a `Schedule`.
    pub schedule_expression: Option<String>,
    /// The state that schedule is in, one of the modelled `ScheduleState` values: a cron
    /// expression a write states is in force ([`SCHEDULE_STATE_SCHEDULED`]) until the
    /// operation that suspends a schedule moves it to [`SCHEDULE_STATE_NOT_SCHEDULED`],
    /// which is also where a crawler carrying no expression sits. Reported as the
    /// `Schedule`'s `State`, so it is observable only while an expression is there to
    /// report it with.
    pub schedule_state: String,
    /// The catalog the crawler writes its output to.
    pub catalog_id: String,
    /// One of the modelled `CrawlerState` values; [`CRAWLER_STATE_READY`] at create.
    pub state: String,
    /// Server-set version counter, [`FIRST_CRAWLER_VERSION`] for a fresh crawler.
    pub version: i64,
    /// Server-set, written once at create so every later read reports the same instant.
    pub creation_time: SystemTime,
    /// Server-set; moves with every write to this crawler.
    pub last_updated: SystemTime,
    /// The request's `Tags`, which are not part of the `Crawler` shape — they are read
    /// back through the tagging operations.
    pub tags: HashMap<String, String>,
    /// When the crawl IN FLIGHT began, or `None` while no crawl is. What
    /// `CrawlElapsedTime` is measured from: "if the crawler is running, contains the total
    /// time elapsed since the last crawl began" (`Crawler$CrawlElapsedTime`).
    pub crawl_started_at: Option<SystemTime>,
    /// The instant the clock owes this crawler its way back to [`CRAWLER_STATE_READY`];
    /// `None` once the crawl has settled, which is also every crawler that has none in
    /// flight.
    pub crawl_settles_at: Option<SystemTime>,
    /// The Region the last crawl of this crawler was STARTED in, or `None` for a crawler
    /// nothing has crawled. Glue is a regional service and a crawl is set going through one
    /// endpoint, so this is the Region whose default event bus the crawl's state change is
    /// published onto ([`CrawlerStateChange::region`]).
    pub crawl_region: Option<String>,
    /// What the crawl in flight did, to be published when the clock settles it
    /// ([`Self::settle_crawl_if_due`]). The work itself was done where the crawl was
    /// STARTED, so this is what it answered — which is why the status, the error message
    /// and the table counts a settled crawl reports are its own reading's.
    pub pending_crawl: Option<CrawlRecord>,
    /// The crawls of this crawler that have FINISHED, newest first and bounded by
    /// [`MAX_CRAWL_HISTORY`] (the oldest dropped first).
    ///
    /// ONE record per crawl, so everything read off this crawler's past agrees: the newest
    /// entry is what `LastCrawl` reports and where a metrics read takes this crawler's table
    /// counts from ([`Self::last_crawl`]), the durations of the entries are the ones
    /// `MedianRuntimeSeconds` is the median of, and the whole list — behind the crawl in
    /// flight, which is newer than any of them — is the history a crawl listing pages over
    /// ([`Self::crawls`]).
    pub crawl_history: VecDeque<CrawlRecord>,
}

impl CrawlerEntry {
    /// Build a stored crawler from a `CreateCrawler` request plus the server-derived
    /// members (`State`, `Version`, `CreationTime`, `LastUpdated`), none of which the
    /// client may set.
    pub fn from_create_input(
        input: &CreateCrawlerInput,
        catalog_id: &str,
        now: SystemTime,
    ) -> Self {
        Self {
            name: input.name.clone(),
            role: input.role.clone(),
            targets: input.targets.clone(),
            database_name: input.database_name.clone(),
            description: input.description.clone(),
            classifiers: input.classifiers.clone().unwrap_or_default(),
            table_prefix: input.table_prefix.clone(),
            schema_change_policy: input.schema_change_policy.clone(),
            recrawl_policy: input.recrawl_policy.clone(),
            lineage_configuration: input.lineage_configuration.clone(),
            lake_formation_configuration: input.lake_formation_configuration.clone(),
            configuration: input.configuration.clone(),
            crawler_security_configuration: input.crawler_security_configuration.clone(),
            schedule_expression: input.schedule.clone(),
            schedule_state: stated_schedule_state(input.schedule.as_deref()),
            catalog_id: catalog_id.to_string(),
            state: CRAWLER_STATE_READY.to_string(),
            version: FIRST_CRAWLER_VERSION,
            creation_time: now,
            last_updated: now,
            tags: input.tags.clone().unwrap_or_default(),
            // A freshly created crawler has crawled nothing: no crawl is in flight, none
            // has finished, and there is no duration to report.
            crawl_started_at: None,
            crawl_settles_at: None,
            crawl_region: None,
            pending_crawl: None,
            crawl_history: VecDeque::new(),
        }
    }

    /// Apply an `UpdateCrawler` request to this stored crawler, member by member.
    ///
    /// Every member the request CARRIES replaces the stored one; every member it OMITS
    /// is preserved. That is what the model states for the one member whose preservation
    /// it spells out — "if you omit this value, the existing value on the crawler is
    /// preserved" (`UpdateCrawlerRequest$CatalogId`) — and what the update's own
    /// `Description` requires: it targets the REMOVABLE description shape where the
    /// create targets the plain one, so sending an empty description is how a caller
    /// takes the description OFF a crawler, a distinction that exists only because
    /// omitting a member leaves it alone.
    ///
    /// `catalog_id` is the catalog the request named, already resolved and checked by the
    /// handler, or `None` when the request named none.
    ///
    /// The server-set members move with the write: `Version` advances by one (the first
    /// version of a crawler is [`FIRST_CRAWLER_VERSION`] and an update moves it on) and
    /// `LastUpdated` becomes `now`. The crawler's `Name`, `CreationTime`, `State` and
    /// tags are not this request's to change: `UpdateCrawlerRequest` has no member for
    /// any of them.
    pub fn apply_update(
        &mut self,
        input: &UpdateCrawlerInput,
        catalog_id: Option<String>,
        now: SystemTime,
    ) {
        if let Some(role) = input.role.as_ref() {
            self.role = role.clone();
        }
        if let Some(targets) = input.targets.as_ref() {
            self.targets = targets.clone();
        }
        if let Some(database_name) = input.database_name.as_ref() {
            self.database_name = Some(database_name.clone());
        }
        // The removable description: empty takes it off the crawler rather than storing
        // a description that is the empty string.
        if let Some(description) = input.description.as_ref() {
            self.description = (!description.is_empty()).then(|| description.clone());
        }
        if let Some(classifiers) = input.classifiers.as_ref() {
            self.classifiers = classifiers.clone();
        }
        if let Some(table_prefix) = input.table_prefix.as_ref() {
            self.table_prefix = Some(table_prefix.clone());
        }
        if let Some(schema_change_policy) = input.schema_change_policy.as_ref() {
            self.schema_change_policy = Some(schema_change_policy.clone());
        }
        if let Some(recrawl_policy) = input.recrawl_policy.as_ref() {
            self.recrawl_policy = Some(recrawl_policy.clone());
        }
        if let Some(lineage_configuration) = input.lineage_configuration.as_ref() {
            self.lineage_configuration = Some(lineage_configuration.clone());
        }
        if let Some(lake_formation_configuration) = input.lake_formation_configuration.as_ref() {
            self.lake_formation_configuration = Some(lake_formation_configuration.clone());
        }
        if let Some(configuration) = input.configuration.as_ref() {
            self.configuration = Some(configuration.clone());
        }
        if let Some(security_configuration) = input.crawler_security_configuration.as_ref() {
            self.crawler_security_configuration = Some(security_configuration.clone());
        }
        if let Some(schedule) = input.schedule.as_ref() {
            self.schedule_expression = Some(schedule.clone());
            self.schedule_state = stated_schedule_state(Some(schedule));
        }
        if let Some(catalog_id) = catalog_id {
            self.catalog_id = catalog_id;
        }
        self.version = self.version.saturating_add(1);
        self.last_updated = now;
    }

    /// Apply an `UpdateCrawlerSchedule` request's schedule to this stored crawler.
    ///
    /// That request STATES the crawler's schedule rather than merging into it: the cron
    /// expression it carries becomes the crawler's schedule, and a request that carries
    /// NONE leaves the crawler with no schedule at all. `CrawlerName` only ADDRESSES the
    /// crawler, so the schedule is the one thing the request says anything about and its
    /// absence can only mean "no schedule" — which is also the one way a caller can take
    /// a schedule off a crawler, since `UpdateCrawler`'s own `Schedule` is a plain
    /// expression whose absence means "leave the crawler alone".
    ///
    /// The write moves the server-set members exactly as [`Self::apply_update`] does,
    /// because it writes the same member of the same crawler definition: `Version`
    /// advances by one and `LastUpdated` becomes `now`. Nothing else about the crawler is
    /// this request's to change.
    pub fn apply_schedule(&mut self, schedule: Option<&str>, now: SystemTime) {
        self.schedule_expression = schedule.map(str::to_string);
        self.schedule_state = stated_schedule_state(schedule);
        self.version = self.version.saturating_add(1);
        self.last_updated = now;
    }

    /// Whether this crawler's schedule is RUNNING, i.e. its cron expression is in force
    /// ([`SCHEDULE_STATE_SCHEDULED`]). False both for a crawler whose schedule has been
    /// suspended and for one that carries no expression to run: neither has a scheduler
    /// running for it, which is what the operations that suspend and resume a schedule
    /// decide about.
    pub fn schedule_is_running(&self) -> bool {
        self.schedule_state == SCHEDULE_STATE_SCHEDULED
    }

    /// Suspend this crawler's schedule: the schedule state becomes
    /// [`SCHEDULE_STATE_NOT_SCHEDULED`], which is all `StopCrawlerSchedule` decides —
    /// "sets the schedule state of the specified crawler to NOT_SCHEDULED, but does not
    /// stop the crawler if it is already running".
    ///
    /// The cron expression STAYS on the crawler: suspending a schedule is not removing it
    /// (that is what a schedule write stating no expression does), so the expression is
    /// there to be put back in force. The crawler's run `State` is untouched for the
    /// reason the documentation gives, and `Version` with it: it counts the crawler's
    /// DEFINITION, which this call writes no member of. `LastUpdated` moves, because the
    /// crawler record did change.
    pub fn suspend_schedule(&mut self, now: SystemTime) {
        self.schedule_state = SCHEDULE_STATE_NOT_SCHEDULED.to_string();
        self.last_updated = now;
    }

    /// Whether this crawler has a cron expression to run at all, which is what a schedule
    /// write put on it ([`Self::apply_schedule`]). A crawler carrying none has no schedule
    /// to start, resume or report.
    pub fn has_schedule(&self) -> bool {
        self.schedule_expression.is_some()
    }

    /// Put this crawler's suspended schedule back in force: the schedule state becomes
    /// [`SCHEDULE_STATE_SCHEDULED`], which is all `StartCrawlerSchedule` decides — it
    /// "changes the schedule state of the specified crawler to `SCHEDULED`, unless the
    /// crawler is already running or the schedule state is already `SCHEDULED`".
    ///
    /// The cron expression is the one already on the crawler: this call carries none, so what
    /// it resumes is the schedule a write stated. Nothing else moves, for the reason
    /// [`Self::suspend_schedule`] gives for the opposite write: the crawler's run `State` is
    /// not this call's to change and `Version` counts the crawler's DEFINITION, which this
    /// call writes no member of, while `LastUpdated` moves because the record did change.
    pub fn resume_schedule(&mut self, now: SystemTime) {
        self.schedule_state = SCHEDULE_STATE_SCHEDULED.to_string();
        self.last_updated = now;
    }

    /// Whether a crawl of this crawler is IN PROGRESS, i.e. its run state is not the one
    /// modelled `CrawlerState` with no run in it ([`CRAWLER_STATE_READY`]): of the other
    /// two, [`CRAWLER_STATE_RUNNING`] is a crawl under way and [`CRAWLER_STATE_STOPPING`]
    /// one that has been asked to stop and has not finished, so each still has a crawl
    /// the service is working on.
    pub fn crawl_in_progress(&self) -> bool {
        self.state != CRAWLER_STATE_READY
    }

    /// Record the crawl `record` as this crawler's crawl IN FLIGHT: the crawler leaves
    /// [`CRAWLER_STATE_READY`] for [`CRAWLER_STATE_RUNNING`], and the clock owes it its way
    /// back [`CRAWL_SETTLES_AFTER`] later.
    ///
    /// The crawl's own work was already done by the caller that performed it, and `record`
    /// is what it answered; nothing of it is published yet — a crawl in flight leaves the
    /// PREVIOUS crawl's `LastCrawl` and table counts in place, which is what a read of a
    /// running crawler reports.
    ///
    /// `region` is the Region the start was served in, which is the Region whose default event
    /// bus this crawl's state change is published onto when it comes to rest.
    ///
    /// The crawler's definition is untouched: a crawl writes no member of it, so `Version`
    /// does not move. `LastUpdated` does, because the crawler record did change.
    pub fn start_crawl(&mut self, record: CrawlRecord, now: SystemTime, region: &str) {
        self.state = CRAWLER_STATE_RUNNING.to_string();
        self.crawl_started_at = Some(now);
        self.crawl_settles_at = Some(now.checked_add(CRAWL_SETTLES_AFTER).unwrap_or(now));
        self.crawl_region = Some(region.to_string());
        self.pending_crawl = Some(record);
        self.last_updated = now;
    }

    /// Whether this crawler's crawl has ALREADY been asked to stop and has not finished:
    /// the one state ([`CRAWLER_STATE_STOPPING`]) in which another stop has nothing left to
    /// ask for.
    pub fn crawl_is_stopping(&self) -> bool {
        self.state == CRAWLER_STATE_STOPPING
    }

    /// Ask the crawl in flight to STOP: the crawler leaves [`CRAWLER_STATE_RUNNING`] for
    /// [`CRAWLER_STATE_STOPPING`] and the clock owes it its way back to
    /// [`CRAWLER_STATE_READY`] [`CRAWL_SETTLES_AFTER`] from this call, so a caller that reads
    /// the crawler straight after the stop still observes the crawl coming to rest.
    ///
    /// The crawl it stops ends CANCELLED — "`CANCELLED`" is the modelled `LastCrawlStatus`
    /// of a crawl that neither succeeded nor failed — and keeps everything it had already
    /// recorded, the tables it wrote included: a stop ends the crawl, it does not undo it.
    /// Nothing is published here, for the reason [`Self::start_crawl`] gives: the crawl's
    /// report reaches a reader when the clock settles it ([`Self::settle_crawl_if_due`]).
    ///
    /// The crawler's definition is untouched, so `Version` does not move; `LastUpdated` does,
    /// because the crawler record did change.
    pub fn stop_crawl(&mut self, now: SystemTime) {
        self.state = CRAWLER_STATE_STOPPING.to_string();
        self.crawl_settles_at = Some(now.checked_add(CRAWL_SETTLES_AFTER).unwrap_or(now));
        if let Some(record) = self.pending_crawl.as_mut() {
            record.status = LAST_CRAWL_STATUS_CANCELLED.to_string();
        }
        self.last_updated = now;
    }

    /// Move this crawler into the state the clock owed it at `now`, if it owed one.
    ///
    /// The crawl was due to finish at [`Self::crawl_settles_at`] whether or not anybody
    /// called, and the instants recorded are that DUE instant, not the instant somebody read
    /// it — so two reads of a settled crawl report the same crawl. Finishing it is what
    /// publishes the crawl's `LastCrawl` and its table counts, and what puts the crawler back
    /// in [`CRAWLER_STATE_READY`]: the state with no run in it.
    ///
    /// Answers the state change the crawl that came to rest PUBLISHES, so the caller can queue
    /// it ([`ComAmazonawsGlueAwsglueState::settle_due_crawls`]): this is the one place a crawl
    /// moves from in-flight to finished, which is what makes the event exactly once. A call
    /// that settled nothing answers `None`.
    pub fn settle_crawl_if_due(&mut self, now: SystemTime) -> Option<CrawlerStateChange> {
        let due = self.crawl_settles_at?;
        if due > now {
            return None;
        }
        self.crawl_settles_at = None;
        self.crawl_started_at = None;
        self.state = CRAWLER_STATE_READY.to_string();
        self.last_updated = due;
        let mut record = self.pending_crawl.take()?;
        record.finished_at = Some(due);
        let change = self.crawl_state_change(&record, due);
        // Newest first, bounded: the oldest crawl this crawler holds the record of is dropped
        // to make room, the way every other bounded history in this store is kept.
        self.crawl_history.push_front(record);
        while self.crawl_history.len() > MAX_CRAWL_HISTORY {
            self.crawl_history.pop_back();
        }
        change
    }

    /// The most recent crawl of this crawler that has FINISHED, or `None` until one has:
    /// the newest entry of [`Self::crawl_history`], which is what `LastCrawl` and a metrics
    /// read report.
    pub fn last_crawl(&self) -> Option<&CrawlRecord> {
        self.crawl_history.front()
    }

    /// Every crawl of this crawler this store holds the record of, newest first: the crawl in
    /// FLIGHT, which is newer than any that finished, then the finished ones in
    /// [`Self::crawl_history`] order.
    ///
    /// The one order the crawls of a crawler are read in, so a listing of them cannot
    /// disagree with what `LastCrawl` reports.
    pub fn crawls(&self) -> impl Iterator<Item = &CrawlRecord> {
        self.pending_crawl
            .iter()
            .chain(self.crawl_history.iter())
    }

    /// How long the crawl in flight has been running at `now`, in milliseconds: "if the
    /// crawler is running, contains the total time elapsed since the last crawl began"
    /// (`Crawler$CrawlElapsedTime`).
    ///
    /// Zero for a crawler with no crawl in flight: no crawl has begun for any time to have
    /// elapsed since.
    fn crawl_elapsed_time(&self, now: SystemTime) -> i64 {
        let Some(started) = self.crawl_started_at else {
            return 0;
        };
        now.duration_since(started)
            .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    /// Rebuild the modelled `CrawlerMetrics` shape from what is stored: "Metrics for a
    /// specified crawler".
    ///
    /// Every member is computed from what the store holds about THIS crawler — whether it
    /// has a crawl in progress, and what the crawls it has finished did:
    ///
    /// * "True if the crawler is still estimating how long it will take to complete this
    ///   run" (`StillEstimating`) is about a run in progress, and the service estimates a
    ///   run's remaining time from the runs that finished before it; a crawler here has
    ///   finished none, so a crawl in progress is one that is still being estimated and a
    ///   crawler with no crawl in progress has nothing to estimate.
    /// * `TimeLeftSeconds` is "the estimated time left to complete a running crawl", so
    ///   there is no estimate to report for a run still being estimated, and none at all
    ///   for a crawler that is not running.
    /// * `LastRuntimeSeconds` and `MedianRuntimeSeconds` are durations of this crawler's
    ///   runs, so they are read off the crawls that have FINISHED
    ///   ([`Self::crawl_history`]) — zero for a crawler none of whose crawls has
    ///   finished, because it has no run to have taken any time.
    /// * `TablesCreated`, `TablesUpdated` and `TablesDeleted` are the catalog tables this
    ///   crawler's crawls wrote, so they are the ones the LAST finished crawl wrote
    ///   ([`Self::last_crawl`]) — the same crawl [`Self::to_crawler`] reports as
    ///   `LastCrawl`, so a metrics read and a crawler read cannot disagree about which
    ///   crawl they describe.
    pub fn to_crawler_metrics(&self) -> CrawlerMetrics {
        let finished = self.last_crawl();
        CrawlerMetrics {
            crawler_name: Some(self.name.clone()),
            time_left_seconds: 0.0,
            still_estimating: self.crawl_in_progress(),
            last_runtime_seconds: finished
                .and_then(CrawlRecord::duration)
                .map_or(0.0, |duration| duration.as_secs_f64()),
            median_runtime_seconds: median_seconds(
                &self
                    .crawl_history
                    .iter()
                    .filter_map(CrawlRecord::duration)
                    .collect::<Vec<Duration>>(),
            ),
            tables_created: finished.map_or(0, |crawl| crawl.tables_created),
            tables_updated: finished.map_or(0, |crawl| crawl.tables_updated),
            tables_deleted: finished.map_or(0, |crawl| crawl.tables_deleted),
        }
    }

    /// Rebuild the modelled `Crawler` output shape from what is stored.
    ///
    /// `now` is the instant the read is served, which the one time-derived member needs:
    /// `CrawlElapsedTime` is measured from the instant the crawl in flight began. Every
    /// other member comes from the store, so repeated reads of a crawler with no crawl in
    /// flight agree.
    ///
    /// The sub-structures of the definition that the service defaults are reported with the
    /// value they are IN FORCE with: the members the write stated, and the service's own
    /// default for each one it did not ([`stated_schema_change_policy`] and the three
    /// beside it). A crawler whose write stated no policy at all is still crawling under
    /// one, so the read reports the policy it would be crawled under rather than leaving a
    /// client to guess it — and the target lists it names no target in are reported EMPTY,
    /// because a crawler has every kind of target list and crawls nothing in the ones it
    /// was given no target for.
    pub fn to_crawler(&self, now: SystemTime) -> Crawler {
        Crawler {
            name: Some(self.name.clone()),
            // "The IAM role or Amazon Resource Name (ARN) of an IAM role" is what a write
            // may name the crawler's role by; what a read reports is the NAME IAM holds
            // that role under ([`reported_crawler_role`]).
            role: Some(reported_crawler_role(&self.role)),
            targets: Some(stated_targets(&self.targets)),
            database_name: self.database_name.clone(),
            description: self.description.clone(),
            classifiers: Some(self.classifiers.clone()),
            recrawl_policy: Some(stated_recrawl_policy(self.recrawl_policy.as_ref())),
            schema_change_policy: Some(stated_schema_change_policy(
                self.schema_change_policy.as_ref(),
            )),
            lineage_configuration: Some(stated_lineage_configuration(
                self.lineage_configuration.as_ref(),
            )),
            state: Some(self.state.clone()),
            table_prefix: self.table_prefix.clone(),
            schedule: self.schedule_expression.as_ref().map(|expression| Schedule {
                schedule_expression: Some(expression.clone()),
                state: Some(self.schedule_state.clone()),
            }),
            // "If the crawler is running, contains the total time elapsed since the
            // last crawl began".
            crawl_elapsed_time: self.crawl_elapsed_time(now),
            creation_time: Some(to_date_time(self.creation_time)),
            last_updated: Some(to_date_time(self.last_updated)),
            // "Status and error information about the most recent crawl": absent until
            // a crawl has FINISHED and produced one — a crawl in flight has produced no
            // status yet, so it leaves the previous crawl's report in place.
            last_crawl: self.last_crawl().map(CrawlRecord::to_last_crawl_info),
            version: self.version,
            configuration: self.configuration.clone(),
            crawler_security_configuration: self.crawler_security_configuration.clone(),
            lake_formation_configuration: Some(stated_lake_formation_configuration(
                self.lake_formation_configuration.as_ref(),
            )),
            catalog_id: Some(self.catalog_id.clone()),
        }
    }
}

/// The `Role` a READ of a crawler reports, from the value its write named the role by.
///
/// A write names the crawler's execution role by "the IAM role or Amazon Resource Name
/// (ARN) of an IAM role" (`CreateCrawler$Role`), so the member admits two forms of the same
/// thing; a read reports the NAME IAM holds that role under — the role's own name, which an
/// ARN of this account's role carries after its path ([`glue_role_reference`], this crate's
/// one reading of the member). A value that names no role of this account is reported as it
/// was given: there is no name inside it this service may claim.
pub fn reported_crawler_role(role: &str) -> String {
    glue_role_reference(role).name.to_string()
}

/// The median of `durations`, in seconds, or zero for a crawler whose crawls have produced
/// none.
///
/// The median of an even number of durations is the mean of the two in the middle, which is
/// what a median of a sample of runtimes is. The input is at most
/// [`MAX_CRAWL_HISTORY`] long, so the sort is a bounded cost.
fn median_seconds(durations: &[Duration]) -> f64 {
    if durations.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<Duration> = durations.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1].as_secs_f64() + sorted[middle].as_secs_f64()) / 2.0
    } else {
        sorted[middle].as_secs_f64()
    }
}

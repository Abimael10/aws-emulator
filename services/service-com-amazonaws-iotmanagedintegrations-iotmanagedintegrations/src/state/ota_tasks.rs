// SPDX-License-Identifier: CC0-1.0

//! The over-the-air (OTA) TASK half of this service's state: the record `CreateOtaTask`
//! writes, the references that create resolves, and the projections `GetOtaTask` and
//! `ListOtaTasks` report it through.
//!
//! A child module of [`super`] rather than more of `state.rs`: an OTA task is one concern,
//! and it is the only resource of this service whose reported status is DERIVED from the
//! clock and the schedule its create carried rather than stored as a field
//! ([`OtaTask::status_at`]). The OTA task CONFIGURATION a task may name is a different
//! resource, and stays in `state.rs` beside the other records.

use super::*;

/// The summary shape a list projects a stored task onto, which no other item of
/// `crate::state` reaches, so it is imported here rather than at the module root.
use crate::ops::types::{OtaTaskExecutionRetryConfig, OtaTaskSchedulingConfig, OtaTaskSummary};

/// The `ResourceType` this service reports an OTA task under — the Smithy resource name, as
/// [`MANAGED_THING_RESOURCE_TYPE`] is for a managed thing. It is also the resource kind an
/// [`IdempotencyKey`] of a `CreateOtaTask` carries.
pub const OTA_TASK_RESOURCE_TYPE: &str = "OtaTask";

/// The OTA-task resource as the subject of its READ path's 404 sentence
/// ([`with_id_does_not_exist_message`]), spelled the way this service's backend spells it in
/// that sentence — its own capitalisation of the acronym, which is not the one the OTA task
/// CONFIGURATION's read uses for the kind of resource it names
/// ([`OTA_TASK_CONFIGURATION_RESOURCE_SUBJECT`]).
pub const OTA_TASK_RESOURCE_SUBJECT: &str = "Ota task";

/// The resource-type segment an OTA task's ARN carries, which is both what the `OtaTaskArn`
/// pattern (`^arn:aws:iotmanagedintegrations:[0-9a-zA-Z-]+:[0-9]+:ota-task/[0-9a-zA-Z]+$`)
/// requires and how the `IoTManagedIntegrationsResourceARN` pattern of the tagging surface
/// names that kind of resource.
pub const OTA_TASK_ARN_SEGMENT: &str = "ota-task";

/// The `OtaStatus` a task is in while its rollout is live: the status of a task whose
/// schedule has started, and of one created without a schedule at all. A member of
/// [`crate::ops::types::OTA_STATUS_VALUES`].
pub const OTA_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";

/// The `OtaStatus` a task is in before the start time its `OtaTaskSchedulingConfig` names:
/// the rollout is accepted and waiting for its window. A member of
/// [`crate::ops::types::OTA_STATUS_VALUES`].
///
/// `CANCELED`, `COMPLETED` and `DELETION_IN_PROGRESS` are the other values the model
/// declares, and no task of this emulator reports one: a completion is what the devices a
/// task targets report back, and nothing in this emulator executes a task against a
/// device, so no task is moved to a terminal status by the passage of time.
pub const OTA_STATUS_SCHEDULED: &str = "SCHEDULED";

/// The most OTA tasks one store holds, for the same reason and on the same terms as
/// [`MAX_MANAGED_THINGS`]: a bound CHOSEN for the emulator, not a published AWS quota.
pub const MAX_OTA_TASKS: usize = 1_000;

/// The most `OtaTaskSummary` items one `ListOtaTasks` page carries, and the page size an
/// absent `MaxResults` gets, for the same reason and on the same terms as
/// [`MANAGED_THING_PAGE_LIMIT`]: a caller reaches the rest of the store through the
/// `NextToken`, so one response stays bounded however many records are stored.
pub const OTA_TASK_PAGE_LIMIT: usize = 100;

/// The most devices one OTA task targets, and so the most `Target` entries one stored task
/// holds. The member carries no modelled `@length`, and every entry is kept on the record
/// and reported back, so the create clamps what one request may store: a bound CHOSEN for
/// the emulator, not a published AWS quota.
pub const MAX_OTA_TASK_TARGETS: usize = 100;

/// One stored over-the-air (OTA) task: the authoritative record every OTA-task operation
/// reads and writes. `CreateOtaTask` inserts it, `GetOtaTask` projects it onto its output
/// shape and `ListOtaTasks` projects it onto an [`OtaTaskSummary`].
#[derive(Debug, Clone)]
pub struct OtaTask {
    /// The generated `OtaTaskId` this record is keyed by.
    pub id: String,
    /// `OtaTaskArn`, as [`ota_task_arn`] builds it.
    pub arn: String,
    /// The region the creating request was signed for. An OTA task is a regional resource,
    /// as every resource of this service is: only that region's endpoint resolves or lists
    /// it.
    pub region: String,
    /// `OtaDescription`, absent on a task the create named none for.
    pub description: Option<String>,
    /// `S3Url`, which the create `@required`s: where the update the task deploys is stored.
    pub s3_url: String,
    /// `OtaProtocol` enum value, absent when the create named none (the model publishes no
    /// default for it).
    pub protocol: Option<String>,
    /// `OtaType` enum value, which the create `@required`s.
    pub ota_type: String,
    /// `OtaTargetQueryString`: the query that selects the devices to add to the task's
    /// thing group, stored exactly as the create supplied it.
    pub ota_target_query_string: Option<String>,
    /// `OtaMechanism` enum value, absent when the create named none.
    pub ota_mechanism: Option<String>,
    /// The `Target` entries the create carried, in the order it sent them: the devices this
    /// task deploys to. Bounded by [`MAX_OTA_TASK_TARGETS`].
    pub target: Vec<String>,
    /// The `OtaTaskConfigurationId` this task rolls out under, absent when the create named
    /// none. A create that names one resolves it, so a stored id is one this region really
    /// holds a configuration under.
    pub task_configuration_id: Option<String>,
    /// `OtaTaskSchedulingConfig`, stored exactly as the create supplied it so a read reports
    /// the schedule the caller submitted.
    pub scheduling_config: Option<OtaTaskSchedulingConfig>,
    /// `OtaTaskExecutionRetryConfig`, stored the same way.
    pub execution_retry_config: Option<OtaTaskExecutionRetryConfig>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    /// When the task was last changed — the create's instant until an update changes it,
    /// which is what `LastUpdatedAt` reports.
    pub updated_at: SystemTime,
    /// The instant the schedule its `OtaTaskSchedulingConfig` names begins, when that
    /// config named a `StartTime` this service could read as a date-time. `None` for a task
    /// created without a schedule, which starts rolling out at once.
    ///
    /// Storing the instant is what keeps a read a pure read: the reported status is DERIVED
    /// from it and the clock ([`Self::status_at`]), so nothing has to be written when a
    /// schedule becomes due.
    pub scheduled_start: Option<SystemTime>,
    /// Monotonic insertion sequence. `ListOtaTasks` pages by it, so a page token keeps
    /// resuming at the right place even when records are deleted in between.
    pub sequence: u64,
}

impl OtaTask {
    /// The `OtaStatus` this task is in at `now`: waiting for the start time its schedule
    /// names ([`OTA_STATUS_SCHEDULED`]), and rolling out from it ([`OTA_STATUS_IN_PROGRESS`]).
    ///
    /// A task created without a schedule, or with one whose `StartTime` has passed, is
    /// rolling out: there is nothing left for the clock to begin.
    pub fn status_at(&self, now: SystemTime) -> &'static str {
        if self.scheduled_start.is_some_and(|start| now < start) {
            OTA_STATUS_SCHEDULED
        } else {
            OTA_STATUS_IN_PROGRESS
        }
    }
}

/// The ARN of an OTA task in `region`, as the `OtaTaskArn` pattern requires.
pub fn ota_task_arn(region: &str, id: &str) -> String {
    ::engine::emulator_arn(
        SERVICE_ARN_NAMESPACE,
        region,
        &format!("{OTA_TASK_ARN_SEGMENT}/{id}"),
    )
}

/// The modelled `ResourceNotFoundException` a READ of an OTA task answers when this region
/// holds none under the `Identifier` its request carries: the sentence this service's
/// OTA-task backend states such an absence with ([`with_id_does_not_exist_message`], which
/// carries the identifier the request sent), and nothing else.
///
/// The `ResourceId` and `ResourceType` members stay ABSENT, as they do for the OTA task
/// configuration ([`ota_task_configuration_not_found_on_read`]): the identifier is already
/// in the reason, and this resource's backend fills neither.
pub fn ota_task_not_found_on_read(identifier: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(with_id_does_not_exist_message(
            OTA_TASK_RESOURCE_SUBJECT,
            identifier,
        )),
        resource_id: None,
        resource_type: None,
    }
}

/// How a `CreateOtaTask` states that the `S3Url` it carried is not one this service can take
/// an artifact from: the fixed sentence AWS words an S3 fault OF THE REQUEST with, followed
/// by the detail that came back about the URL.
///
/// The text is AWS's own: the recorded refusal of a create whose `S3Url` does not name a
/// usable Amazon S3 object reads `S3 error occurred due to invalid request: ` — the fixed
/// sentence, and after its colon whatever detail the S3 fault carried. A value whose own
/// FORM addresses no object is the whole of what this service establishes, and nothing was
/// asked of any store to establish it, so there is no detail to state and the sentence ends
/// at its colon ([`NO_S3_DETAIL`]).
pub fn s3_invalid_request_message(detail: &str) -> String {
    format!("S3 error occurred due to invalid request: {detail}")
}

/// The detail a refusal grounded in the URL's own form carries: none, for the reason on
/// [`s3_invalid_request_message`].
pub const NO_S3_DETAIL: &str = "";

/// Whether `s3_url` addresses no Amazon S3 object at all — the question `CreateOtaTask` asks
/// of its `S3Url` before it resolves anything the request NAMES, since a task whose artifact
/// URL addresses nothing describes a rollout that cannot be carried out.
///
/// The member is "the URL to the Amazon S3 bucket where the over-the-air (OTA) task is
/// stored" (`smithy_query(query="doc",shape="CreateOtaTaskRequest",member="S3Url")`) and
/// carries a `@length` of 1..1000 and nothing else, so the request boundary's constraint
/// walk admits any text of that length, an artifact URL among the least of them. What a URL
/// NAMES is not decided here: it is the tree's one reading of an S3 location
/// (`service_common::s3_uri::url_object_address`), which accepts every spelling Amazon S3
/// publishes for an object — the `s3://` URI, a virtual-hosted endpoint URL and a path-style
/// one — and reports nothing for a value that addresses no bucket-and-key.
///
/// What is read is the REQUEST, not the store: whether the account's object storage HOLDS
/// that object is a question for Amazon S3, while this refusal is the one the form of the
/// value itself grounds — which is also what AWS's own sentence for it states
/// ([`s3_invalid_request_message`]).
pub fn names_no_s3_object(s3_url: &str) -> bool {
    ::service_common::s3_uri::url_object_address(s3_url).is_none()
}

/// The `OtaTaskSummary` a list reports for the stored task `task` at `now`.
///
/// Every member comes from the record, the status included — it is the same derivation the
/// task's own read reports ([`OtaTask::status_at`]) — so a task reported in a list and the
/// same task read on its own cannot disagree.
pub fn ota_task_summary(task: &OtaTask, now: SystemTime) -> OtaTaskSummary {
    OtaTaskSummary {
        task_id: Some(task.id.clone()),
        task_arn: Some(task.arn.clone()),
        created_at: Some(timestamp(task.created_at)),
        last_updated_at: Some(timestamp(task.updated_at)),
        task_configuration_id: task.task_configuration_id.clone(),
        status: Some(task.status_at(now).to_string()),
    }
}

/// The instant `text` names, when it is an RFC 3339 timestamp: the format every timestamp
/// of this API travels in on the wire.
///
/// Both zone forms RFC 3339 admits are read — the `Z` form and the numeric offset a client
/// building its timestamp with `isoformat()` sends — by splitting the declared offset off
/// through the ONE reader of that rule (`service_common::rfc3339::split_utc_offset`) and
/// shifting the civil part by it, so this service cannot disagree with another about which
/// instant a timestamp names. The calendar conversion itself stays in the maintained
/// library; a schedule is resolved to the second, which is the resolution a rollout window
/// is stated at.
///
/// `None` for anything else, including an instant outside the range a `SystemTime` of this
/// host represents. `ScheduleStartTime` is modelled as a plain `string` with no format
/// trait, so a schedule whose start this service cannot read is stored as the caller sent
/// it and leaves the task rolling out at once rather than being refused for its spelling.
pub fn schedule_instant(text: &str) -> Option<SystemTime> {
    let (civil, offset) = ::service_common::rfc3339::split_utc_offset(text)?;
    let parsed = ::aws_smithy_types::DateTime::from_str(
        &format!("{civil}Z"),
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .ok()?;
    let seconds = parsed.secs().checked_sub(offset)?;
    let magnitude = Duration::from_secs(seconds.unsigned_abs());
    if seconds >= 0 {
        UNIX_EPOCH.checked_add(magnitude)
    } else {
        UNIX_EPOCH.checked_sub(magnitude)
    }
}

impl ComAmazonawsIotmanagedintegrationsIotmanagedintegrationsState {
    /// The OTA task `region`'s endpoint resolves under `id`, if any. A task created in
    /// another region is not visible here, as for every regional resource.
    pub fn ota_task_in(&self, region: &str, id: &str) -> Option<&OtaTask> {
        self.ota_tasks.get(id).filter(|task| task.region == region)
    }

    /// [`Self::ota_task_in`], mutably: what a tagging call writes to, so the write side
    /// resolves a task by exactly the rule the read side resolves it by.
    pub fn ota_task_in_mut(&mut self, region: &str, id: &str) -> Option<&mut OtaTask> {
        self.ota_tasks
            .get_mut(id)
            .filter(|task| task.region == region)
    }

    /// Every OTA task `region`'s endpoint resolves, in the order they were created: what a
    /// list reports, and what bounds its paging.
    pub fn ota_tasks_in<'a>(&'a self, region: &'a str) -> impl Iterator<Item = &'a OtaTask> {
        self.ota_task_order
            .iter()
            .filter_map(|id| self.ota_tasks.get(id))
            .filter(move |task| task.region == region)
    }

    /// Store `task`, keyed by its own id and appended to the order every operation that
    /// reports several tasks reports them in.
    pub fn insert_ota_task(&mut self, task: OtaTask) {
        self.ota_task_order.push(task.id.clone());
        self.ota_tasks.insert(task.id.clone(), task);
    }
}

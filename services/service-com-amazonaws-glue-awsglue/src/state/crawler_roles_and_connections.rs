// SPDX-License-Identifier: CC0-1.0

//! What a crawler's targets and its IAM role resolve to — the event queues a crawl would
//! read and the role documents that say whether it may be assumed — and the stored
//! connection records a catalog holds, which those targets are reached through.

use super::*;

/// Whether `targets` names at least one data store to crawl, across every target list
/// `CrawlerTargets` carries. `CreateCrawler` documents that "at least one crawl target
/// must be specified", and an empty list names none.
pub fn has_crawl_target(targets: &CrawlerTargets) -> bool {
    fn any<T>(list: &Option<Vec<T>>) -> bool {
        list.as_ref().is_some_and(|entries| !entries.is_empty())
    }

    any(&targets.s3_targets)
        || any(&targets.jdbc_targets)
        || any(&targets.mongo_db_targets)
        || any(&targets.dynamo_db_targets)
        || any(&targets.catalog_targets)
        || any(&targets.delta_targets)
        || any(&targets.iceberg_targets)
        || any(&targets.hudi_targets)
}

/// Every DISTINCT Amazon SQS queue the crawl of `targets` consumes, in order: the
/// `EventQueueArn` of each S3 target that names one, beside the dead-letter queue it names
/// for the same crawl.
///
/// An S3 target naming an event queue is crawled FROM that queue — "a policy that
/// specifies whether to crawl the entire dataset again, or to crawl only folders that
/// were added since the last crawler run" (`CreateCrawler$RecrawlPolicy`) — so these are
/// the queues the crawler's role has to be able to consume. The list is DEDUPLICATED: the
/// same queue named by two targets is one queue to decide about, and the number of
/// decisions one request can ask for is what [`MAX_CHECKED_EVENT_QUEUES`] bounds.
pub fn crawl_event_queues(targets: &CrawlerTargets) -> Vec<String> {
    let mut queues: Vec<String> = targets
        .s3_targets
        .iter()
        .flatten()
        .flat_map(|target| {
            target
                .event_queue_arn
                .iter()
                .chain(target.dlq_event_queue_arn.iter())
        })
        .filter(|queue| !queue.trim().is_empty())
        .cloned()
        .collect();
    queues.sort();
    queues.dedup();
    queues
}

/// The most Amazon S3 targets of ONE crawler write this resolution asks the object store
/// about.
///
/// CHOSEN, not documented: `CrawlerTargets$S3Targets` carries no modelled length, so the
/// number of object-store calls one create or update pays for is bounded here rather than
/// left to the request. The targets past the ceiling are not resolved, so nothing is refused
/// for them: a bound of this service's own decides what it CHECKS, never what AWS would
/// accept.
pub const MAX_RESOLVED_S3_TARGETS: usize = 32;

/// Why an Amazon S3 target of `targets` cannot back a crawl, or `None` when the store
/// resolves every one of them.
///
/// `S3Target$Path` is "the path to the Amazon S3 target", and a crawler is registered against
/// the targets it will crawl — so each path is RESOLVED against the object store when the
/// crawler is written, and a path whose BUCKET the store does not hold is refused with the
/// operation's modelled `InvalidInputException` naming that target and the store's own answer
/// about it ([`messages::s3_target_not_found`]).
///
/// Only what the store ANSWERED refuses a write: a bucket it serves, and anything it said
/// other than "no such bucket" (`messages::DIAGNOSTIC_CODE_NO_SUCH_BUCKET`, the one answer
/// this reads the store for and the one the refusal states), leaves the write to go ahead — so a path that names no bucket
/// at all, and a store that cannot be reached, refuse nothing. A PREFIX holding no object is
/// not refused either: a crawler is routinely registered before the data it crawls is
/// written, so only the bucket's absence is the target that cannot be resolved.
///
/// The walk is bounded ([`MAX_RESOLVED_S3_TARGETS`]) and each DISTINCT bucket is asked about
/// once, so a request naming many targets in one bucket pays for one call. Awaited with no
/// store guard held.
pub async fn s3_target_refusal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    targets: &CrawlerTargets,
) -> Option<String> {
    let paths: Vec<&str> = targets
        .s3_targets
        .iter()
        .flatten()
        .take(MAX_RESOLVED_S3_TARGETS)
        .filter_map(|target| target.path.as_deref())
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .collect();
    if paths.is_empty() {
        return None;
    }
    let client = super::column_statistics_source::s3_client(ctx, region);
    let mut resolved: Vec<String> = Vec::new();
    for path in paths {
        // A path that names no bucket is one this resolution cannot put to the store, so it
        // says nothing about the target.
        let Some((bucket, prefix)) = super::split_s3_location(path) else {
            continue;
        };
        if resolved.contains(&bucket) {
            continue;
        }
        resolved.push(bucket.clone());
        // The question is "does the store hold this target": the listing is the call a crawl
        // of the target starts with, and it is asked for ONE key, so a bucket holding data
        // costs nothing to resolve.
        let answer = client
            .list_objects_v2()
            .bucket(&bucket)
            .prefix(&prefix)
            .max_keys(1)
            .send()
            .await;
        if let Err(error) = answer {
            let code = ::aws_smithy_types::error::metadata::ProvideErrorMetadata::code(&error);
            if code == Some(messages::DIAGNOSTIC_CODE_NO_SUCH_BUCKET) {
                return Some(messages::s3_target_not_found(path));
            }
            ::tracing::debug!(
                target = %path,
                code = ?code,
                "the object store answered nothing about a crawler's Amazon S3 target, so \
                 the target is not refused"
            );
        }
    }
    None
}

/// The IAM store a caller-supplied `Role` is resolved against.
///
/// It is the IAM service's OWN state struct, reached through the SAME shared engine this
/// service's store lives in, so Glue reads the very records `CreateRole`,
/// `PutRolePolicy` and `AttachRolePolicy` wrote rather than a second copy of them.
pub type IamState =
    ::service_com_amazonaws_iam_awsidentitymanagementv20100508::ComAmazonawsIamAwsidentitymanagementv20100508State;

/// The IAM store in the SAME shared engine, which is where the account's roles live.
/// `None` means the engine carries no such store — a deployment with no IAM to ask,
/// which is evidence of nothing.
pub fn iam_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<IamState>> {
    ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::service_state(engine)
}

/// The service principal a caller-supplied execution `Role` has to trust for this service to
/// act as it. The work — a crawl, a statistics run, a transform's task runs, a session's
/// statements — is performed BY Glue acting as that role, so the role's trust policy is what
/// lets Glue assume it at all.
pub const GLUE_SERVICE_PRINCIPAL: &str = "glue.amazonaws.com";

/// The ONE reading of the form an execution `Role` may take: its role NAME and its role ARN.
///
/// `Role` is "the IAM role or Amazon Resource Name (ARN) of an IAM role used by the new
/// crawler to access customer resources" (`CreateCrawler$Role`), and the same member of the
/// other writes that take one names a role the same way (`CreateMLTransform$Role` is "the
/// name or Amazon Resource Name (ARN) of the IAM role",
/// `CreateColumnStatisticsTaskSettings$Role` "the role used for running the column
/// statistics", while `CreateSession$Role` is modelled as an ARN outright). So a value that
/// reads as an ARN carries the name inside it ([`::service_common::iam_role::role_name`], the
/// tree's one reading of an IAM ARN) and any other value IS the name. Stated once here so the
/// questions this crate asks about that member — the trust document
/// ([`glue_role_is_unassumable`]) and the identity policies in force on the role
/// ([`role_policy_documents`]) — cannot read the member's form two different ways.
pub fn glue_role_reference(role: &str) -> GlueRole<'_> {
    match ::service_common::iam_role::role_name(role) {
        // A value that reads as this account's role ARN: the name is the one IAM holds it
        // under, and the ARN is the value as sent.
        Some(name) => GlueRole {
            name,
            arn: role.to_string(),
        },
        // Not this account's role ARN. Either it is a bare role name — in which case the
        // ARN is the one this emulator mints for a role of that name — or it is an ARN of
        // another account, which names no role this emulator's IAM can be asked about and
        // so resolves to no local name either.
        None if ::service_common::arn::parse(role).is_some() => GlueRole {
            name: role,
            arn: role.to_string(),
        },
        None => GlueRole {
            name: role,
            // An IAM ARN is partition-global, so it carries no Region.
            arn: ::engine::emulator_arn(IAM_ARN_NAMESPACE, "", &format!("{ROLE_ARN_SEGMENT}/{role}")),
        },
    }
}

/// What an execution `Role` resolves to: the name IAM would hold the role under, and the ARN
/// that addresses it. See [`glue_role_reference`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlueRole<'a> {
    /// The role name, for a lookup in the IAM store's own records.
    pub name: &'a str,
    /// The role ARN, for a reading that addresses the role by ARN.
    pub arn: String,
}

/// The ARN vendor field of an IAM entity.
pub(super) const IAM_ARN_NAMESPACE: &str = "iam";

/// The ARN resource-type segment of an IAM role.
pub(super) const ROLE_ARN_SEGMENT: &str = "role";

/// Whether this emulator's own IAM ANSWERED that `role` is a role Glue cannot assume: this
/// account holds no such role, or the trust policy the role carries does not admit
/// [`GLUE_SERVICE_PRINCIPAL`].
///
/// A resource that RUNS AS its `Role` — a crawler, the column statistics task settings of a
/// table, a machine learning transform, an interactive session — is backed by nothing at all
/// when Glue cannot assume that role, and each of those writes settles this before it stores
/// anything (the refusal each of them words its own way lives in [`super::messages`]). The
/// whole question — which role of
/// this account the value names, what IAM answers about it, and whether the trust document
/// it carries admits a service principal — is the ONE reading every emulated service that
/// resolves a caller-supplied execution role shares
/// (`service_common::iam_role::role_is_unassumable`), so this crate states only WHICH
/// principal has to be admitted and WHICH operations ask.
///
/// IAM is reached the way any emulated service reaches another: the AWS SDK over the
/// emulator's OWN in-process callback ([`smithy_server_runtime::OperationContext::callback`]),
/// so nothing here opens a socket or reads an ambient credential. `false` — do not refuse —
/// for every case where nothing is KNOWN about the role, including an emulator with no IAM
/// mounted to ask: a refusal has to be grounded in what IAM answered, never assumed. Awaited
/// with NO store guard held.
pub async fn glue_role_is_unassumable(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role: &str,
) -> bool {
    glue_role_is_unassumable_through(ctx.callback(), region, role).await
}

/// [`glue_role_is_unassumable`] asked through the emulator's own callback rather than through
/// a request's context, for work this service performs on a task of its own — a job run's
/// script, which is executed past the life of the call that started the run
/// ([`super::job_script`]).
///
/// The question, the grounds and the answer are [`glue_role_is_unassumable`]'s; only who is
/// asking differs ([`::service_common::iam_role::role_is_unassumable_through`]).
pub async fn glue_role_is_unassumable_through(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    role: &str,
) -> bool {
    ::service_common::iam_role::role_is_unassumable_through(
        callback,
        region,
        &glue_role_reference(role).arn,
        GLUE_SERVICE_PRINCIPAL,
    )
    .await
}

/// The Amazon SQS actions a crawler performs against the event queue of an S3 target it
/// crawls in event mode: it RECEIVES the change messages, reads the queue's attributes to
/// learn what is pending, and DELETES each message it has processed. Listed in the order
/// the crawl makes them, which is the order a refusal states the grant it needs in
/// ([`event_queue_actions_clause`]).
///
/// Each is qualified the way the queue's OWN service qualifies its actions — `SQS:` before
/// the API name, the spelling its policies and its authorization refusals carry (see that
/// service's `state::permissions::POLICY_ACTION_PREFIX`) — so the action a refusal names is
/// spelled as the service that owns it spells it. IAM compares an action name without
/// regard to case, so a role whose document grants `sqs:ReceiveMessage` grants exactly
/// this.
pub const EVENT_QUEUE_ACTIONS: [&str; 3] = [
    "SQS:ReceiveMessage",
    "SQS:GetQueueAttributes",
    "SQS:DeleteMessage",
];

/// The queue calls a crawl makes, as a refusal lists them: each action in the order the crawl
/// makes them ([`EVENT_QUEUE_ACTIONS`]), comma-separated and closed by `and`.
///
/// The REQUIREMENT clause of [`messages::event_queue_grant_missing`] states the whole grant
/// the crawl needs rather than the one call that happened to be withheld first, because every
/// one of them has to be admitted for the crawl to read the queue at all — so a caller is told
/// what to grant, whichever of them its policies are missing.
pub fn event_queue_actions_clause() -> String {
    actions_clause(&EVENT_QUEUE_ACTIONS)
}

/// The Amazon DynamoDB actions a crawl of a DynamoDB target performs against the table it
/// names: it READS the table's definition, to learn the key attributes the dataset's columns
/// are declared from, and then SCANS its items to read those columns off the data. Listed in
/// the order the crawl makes them, which is the order a refusal states the grant it needs in
/// ([`dynamodb_target_actions_clause`]).
///
/// Each is qualified the way the table's OWN service qualifies its actions — the API name
/// behind `dynamodb:`, the spelling its own authorization decisions carry — so the action a
/// refusal names is spelled as the service that owns it spells it. IAM compares an action
/// name without regard to case.
pub const DYNAMODB_TARGET_ACTIONS: [&str; 2] = ["dynamodb:DescribeTable", "dynamodb:Scan"];

/// The table calls a crawl makes, as a refusal lists them: each action in the order the crawl
/// makes them ([`DYNAMODB_TARGET_ACTIONS`]), comma-separated and closed by `and`.
///
/// The REQUIREMENT clause of [`messages::dynamodb_target_grant_missing`] states the whole
/// grant the crawl needs rather than the one call that happened to be withheld first, for the
/// reason [`event_queue_actions_clause`] gives: every one of them has to be admitted for the
/// crawl to read the table at all.
pub fn dynamodb_target_actions_clause() -> String {
    actions_clause(&DYNAMODB_TARGET_ACTIONS)
}

/// `actions` as a refusal lists them: comma-separated, with the last one closed by `and`.
///
/// ONE renderer, shared by every grant clause this crate states, so two refusals cannot list
/// the calls they need in two different shapes.
fn actions_clause(actions: &[&str]) -> String {
    match actions.split_last() {
        Some((last, [])) => (*last).to_string(),
        Some((last, leading)) => format!("{} and {last}", leading.join(", ")),
        None => String::new(),
    }
}

/// Every action name a policy may state about a queue, as the service that OWNS the queue
/// publishes them.
///
/// Read from that service rather than listed here, so the question [`queue_is_disclosed`]
/// asks — may this role touch that queue AT ALL? — leaves out no call it serves, and a name
/// added to or withdrawn from its API changes this with it. Each is asked about qualified with
/// that service's own action prefix, which IAM compares without regard to case, so it decides
/// exactly as the `SQS:`-spelled [`EVENT_QUEUE_ACTIONS`] do.
fn queue_action_names() -> Vec<&'static str> {
    ::service_com_amazonaws_sqs_amazonsqs::state::authorization::policy_action_names()
}

/// The most policy documents this check reads for ONE role.
///
/// A role carries its inline policies plus the managed policies attached to it. A role
/// whose documents outnumber this is one the check does not finish reading, and an unread
/// document might be the one that carries the grant — so nothing is shown about such a
/// role and the create goes ahead. The count is taken BEFORE any document is read, so the
/// reading itself is bounded by it.
pub const MAX_ROLE_DOCUMENTS: usize = 32;

/// The most event queues one `CreateCrawler` request may have decided about.
///
/// CHOSEN, not documented: the target lists carry no modelled length, so the number of
/// distinct queues a request can name — and with it the number of policy evaluations the
/// create pays for, each over at most [`MAX_ROLE_DOCUMENTS`] documents — is bounded here
/// rather than left to the request. A create past the ceiling is refused with the
/// operation's own modelled `InvalidInputException`.
pub const MAX_CHECKED_EVENT_QUEUES: usize = 32;

/// The most Amazon DynamoDB tables one crawler write may have decided about.
///
/// CHOSEN, not documented, for the reason [`MAX_CHECKED_EVENT_QUEUES`] gives: the target
/// lists carry no modelled length, so the number of distinct tables a request can name — and
/// with it the number of policy evaluations the write pays for, each over at most
/// [`MAX_ROLE_DOCUMENTS`] documents — is bounded here rather than left to the request. A
/// write past the ceiling is refused with the operation's own modelled
/// `InvalidInputException`.
pub const MAX_CHECKED_DYNAMODB_TARGETS: usize = 32;

/// Every DISTINCT Amazon DynamoDB table the crawl of `targets` reads, in order: the `Path` of
/// each DynamoDB target, which is "the name of the DynamoDB table to crawl"
/// (`DynamoDBTarget$Path`).
///
/// The list is DEDUPLICATED: the same table named by two targets is one table to decide
/// about, and the number of decisions one request can ask for is what
/// [`MAX_CHECKED_DYNAMODB_TARGETS`] bounds. A target naming no table at all names nothing to
/// decide about.
pub fn crawl_dynamodb_tables(targets: &CrawlerTargets) -> Vec<String> {
    let mut tables: Vec<String> = targets
        .dynamo_db_targets
        .iter()
        .flatten()
        .filter_map(|target| target.path.as_deref())
        .map(str::trim)
        .filter(|table| !table.is_empty())
        .map(str::to_string)
        .collect();
    tables.sort();
    tables.dedup();
    tables
}

/// The identity-based policy documents in force on the role a crawler's `Role` names: its
/// inline role policies plus the documents of the managed policies attached to it.
///
/// `Role` is "the IAM role or Amazon Resource Name (ARN) of an IAM role"
/// (`CreateCrawler$Role`), whose form is read HERE by the crate's one reading of that
/// member ([`glue_role_reference`]), so this lookup and the trust-document reading
/// beside it resolve the same value to the same role.
///
/// `None` when nothing can be SHOWN about the role's permissions: this account holds no
/// such role, it carries more documents than [`MAX_ROLE_DOCUMENTS`] (counted before any is
/// read), or one of its attachments is an AWS-managed policy whose document IAM does not
/// carry. Called with the IAM guard held, and it CLONES what it reads, so the caller drops
/// the guard before deciding anything: nothing bounded by the request runs under it.
pub fn role_policy_documents(role: &str, iam: &IamState) -> Option<Vec<String>> {
    use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;

    let name = glue_role_reference(role).name;
    let stored = iam.roles.get(&iam_store::name_key(name))?;
    let role_key = iam_store::name_key(&stored.role_name);
    let inline = iam.role_inline_policies.get(&role_key);
    let attached = iam.role_attached_policies.get(&role_key);
    // Counted FIRST, so a role carrying more documents than this check reads costs no
    // clone and no parse: an unread document might carry the grant, so nothing is shown
    // about such a role either way.
    let held = inline.map_or(0, |policies| policies.len())
        + attached.map_or(0, |attachments| attachments.len());
    if held > MAX_ROLE_DOCUMENTS {
        return None;
    }
    let mut documents: Vec<String> = inline
        .into_iter()
        .flat_map(|policies| policies.values())
        .map(|policy| policy.policy_document.clone())
        .collect();
    for arn in attached.into_iter().flatten() {
        match default_policy_document(iam, arn) {
            Some(document) => documents.push(document),
            // An AWS-managed policy ARN this emulator's IAM carries no document for might
            // be the one that carries the grant, and this account cannot read it: nothing
            // can be shown about this role. Which ARNs are in AWS's namespace is IAM's
            // OWN reading, called rather than re-derived.
            None if iam_store::is_aws_managed_policy_arn(arn) => return None,
            None => {}
        }
    }
    Some(documents)
}

/// Why the crawler role whose policies are `prepared` cannot consume the event queue
/// `queue_arn`, or `None` when those documents admit the whole crawl.
///
/// A crawler that crawls an S3 target from an event queue runs AS its `Role`, so the
/// policies in force on that role must admit the queue operations the crawl makes
/// ([`EVENT_QUEUE_ACTIONS`]). The decision is made against the account's own
/// IAM records — the ones `CreateRole`/`PutRolePolicy`/`AttachRolePolicy` wrote, collected
/// by [`role_policy_documents`] — through the shared evaluator every service that decides
/// authorization uses, so no two services answer differently.
///
/// EVERY action the crawl makes has to be admitted, so one the documents do not admit settles
/// THAT the create is refused; which of IAM's rules withheld it (an explicit `Deny`, or no
/// grant at all) decides nothing, because Glue states its own refusal rather than the queue
/// service's authorization answer.
///
/// WHICH of Glue's two refusals it is, is settled by what those same documents show the role
/// about the queue ITSELF ([`queue_is_disclosed`]): a role they admit some call on that queue
/// to is refused with the grant the crawl needs named, since a queue it may touch is a queue
/// that is there and a missing grant is the only thing left to report
/// ([`messages::event_queue_grant_missing`]); a role they admit no call on it to is shown
/// nothing that tells an absent queue from one it may not reach, and is refused with the
/// sentence that conflates exactly those two ([`messages::event_queue_inaccessible`]).
///
/// The documents are PREPARED once per request and passed in, so a request naming several
/// queues parses them once rather than once per queue, and the caller holds no IAM guard
/// while this runs.
pub fn event_queue_refusal(
    prepared: &[::service_common::iam_authorization::PreparedDocument],
    queue_arn: &str,
) -> Option<String> {
    if EVENT_QUEUE_ACTIONS
        .iter()
        .all(|action| admits_call(prepared, action, queue_arn))
    {
        return None;
    }
    Some(if queue_is_disclosed(prepared, queue_arn) {
        messages::event_queue_grant_missing(queue_arn)
    } else {
        messages::event_queue_inaccessible(queue_arn)
    })
}

/// Why the crawler role whose policies are `prepared` cannot read a target the crawl of
/// `targets` would walk, or `None` when those documents admit every read it makes.
///
/// A crawl runs AS the crawler's `Role`, so the policies in force on that role must admit the
/// calls the crawl makes on each store it reads through: the queue operations of an S3 target
/// crawled in event mode ([`event_queue_refusal`]) and the table reads of a DynamoDB target
/// ([`dynamodb_target_refusal`]). The targets are decided about in that order, and the first
/// one the documents do not admit is the refusal — the whole decision is made against the
/// account's own IAM records through the shared evaluator, so no two services answer
/// differently.
///
/// The documents are PREPARED once per request and passed in, so a request naming several
/// targets parses them once rather than once per target, and the caller holds no IAM guard
/// while this runs.
pub fn crawl_target_grant_refusal(
    prepared: &[::service_common::iam_authorization::PreparedDocument],
    region: &str,
    targets: &CrawlerTargets,
) -> Option<String> {
    crawl_event_queues(targets)
        .iter()
        .find_map(|queue| event_queue_refusal(prepared, queue))
        .or_else(|| {
            crawl_dynamodb_tables(targets)
                .iter()
                .find_map(|table| dynamodb_target_refusal(prepared, region, table))
        })
}

/// Why the crawler role whose policies are `prepared` cannot read the Amazon DynamoDB table
/// `table`, or `None` when those documents admit the whole crawl of it.
///
/// A crawler with a DynamoDB target reads that table AS its `Role`, so the policies in force
/// on the role must admit the table calls the crawl makes ([`DYNAMODB_TARGET_ACTIONS`]) on the
/// table itself ([`dynamodb_table_arn`]). The decision is made against the account's own IAM
/// records — the ones `CreateRole`/`PutRolePolicy`/`AttachRolePolicy` wrote, collected by
/// [`role_policy_documents`] — through the shared evaluator every service that decides
/// authorization uses.
///
/// EVERY action the crawl makes has to be admitted, so one the documents do not admit settles
/// that the write is refused; which of IAM's rules withheld it (an explicit `Deny`, or no
/// grant at all) decides nothing, because Glue states its own refusal rather than the table
/// service's authorization answer ([`messages::dynamodb_target_grant_missing`]).
///
/// A role the account holds NO document for is shown nothing about the table, so it is not
/// refused: a role is routinely provisioned with its grant out of band, and a call no document
/// is in force over is decided by nothing — the reading the crawl's own read authorization
/// makes of the same records ([`CrawlReadAuthorization::withholds_table_read`]).
pub fn dynamodb_target_refusal(
    prepared: &[::service_common::iam_authorization::PreparedDocument],
    region: &str,
    table: &str,
) -> Option<String> {
    if prepared.is_empty() {
        return None;
    }
    let resource = dynamodb_table_arn(region, table);
    if DYNAMODB_TARGET_ACTIONS
        .iter()
        .all(|action| admits_call(prepared, action, &resource))
    {
        return None;
    }
    Some(messages::dynamodb_target_grant_missing(table))
}

/// The resource a policy statement about the Amazon DynamoDB table `table` names:
/// `arn:aws:dynamodb:<region>:<account>:table/<name>`, in the Region the request reached and
/// with this emulator's own account segment ([`::engine::emulator_arn`]) — the form that
/// service mints every table ARN of this account in.
pub fn dynamodb_table_arn(region: &str, table: &str) -> String {
    ::engine::emulator_arn(
        DYNAMODB_ARN_NAMESPACE,
        region,
        &format!("{DYNAMODB_TABLE_ARN_SEGMENT}/{table}"),
    )
}

/// The ARN vendor field of an Amazon DynamoDB resource.
const DYNAMODB_ARN_NAMESPACE: &str = "dynamodb";

/// The ARN resource-type segment of an Amazon DynamoDB table.
const DYNAMODB_TABLE_ARN_SEGMENT: &str = "table";

/// Whether the policies `prepared` admit the role the call `action` on the resource
/// `resource`.
///
/// Nothing stated is an implicit deny in IAM, and the documents in force on the role ARE the
/// authority on what it may do: a role whose policies say nothing about the call may not make
/// it. The one reading of that question in this module, so what a refusal is decided on and
/// what it is WORDED from cannot drift apart.
fn admits_call(
    prepared: &[::service_common::iam_authorization::PreparedDocument],
    action: &str,
    resource: &str,
) -> bool {
    use ::service_common::iam_authorization as authorization;

    let request = authorization::Request::new(action, resource, &[]);
    matches!(
        authorization::identity_decision_prepared(prepared, &request),
        authorization::Decision::Allowed
    )
}

/// Whether the policies `prepared` show the role ANYTHING about the queue `queue_arn`: whether
/// they admit it so much as one of that queue's own service's calls on it
/// ([`queue_action_names`]).
///
/// This is the question that tells Glue's two refusals apart, and it is NOT a requirement on
/// the create — what an event-mode crawl has to be admitted is [`EVENT_QUEUE_ACTIONS`], and a
/// role holding that whole grant is never refused. A role admitted any call on the queue has
/// been shown the queue is there, so the refusal can name the grant it still needs; a role
/// admitted none has been shown nothing that tells a queue that is not there from one it may
/// not reach, which is what the other refusal says. A grant of some OTHER service's action on
/// the same ARN shows nothing about this queue: only the owning service's calls reach it.
///
/// The decision is the role's DOCUMENTS alone, exactly as the refusal beside it is: whether a
/// queue of that ARN is there is the queue service's own answer, and this check does not ask
/// it — it refuses only what the account's IAM records SHOW, so the two refusals differ by
/// what the role may do and never by what another service holds.
fn queue_is_disclosed(
    prepared: &[::service_common::iam_authorization::PreparedDocument],
    queue_arn: &str,
) -> bool {
    let prefix = ::service_com_amazonaws_sqs_amazonsqs::state::authorization::ACTION_PREFIX;

    queue_action_names()
        .into_iter()
        .any(|name| admits_call(prepared, &format!("{prefix}:{name}"), queue_arn))
}

/// The default version's document of the managed policy `arn` names, or `None` when the
/// account holds no such policy and it is none of the AWS-managed policies IAM carries.
pub(super) fn default_policy_document(iam: &IamState, arn: &str) -> Option<String> {
    use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;

    // An AWS-managed policy is not account state: IAM holds its document in its own
    // catalogue, so a role's grant that lives in one is read from there.
    if let Some(managed) = iam_store::aws_managed_policy(arn) {
        return Some(managed.document.to_string());
    }
    let managed = iam.policy_by_arn(arn)?;
    managed
        .policy
        .default_version_id
        .as_deref()
        .and_then(|default| managed.versions.get(default))
        .or_else(|| {
            managed
                .versions
                .values()
                .find(|version| version.is_default_version)
        })
        .and_then(|version| version.document.clone())
}

/// Whether an `UpdateCrawler` request names at least one member of the crawler to
/// change.
///
/// `Name` ADDRESSES the crawler rather than changing it (`UpdateCrawlerRequest` has no
/// rename member), so a request carrying nothing besides it asks for no change: there is
/// nothing to write, and the crawler's `Version` and `LastUpdated` stay where the last
/// write that did change something left them. No recording or doc states what the service
/// answers such a call, so what is NOT done here is advancing a write counter for a change
/// nobody asked for; a recording of that call would settle it. Written as an exhaustive
/// destructuring so a member added to the request shape cannot be silently left out of the
/// answer.
pub fn crawler_update_names_a_change(input: &UpdateCrawlerInput) -> bool {
    let UpdateCrawlerInput {
        name: _,
        role,
        database_name,
        description,
        targets,
        schedule,
        classifiers,
        table_prefix,
        schema_change_policy,
        recrawl_policy,
        lineage_configuration,
        lake_formation_configuration,
        configuration,
        crawler_security_configuration,
        catalog_id,
    } = input;

    role.is_some()
        || database_name.is_some()
        || description.is_some()
        || targets.is_some()
        || schedule.is_some()
        || classifiers.is_some()
        || table_prefix.is_some()
        || schema_change_policy.is_some()
        || recrawl_policy.is_some()
        || lineage_configuration.is_some()
        || lake_formation_configuration.is_some()
        || configuration.is_some()
        || crawler_security_configuration.is_some()
        || catalog_id.is_some()
}

/// Whether `expression` has the form the service documents for a crawler schedule: a
/// `cron(..)` expression of six fields ("to run something every day at 12:15 UTC, you
/// would specify: `cron(15 12 * * ? *)`" — `Schedule$ScheduleExpression`). Only the
/// form is checked here; the field values are the scheduler's business.
pub fn is_cron_expression(expression: &str) -> bool {
    let trimmed = expression.trim();
    let Some(fields) = trimmed
        .strip_prefix("cron(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    fields.split_whitespace().count() == 6
}

/// The `VersionId` a table carries straight after `CreateTable`.
///
/// The catalog's table version history starts at 0: the version a create makes is
/// version `"0"`, and each update appends the next one. So a table that has never been
/// updated reports `"0"`, and that is the id `GetTableVersion`/`DeleteTableVersion`
/// address its only version by — numbering the first version `"1"` left version `"0"`
/// unfetchable. `VersionString` is a string, so the id is formatted as one.
pub const FIRST_TABLE_VERSION_ID: &str = "0";

/// A stored connection definition: the `Connection` every read projects, plus the tags the
/// create carried.
#[derive(Debug, Clone)]
pub struct ConnectionEntry {
    /// `Name`, which is also this entry's key in [`Catalog::connections`].
    pub name: String,
    /// What a read of this connection reports, before `HidePassword` is applied.
    pub definition: Connection,
    /// `CreateConnectionRequest$Tags`.
    pub tags: HashMap<String, String>,
}

impl ConnectionEntry {
    /// Build a stored connection definition from the client's `ConnectionInput`.
    ///
    /// `CreationTime` and `LastUpdatedTime` are both the instant of the create (nothing has
    /// updated the definition yet), `LastUpdatedBy` is the principal that issued it, and
    /// `ConnectionSchemaVersion` is 2 exactly when the input carried the
    /// per-compute-environment property maps version 2 adds ("Version 2 supports properties
    /// for specific compute environments"), else 1.
    pub fn from_input(
        input: &ConnectionInput,
        last_updated_by: Option<String>,
        tags: Option<&HashMap<String, String>>,
        now: SystemTime,
    ) -> Self {
        let compute_environment_properties = [
            input.spark_properties.as_ref(),
            input.athena_properties.as_ref(),
            input.python_properties.as_ref(),
        ]
        .iter()
        .any(|properties| properties.is_some_and(|map| !map.is_empty()));
        let at = to_date_time(now);
        Self {
            name: input.name.clone(),
            definition: Connection {
                name: Some(input.name.clone()),
                description: input.description.clone(),
                connection_type: Some(input.connection_type.clone()),
                match_criteria: input.match_criteria.clone(),
                connection_properties: Some(input.connection_properties.clone()),
                spark_properties: input.spark_properties.clone(),
                athena_properties: input.athena_properties.clone(),
                python_properties: input.python_properties.clone(),
                physical_connection_requirements: input.physical_connection_requirements.clone(),
                creation_time: Some(at),
                last_updated_time: Some(at),
                last_updated_by,
                // `Status`/`StatusReason` report the outcome of VALIDATING the connection
                // ("Can be one of: READY, IN_PROGRESS, or FAILED"), and this emulator
                // connects to nothing, so it reports no status and no validation instant
                // rather than inventing one.
                status: None,
                status_reason: None,
                last_connection_validation_time: None,
                // The authentication configuration a read reports is the one the input
                // declared, with its credentials left where they were: an
                // `AuthenticationConfigurationInput` carries secrets
                // (`BasicAuthenticationCredentials`, OAuth2 credentials) that the response
                // shape has no member for, and nothing here copies them into one.
                authentication_configuration: input.authentication_configuration.as_ref().map(
                    |configuration| crate::ops::types::AuthenticationConfiguration {
                        authentication_type: configuration.authentication_type.clone(),
                        secret_arn: configuration.secret_arn.clone(),
                        kms_key_arn: configuration.kms_key_arn.clone(),
                        o_auth2_properties: None,
                    },
                ),
                connection_schema_version: Some(if compute_environment_properties {
                    CONNECTION_SCHEMA_VERSION_V2
                } else {
                    CONNECTION_SCHEMA_VERSION_V1
                }),
                // "A list of compute environments compatible with the connection" is a
                // property of the connection TYPE, which this emulator does not model.
                compatible_compute_environments: None,
            },
            tags: tags.cloned().unwrap_or_default(),
        }
    }

    /// Overwrite this connection's definition with the one the client submitted.
    ///
    /// `ConnectionInput` is the WHOLE client-settable definition of a connection — the
    /// same shape a create submits, "used to specify a connection to create or update" —
    /// so an update writes every member it carries and clears the ones it omits, and
    /// `ConnectionSchemaVersion` is recomputed from what it carries exactly as at create.
    /// The members no `ConnectionInput` carries keep their own rules, because the client
    /// cannot set them: `CreationTime` is the instant of the CREATE and never moves,
    /// `LastUpdatedTime` and `LastUpdatedBy` become this call's instant and principal, and
    /// the tags are written through the tagging operations rather than by this update.
    ///
    /// The submitted `Name` is written like any other member; relocating the entry under
    /// a new key would be the caller's job, since the key lives in
    /// [`Catalog::connections`].
    pub fn apply_input(
        &mut self,
        input: &ConnectionInput,
        last_updated_by: Option<String>,
        now: SystemTime,
    ) {
        // One function decides what a `ConnectionInput` means, so an update projects it
        // exactly as a create does; only the create's own instant is carried over.
        let created_at = self.definition.creation_time;
        let rewritten = Self::from_input(input, last_updated_by, None, now);
        self.name = rewritten.name;
        self.definition = rewritten.definition;
        self.definition.creation_time = created_at;
    }

    /// What a read of this connection reports.
    ///
    /// `hide_password` is `GetConnectionRequest$HidePassword`: "Allows you to retrieve the
    /// connection metadata without returning the password", and
    /// `compute_environment` is `GetConnectionRequest$ApplyOverrideForComputeEnvironment`:
    /// "For connections that may be used in multiple services, specifies returning
    /// properties for the specified compute environment" — that environment's own property
    /// map overlays the connection's, key by key, so a property only that environment sets
    /// is reported and one it does not set keeps the connection's value.
    ///
    /// The overlay is applied FIRST and the password properties are withheld from EVERY
    /// property map the answer carries — the connection's own and each
    /// per-compute-environment one — because a per-environment map may itself carry a
    /// `PASSWORD`, and "without returning the password" is about the password, not about
    /// one member.
    pub fn to_connection(
        &self,
        hide_password: bool,
        compute_environment: Option<&str>,
    ) -> Connection {
        let mut connection = self.definition.clone();
        let overrides = match compute_environment {
            Some(COMPUTE_ENVIRONMENT_SPARK) => connection.spark_properties.clone(),
            Some(COMPUTE_ENVIRONMENT_ATHENA) => connection.athena_properties.clone(),
            Some(COMPUTE_ENVIRONMENT_PYTHON) => connection.python_properties.clone(),
            // The model's `ComputeEnvironment` enum carries exactly those three, and the
            // request walk refuses any other value before a handler runs.
            _ => None,
        };
        if let Some(overrides) = overrides {
            connection
                .connection_properties
                .get_or_insert_with(Default::default)
                .extend(overrides);
        }
        if hide_password {
            for properties in [
                connection.connection_properties.as_mut(),
                connection.spark_properties.as_mut(),
                connection.athena_properties.as_mut(),
                connection.python_properties.as_mut(),
            ]
            .into_iter()
            .flatten()
            {
                properties.remove(PASSWORD_PROPERTY);
                properties.remove(ENCRYPTED_PASSWORD_PROPERTY);
            }
        }
        connection
    }
}

// SPDX-License-Identifier: CC0-1.0

//! What a restore FROM Amazon S3 is accepted against: the engines the migration supports,
//! and the backup the named bucket and prefix have to hold.
//!
//! Two operations migrate a database backed up into Amazon S3 into this service —
//! `RestoreDBClusterFromS3` ("creates an Amazon Aurora DB cluster from MySQL data stored in an
//! Amazon S3 bucket … the data must be created using the Percona XtraBackup utility") and
//! `RestoreDBInstanceFromS3` — and both are accepted against the same two things: the engine
//! PAIR the request names, and whether the location it names holds a backup at all.
//!
//! Each operation publishes its own pair ([`S3RestorePair`]): the engine of the resource it
//! creates (`Engine`), the engine the backup was taken from (`SourceEngine`) and the versions
//! of that engine it reads (`SourceEngineVersion`). The pair is decided from the REQUEST
//! alone, so it is answered before the bucket is read; the location is a resource of ANOTHER
//! service, so it is resolved where that service holds it, and an emulator with no object
//! store beside this one establishes nothing and refuses nothing.
//!
//! The third thing both are accepted against is the ROLE they read the backup UNDER:
//! `S3IngestionRoleArn` is "an Amazon Web Services Identity and Access Management (IAM) role
//! with a trust policy and a permissions policy that allows Amazon RDS to access your Amazon
//! S3 bucket", so the Amazon S3 reads the migration makes are authorized against that role's
//! permissions rather than against the caller's own
//! ([`ingestion_role_is_unauthorized`]) — and the role is read in two stages, because the two
//! answer different things to the caller: whether the value is a role this restore can be run
//! under AT ALL ([`ingestion_role_is_unusable`], asked before the location is resolved, since
//! there is no role to read the location under), and then whether the permissions in force on
//! that role cover the reads this restore makes on the location it resolved.

/// The engine pair ONE restore-from-Amazon-S3 operation migrates between, as that operation's
/// own members publish it, plus the sentence that operation words a source bucket it cannot
/// reach with.
#[derive(Debug, Clone, Copy)]
pub struct S3RestorePair {
    /// `Engine`'s published domain for the operation: the engine of the resource the restore
    /// creates.
    pub engine: &'static str,
    /// `SourceEngine`'s published domain: the engine the backup in Amazon S3 was taken from.
    pub source_engine: &'static str,
    /// The `SourceEngineVersion` series the operation reads a backup of. A request names one
    /// bare ("8.0") or with its patch component ("8.0.28"), which is how the member's own
    /// examples spell them.
    pub source_engine_versions: &'static [&'static str],
    /// The sentence THIS operation refuses a source bucket it cannot find or read at all
    /// with. A refusal belongs to the operation that answers it, and the two restores word
    /// this one apart — see [`CLUSTER_BUCKET_UNREACHABLE_MESSAGE`] and
    /// [`INSTANCE_BUCKET_UNREACHABLE_MESSAGE`] — so it travels with the operation's own facts
    /// rather than being one text both reach for.
    pub bucket_unreachable_message: &'static str,
}

/// What `RestoreDBClusterFromS3` migrates: "Valid Values: aurora-mysql (for Aurora MySQL)"
/// (`Engine`), "Valid Values: mysql" (`SourceEngine`) and "MySQL versions 5.7 and 8.0 are
/// supported. Example: 5.7.40, 8.0.28" (`SourceEngineVersion`).
pub const S3_RESTORE_CLUSTER: S3RestorePair = S3RestorePair {
    engine: "aurora-mysql",
    source_engine: "mysql",
    source_engine_versions: &["5.7", "8.0"],
    bucket_unreachable_message: CLUSTER_BUCKET_UNREACHABLE_MESSAGE,
};

/// What `RestoreDBInstanceFromS3` migrates: "Valid Values: mysql" for both `Engine` and
/// `SourceEngine`, and the MySQL series the import reads a backup of.
///
/// The member's own example text ("MySQL versions 5.6 and 5.7 are supported. Example: 5.6.40")
/// trails the engine's releases: the recorded front door accepts a restore whose backup was
/// taken from MySQL 8.0 and goes on to resolve the ingestion role, so the series it reads are
/// the MySQL major-version lines this service publishes engine versions of
/// (`crate::ops::describe_db_engine_versions`) rather than the two that example names. The
/// cluster-level restore reads the same lines minus the one the Aurora MySQL migration does
/// not ([`S3_RESTORE_CLUSTER`]).
pub const S3_RESTORE_INSTANCE: S3RestorePair = S3RestorePair {
    engine: "mysql",
    source_engine: "mysql",
    source_engine_versions: &["5.6", "5.7", "8.0"],
    bucket_unreachable_message: INSTANCE_BUCKET_UNREACHABLE_MESSAGE,
};

impl S3RestorePair {
    /// How this restore refuses the engine triple a request carried, or `None` where the
    /// triple is the pair this operation migrates.
    ///
    /// Amazon RDS words the refusal as the restore it will not perform, naming the two engines
    /// the request put on either side of it — and, where the engines hold but the version the
    /// backup was taken from is not one the migration reads, that version beside the source
    /// engine. Every value named comes from the request.
    #[must_use]
    pub fn refusal(&self, engine: &str, source_engine: &str, version: &str) -> Option<String> {
        if !engine.eq_ignore_ascii_case(self.engine)
            || !source_engine.eq_ignore_ascii_case(self.source_engine)
        {
            return Some(format!(
                "You can't restore to {engine} from an S3 backup of {source_engine}."
            ));
        }
        if !self.reads_source_version(version) {
            return Some(format!(
                "You can't restore to {engine} from an S3 backup of {source_engine} {version}."
            ));
        }
        None
    }

    /// Whether `version` names one of the source engine versions this restore reads: a
    /// published series, bare ("5.7") or carrying its patch component ("5.7.40").
    #[must_use]
    pub fn reads_source_version(&self, version: &str) -> bool {
        self.source_engine_versions
            .iter()
            .any(|series| match version.strip_prefix(series) {
                Some("") => true,
                Some(rest) => rest.starts_with('.'),
                None => false,
            })
    }
}

/// What a refusal of a source location the restore cannot FIND OR READ AT ALL carries on the
/// DB CLUSTER restore: "The specified Amazon S3 bucket cannot be found. Make sure that you
/// have created an AWS Identity and Access Management (IAM) role that lets Amazon RDS access
/// Amazon S3 for you."
///
/// `RestoreDBClusterFromS3` states the one thing that is wrong — the bucket is not there —
/// and points at the IAM role a caller has to create for Amazon RDS, which is the wording
/// this operation shares with the refusal of a location its role cannot list
/// ([`ingestion_role_cannot_list_message`]) and NOT the DB instance restore's, which names
/// the two members of ITS own request instead ([`INSTANCE_BUCKET_UNREACHABLE_MESSAGE`]).
///
/// Fixed text with no variable part: it names neither the bucket nor the role.
pub const CLUSTER_BUCKET_UNREACHABLE_MESSAGE: &str =
    "The specified Amazon S3 bucket cannot be found. Make sure that you have created an AWS \
     Identity and Access Management (IAM) role that lets Amazon RDS access Amazon S3 for you.";

/// The same refusal on the DB INSTANCE restore: the sentence the MODEL states for that error
/// class on `RestoreDBInstanceFromS3` — "The specified Amazon S3 bucket name can't be found
/// or Amazon RDS isn't authorized to access the specified Amazon S3 bucket. Verify the
/// SourceS3BucketName and S3IngestionRoleArn values and try again." — which states the two
/// conditions that leave a location unreachable and names the two members of that request a
/// caller can act on.
///
/// One error class, several causes: the CODE is the same for each of them, so the MESSAGE is
/// what tells a caller which cause it was and therefore what to fix. This sentence is the one
/// for a location that cannot be found or read. A location that IS there and readable but
/// holds no backup for the migration to read is a DIFFERENT cause of the same class, and
/// carries its own sentence ([`no_backup_files_message`]) — that one worded alike on both
/// operations; a role that withholds a read of a location that is reachable reports the files
/// it cannot list ([`ingestion_role_cannot_list_message`]).
///
/// It carries no variable part of its own: the bucket and the role are values the request
/// already holds, and the sentence names the MEMBERS rather than their values.
pub const INSTANCE_BUCKET_UNREACHABLE_MESSAGE: &str =
    "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to \
     access the specified Amazon S3 bucket. Verify the SourceS3BucketName and \
     S3IngestionRoleArn values and try again.";

/// What a refusal of a source location that IS there and readable but holds NO BACKUP FILES
/// carries: "No backup files were found in the specified Amazon S3 bucket. Check the bucket
/// name or create backup files and try again."
///
/// The other cause of the error class the unreachable-bucket sentences word, and the
/// reason the two are worded apart: a caller told to verify the bucket name and the ingestion
/// role would be checking two values that are already right, when what the location is
/// missing is the backup itself — the files the migration reads. This sentence states that,
/// and the remedy for it.
///
/// Fixed text with no variable part: it names neither the bucket nor the prefix nor the role,
/// so no value the request carried appears in it.
#[must_use]
pub fn no_backup_files_message() -> String {
    "No backup files were found in the specified Amazon S3 bucket. Check the bucket name or \
     create backup files and try again."
        .to_string()
}

/// What a restore from Amazon S3 cannot read its backup out of: the two conditions a source
/// location is refused for, which Amazon RDS answers differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestionLocationDefect {
    /// This Region's object store holds NO bucket of that name, or holds one that belongs to
    /// another Region: the value `SourceS3BucketName`/`S3BucketName` carried names no bucket
    /// the restore can reach, so it is refused as the parameter value it is — the Query
    /// API's `InvalidParameterValue`, carrying the sentence the operation that answers it
    /// words a location it cannot find in ([`S3RestorePair::bucket_unreachable_message`]).
    BucketUnreachable,
    /// The bucket IS there and is readable, and holds no file under the prefix the request
    /// named, so there is no backup under it for the migration to read: the modelled
    /// `InvalidS3BucketFault`, carrying the sentence that reports the missing backup files
    /// rather than the one about a bucket that cannot be found or read
    /// ([`no_backup_files_message`]).
    NoBackupUnderPrefix,
}

/// Whether the object store on this engine holds NO data for a restore to migrate at
/// `bucket`/`prefix`, and which of the two conditions that is.
///
/// The bucket's existence and its Region are the questions this crate already resolves a
/// destination bucket with ([`super::bucket_is_absent`], [`super::bucket_region`]), so they
/// are asked there rather than a second time here; the prefix is the one question this path
/// adds. `S3Prefix` is "the prefix for all of the file names that contain the data used to
/// create the Amazon Aurora DB cluster", and a bucket and prefix that name no file at all
/// contain no backup for the migration to read. WHAT those files hold is read by the
/// migration itself, afterwards, so it decides nothing here.
///
/// `None` means nothing was established — either there is no object store beside this
/// service to ask, or the location holds a backup — so a refusal grounded in this answer is
/// only given where that store really answered.
#[must_use]
pub fn ingestion_location_defect<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    bucket: &str,
    prefix: Option<&str>,
) -> Option<IngestionLocationDefect> {
    if super::bucket_is_absent(engine, bucket)? {
        return Some(IngestionLocationDefect::BucketUnreachable);
    }
    // "The Amazon S3 bucket … must be in the same Amazon Web Services Region as the DB
    // cluster": a bucket this Region's endpoint does not reach is a bucket this restore
    // cannot be run with, the same parameter refusal as one that is not there at all.
    if super::bucket_region(engine, bucket).is_some_and(|held| !held.is_empty() && held != region) {
        return Some(IngestionLocationDefect::BucketUnreachable);
    }
    let state = ::service_com_amazonaws_s3_amazons3::state::service_state(engine)?;
    let store = state.lock();
    let held = store.bucket(bucket)?;
    let under = prefix.unwrap_or_default();
    let holds_a_file = held.objects().any(|(key, _)| key.starts_with(under));
    (!holds_a_file).then_some(IngestionLocationDefect::NoBackupUnderPrefix)
}

/// What a restore whose INGESTION ROLE cannot read the backup is refused with: the sentence
/// Amazon RDS words a role that leaves the location unreadable in — "Files from the specified
/// Amazon S3 bucket cannot be listed. Make sure that you have created an AWS Identity and
/// Access Management (IAM) role that lets Amazon RDS access Amazon S3 for you." — under the
/// Query API's `InvalidParameterValue`, since what the caller has to act on is the value
/// `S3IngestionRoleArn` carried.
///
/// It carries no variable part: the sentence reports the one thing that is wrong — the files
/// of the backup cannot be listed under the role the request named — and points at the role a
/// caller has to create for Amazon RDS, rather than naming the bucket or the role's own value
/// back.
#[must_use]
pub fn ingestion_role_cannot_list_message() -> String {
    "Files from the specified Amazon S3 bucket cannot be listed. Make sure that you have \
     created an AWS Identity and Access Management (IAM) role that lets Amazon RDS access \
     Amazon S3 for you."
        .to_string()
}

/// The permission set a restore from Amazon S3 reads its backup under, as Amazon RDS names it
/// in the refusal of a role it cannot use ([`unusable_ingestion_role_message`]): the feature
/// the ingestion role's trust and permissions are required FOR.
pub const S3_SNAPSHOT_INGESTION_FEATURE: &str = "S3_SNAPSHOT_INGESTION";

/// What a restore whose `S3IngestionRoleArn` is not a role this restore can be run under at
/// all is refused with: "IAM role ARN value is invalid or does not include the required
/// permissions for: S3_SNAPSHOT_INGESTION".
///
/// This is the refusal of the VALUE as a role — a value that is no IAM role ARN of this
/// account, or one IAM holds no such role under, or one whose trust does not let Amazon RDS
/// act as it — and it is decided before the location the role would read is resolved: there is
/// no role to read anything under. The sentence states both of the things that make a value
/// unusable (it is not a valid role ARN, or the role it names does not carry what the feature
/// requires) and names the feature the permissions are required for, so a caller knows which
/// role to fix and for what.
///
/// A role that IS usable and whose permissions withhold one of the reads the migration makes
/// is a DIFFERENT refusal of the same class, reported after the location resolves and worded
/// as the files it cannot list ([`ingestion_role_cannot_list_message`]).
///
/// Fixed text with no variable part: it names neither the role nor the bucket, only the
/// feature the role is required for.
#[must_use]
pub fn unusable_ingestion_role_message() -> String {
    format!(
        "IAM role ARN value is invalid or does not include the required permissions for: \
         {S3_SNAPSHOT_INGESTION_FEATURE}"
    )
}

/// Whether `role_arn` is not a role a restore from Amazon S3 can read its backup UNDER:
/// either the value is no IAM role ARN of this account, or IAM answers that this account holds
/// no such role or that its trust policy does not let Amazon RDS assume it.
///
/// `S3IngestionRoleArn` is the role Amazon RDS acts as while it reads the backup, so the
/// restore is validated against that role BEFORE it reads anything: a value that names no role
/// of this account is not an ARN this restore could ever assume, and a role this account's IAM
/// answers is absent or untrusting is one Amazon RDS cannot act as. Both are the one refusal
/// [`unusable_ingestion_role_message`] words — which is also what the real front door answers
/// while a freshly created role has not propagated to it — and no resource is created for a
/// request carrying one.
///
/// The IAM half is this crate's one reading of that question
/// ([`::service_common::iam_role::role_is_unassumable`]), asked about the principal Amazon RDS
/// acts under ([`super::RDS_SERVICE_PRINCIPAL`], the same principal an associated role of a DB
/// instance or DB cluster is resolved against), so nothing is refused on evidence IAM did not
/// give: an emulator with no IAM beside this service, and an answer it could not read, refuse
/// nothing.
pub async fn ingestion_role_is_unusable(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> bool {
    if ::service_common::iam_role::role_name(role_arn).is_none() {
        return true;
    }
    ::service_common::iam_role::role_is_unassumable(
        ctx,
        region,
        role_arn,
        super::RDS_SERVICE_PRINCIPAL,
    )
    .await
}

/// The Amazon S3 action a restore performs to FIND the backup's files: the listing of the
/// bucket under the prefix the request named.
///
/// Source: the permissions policy the Amazon RDS User Guide asks an ingestion role to carry
/// ("Creating an IAM role manually", Migrating data from MySQL by using an Amazon S3 bucket),
/// whose statement allows `s3:ListBucket` on the bucket and `s3:GetObject` on its objects.
///
/// Source: the permissions policy the Amazon RDS User Guide asks an ingestion role to carry
/// ("Creating an IAM role manually", Migrating data from MySQL by using an Amazon S3 bucket),
/// whose statement allows `s3:ListBucket` on the bucket and `s3:GetObject` on its objects.
pub const INGESTION_LIST_ACTION: &str = "s3:ListBucket";

/// The Amazon S3 action a restore performs to READ each of the backup's files, from the same
/// policy [`INGESTION_LIST_ACTION`] is taken from.
pub const INGESTION_READ_ACTION: &str = "s3:GetObject";

/// How many of the backup's files one restore asks IAM about. The migration reads them all,
/// and a grant is per object, so the files under the prefix are asked about rather than
/// assumed from one another — bounded here so a bucket holding a large backup costs a bounded
/// number of decisions, with the first files in key order the ones asked about.
const MAX_INGESTION_OBJECTS_CHECKED: usize = 8;

/// Whether IAM ANSWERS that the ingestion role `role_arn` names may not perform an Amazon S3
/// read this restore makes on the backup at `bucket`/`prefix`.
///
/// `S3IngestionRoleArn` is the role "that authorizes Amazon RDS to access the Amazon S3
/// bucket on your behalf" — "a trust policy and a permissions policy that allows Amazon RDS
/// to access your Amazon S3 bucket" — so the reads the migration makes are authorized against
/// THAT role's permissions and not against the caller's own: the listing that finds the
/// backup's files ([`INGESTION_LIST_ACTION`], on the bucket) and the read of each file it
/// found ([`INGESTION_READ_ACTION`], on the object). A role whose permissions withhold one of
/// them cannot read the backup, so the restore could never have run under it.
///
/// Nothing is refused on evidence this emulator does not have, the reading
/// [`super::data_plane::engine_s3_role`] takes of the same question for an engine's own S3
/// statement: an explicit `Deny` is an answer about the role whatever else it carries, while
/// a policy set that merely STATES NOTHING about the read is an answer only where the role
/// states permissions at all — a role provisioned out of band routinely carries none. A value
/// that names no role of this account, a role this account's IAM holds nothing under, and an
/// emulator with no IAM to ask all establish nothing and refuse nothing.
///
/// The questions go to IAM itself over the in-process hop
/// ([`::service_common::iam_role::role_action_decision_through`]), so the documents in force
/// are collected and decided by the service that holds them.
pub async fn ingestion_role_is_unauthorized<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
    bucket: &str,
    prefix: Option<&str>,
) -> bool {
    let mut reads = vec![(
        INGESTION_LIST_ACTION,
        ::service_com_amazonaws_s3_amazons3::state::bucket_arn(bucket),
    )];
    reads.extend(
        backup_object_keys(engine, bucket, prefix)
            .into_iter()
            .map(|key| {
                (
                    INGESTION_READ_ACTION,
                    ::service_com_amazonaws_s3_amazons3::state::object_arn(bucket, &key),
                )
            }),
    );
    for (action, resource) in reads {
        if role_withholds_ingestion_read(ctx, region, role_arn, action, &resource).await {
            return true;
        }
    }
    false
}

/// Whether the permissions in force on `role_arn` withhold `action` on `resource`, read as
/// [`ingestion_role_is_unauthorized`] documents: an explicit deny always, an implicit one only
/// from a role that states permissions at all.
async fn role_withholds_ingestion_read(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
    action: &str,
    resource: &str,
) -> bool {
    match ::service_common::iam_role::role_action_decision_through(
        ctx.callback(),
        region,
        role_arn,
        action,
        resource,
        &[],
    )
    .await
    {
        Some(::service_common::iam_authorization::Decision::Denied) => true,
        Some(::service_common::iam_authorization::Decision::NotStated) => {
            ::service_common::iam_role::role_states_permissions(ctx, region, role_arn).await
        }
        Some(::service_common::iam_authorization::Decision::Allowed) | None => false,
    }
}

/// The files of the backup at `bucket`/`prefix`, in key order, bounded by
/// [`MAX_INGESTION_OBJECTS_CHECKED`] — the objects the migration would read, as the object
/// store beside this service holds them.
///
/// Empty where nothing can be read: no object store beside this service, no bucket of that
/// name, or no file under the prefix. The store's guard is released before this returns, so
/// the decisions the keys are used for are made with no lock held.
fn backup_object_keys<Eng: Send + Sync + 'static>(
    engine: &Eng,
    bucket: &str,
    prefix: Option<&str>,
) -> Vec<String> {
    let Some(state) = ::service_com_amazonaws_s3_amazons3::state::service_state(engine) else {
        return Vec::new();
    };
    let store = state.lock();
    let Some(held) = store.bucket(bucket) else {
        return Vec::new();
    };
    let under = prefix.unwrap_or_default();
    let mut keys: Vec<String> = held
        .objects()
        .map(|(key, _)| key)
        .filter(|key| key.starts_with(under))
        .map(str::to_string)
        .collect();
    keys.sort();
    keys.truncate(MAX_INGESTION_OBJECTS_CHECKED);
    keys
}

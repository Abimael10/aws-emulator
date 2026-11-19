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

/// The engine pair ONE restore-from-Amazon-S3 operation migrates between, as that operation's
/// own members publish it.
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
}

/// What `RestoreDBClusterFromS3` migrates: "Valid Values: aurora-mysql (for Aurora MySQL)"
/// (`Engine`), "Valid Values: mysql" (`SourceEngine`) and "MySQL versions 5.7 and 8.0 are
/// supported. Example: 5.7.40, 8.0.28" (`SourceEngineVersion`).
pub const S3_RESTORE_CLUSTER: S3RestorePair = S3RestorePair {
    engine: "aurora-mysql",
    source_engine: "mysql",
    source_engine_versions: &["5.7", "8.0"],
};

/// What `RestoreDBInstanceFromS3` migrates: "Valid Values: mysql" for both `Engine` and
/// `SourceEngine`, and "MySQL versions 5.6 and 5.7 are supported. Example: 5.6.40"
/// (`SourceEngineVersion`).
pub const S3_RESTORE_INSTANCE: S3RestorePair = S3RestorePair {
    engine: "mysql",
    source_engine: "mysql",
    source_engine_versions: &["5.6", "5.7"],
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

/// What a refusal of the LOCATION a restore reads carries, whichever of the two conditions
/// it was: the sentence the MODEL states for that error class on these two operations — "The
/// specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to access
/// the specified Amazon S3 bucket. Verify the SourceS3BucketName and S3IngestionRoleArn
/// values and try again." — which states the two conditions that leave a location unusable
/// and names the two members a caller can act on.
///
/// One sentence, two codes: WHICH code the refusal carries is what says whether the value
/// named a bucket at all ([`IngestionLocationDefect`]), while what is wrong is the one thing
/// this sentence reports — the location the restore cannot read a backup out of.
///
/// It carries no variable part of its own: the bucket and the role are values the request
/// already holds, and the sentence names the MEMBERS rather than their values.
#[must_use]
pub fn invalid_ingestion_bucket_message() -> String {
    "The specified Amazon S3 bucket name can't be found or Amazon RDS isn't authorized to \
     access the specified Amazon S3 bucket. Verify the SourceS3BucketName and \
     S3IngestionRoleArn values and try again."
        .to_string()
}

/// What a restore from Amazon S3 cannot read its backup out of: the two conditions a source
/// location is refused for, which Amazon RDS answers differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestionLocationDefect {
    /// This Region's object store holds NO bucket of that name, or holds one that belongs to
    /// another Region: the value `SourceS3BucketName`/`S3BucketName` carried names no bucket
    /// the restore can reach, so it is refused as the parameter value it is — the Query
    /// API's `InvalidParameterValue`, carrying the sentence this operation words a location
    /// it cannot find in ([`invalid_ingestion_bucket_message`]).
    BucketUnreachable,
    /// The bucket IS there and holds no file under the prefix the request named, so there is
    /// no backup under it for the migration to read: the modelled `InvalidS3BucketFault`,
    /// whose own sentence reports a location that "can't be found" and names the two members
    /// a caller can act on ([`invalid_ingestion_bucket_message`]).
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

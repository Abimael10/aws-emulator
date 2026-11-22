// SPDX-License-Identifier: CC0-1.0

//! The Amazon Rekognition Custom Labels DATASETS a project holds: the record `CreateDataset`
//! writes, the status the clock owes it, and the two modelled shapes a dataset is reported as.
//!
//! ## The rule this module implements
//!
//! A dataset belongs to ONE project and to one half of that project's data: "to create a
//! training dataset for a project, specify `TRAIN` for the value of `DatasetType`. To create
//! the test dataset for a project, specify `TEST`"
//! (`smithy_query(query="doc",shape="CreateDataset")`). So a project has at most one dataset of
//! each type, which is what makes the `DatasetType` the project's key for one, and a second
//! create of the same half is the `ResourceAlreadyExistsException` that operation models.
//!
//! The dataset's identity is the ARN this service mints, whose form its own member publishes:
//! `project/<ProjectName>/dataset/(train|test)/<version>` (`DatasetArn`'s pattern,
//! `smithy_query(query="operation",operation="CreateDataset")`). The trailing numeric segment is
//! the dataset's creation instant in milliseconds, for the same reason a project ARN's is
//! ([`super::project_arn`]): it is derived from the record this service just wrote and stays
//! that dataset's ARN for as long as the dataset lives.
//!
//! Creating one is ASYNCHRONOUS — "creating a dataset takes a while to complete. Use
//! `DescribeDataset` to check the current status. The dataset created successfully if the value
//! of `Status` is `CREATE_COMPLETE`" — so a created dataset is
//! [`DATASET_STATUS_CREATE_IN_PROGRESS`] and the clock moves it to
//! [`DATASET_STATUS_CREATE_COMPLETE`] one [`DATASET_CREATION_SPAN`] later. The instant is
//! STORED rather than waited for: nothing sleeps, and the status a reader reports is computed
//! from the clock whenever it is asked for.
//!
//! What a dataset HOLDS is [`DatasetContents`], and it is counted rather than invented: the
//! entries of a dataset created from a Ground Truth manifest are the JSON Lines of the object
//! this emulator's own Amazon S3 serves, read and counted by [`super::dataset_source`], and a
//! dataset created with no source at all holds nothing, so every one of its counts is zero.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::ops::types::{DatasetDescription, DatasetMetadata, DatasetStats};

/// The two halves of a project's data, from the modelled `DatasetType` enum
/// (`smithy_query(query="shape",shape="DatasetType")`): `[TRAIN, TEST]`.
pub const DATASET_TYPE_TRAIN: &str = "TRAIN";
pub const DATASET_TYPE_TEST: &str = "TEST";

/// Every `DatasetType` a create may ask for, which is what a project keys a dataset by.
pub const DATASET_TYPES: &[&str] = &[DATASET_TYPE_TRAIN, DATASET_TYPE_TEST];

/// The `DatasetStatus` a dataset being created is in, and the one its creation finishes in,
/// from the modelled `DatasetStatus` enum (`smithy_query(query="shape",shape="DatasetStatus")`).
pub const DATASET_STATUS_CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
pub const DATASET_STATUS_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `DatasetStatus` a dataset being DELETED is in, from the same enum: "deleting a dataset
/// might take while. Use `DescribeDataset` to check the current status. The dataset is still
/// deleting if the value of `Status` is `DELETE_IN_PROGRESS`. If you try to access the dataset
/// after it is deleted, you get a `ResourceNotFoundException` exception"
/// (`smithy_query(query="doc",shape="DeleteDataset")`).
pub const DATASET_STATUS_DELETE_IN_PROGRESS: &str = "DELETE_IN_PROGRESS";

/// The `DatasetStatusMessageCode` of a dataset operation that went through, from the modelled
/// `DatasetStatusMessageCode` enum (`[SUCCESS, SERVICE_ERROR, CLIENT_ERROR]`).
pub const DATASET_STATUS_MESSAGE_CODE_SUCCESS: &str = "SUCCESS";

/// How long after the create a dataset's creation is complete.
///
/// `CreateDataset` is documented asynchronous — "creating a dataset takes a while to complete"
/// — so a dataset is reported `CREATE_IN_PROGRESS` first and `CREATE_COMPLETE` from this span
/// on. The span is this emulator's own choice of how long "a while" is; it is not zero,
/// because a caller acting straight after the create must still see the transient status the
/// documentation tells it to poll for.
pub const DATASET_CREATION_SPAN: Duration = Duration::from_secs(3);

/// How long after the delete a dataset is gone.
///
/// `DeleteDataset` is documented asynchronous the same way the create is — "deleting a dataset
/// might take while" — so a deleted dataset is reported
/// [`DATASET_STATUS_DELETE_IN_PROGRESS`] and from this span on no reader holds it at all. The
/// span is this emulator's own choice of how long "a while" is, and it matches the create's for
/// the same reason: a caller acting straight after the delete must still see the transient
/// status the documentation tells it to poll for.
pub const DATASET_DELETION_SPAN: Duration = DATASET_CREATION_SPAN;

/// How many datasets one project can hold: one per entry of [`DATASET_TYPES`], which is what
/// the create's own documentation says a project has.
pub const MAX_DATASETS_PER_PROJECT: usize = DATASET_TYPES.len();

/// What a dataset HOLDS, counted off the source it was created from.
///
/// The four counts are the members `DatasetStats` publishes ("the total number of images in the
/// dataset that have labels", "the total number of images in the dataset", "the total number of
/// labels declared in the dataset", "the total number of entries that contain at least one
/// error"), and the LABELS are kept as the set of distinct names rather than as a number so
/// that a dataset copied from another reports the same labels rather than a recount of nothing.
#[derive(Debug, Clone, Default)]
pub struct DatasetContents {
    /// Every entry of the dataset that names an image, counted.
    pub total_entries: u32,
    /// Those of them that declare at least one label.
    pub labeled_entries: u32,
    /// Those entries the source declared that could not be read as entries at all.
    pub error_entries: u32,
    /// The distinct label names the entries declare, which is what `TotalLabels` counts.
    pub labels: BTreeSet<String>,
}

impl DatasetContents {
    /// These contents as the modelled `DatasetStats`: the four counts, each read off what the
    /// dataset really holds.
    pub fn modeled(&self) -> DatasetStats {
        DatasetStats {
            labeled_entries: Some(i32::try_from(self.labeled_entries).unwrap_or(i32::MAX)),
            total_entries: Some(i32::try_from(self.total_entries).unwrap_or(i32::MAX)),
            total_labels: Some(i32::try_from(self.labels.len()).unwrap_or(i32::MAX)),
            error_entries: Some(i32::try_from(self.error_entries).unwrap_or(i32::MAX)),
        }
    }
}

/// One dataset recorded by `CreateDataset`: which half of its project's data it is, the ARN
/// this service minted for it, when it was created, when the clock finishes creating it, and
/// what it holds.
#[derive(Debug, Clone)]
pub struct Dataset {
    /// `CreateDataset.DatasetType` — the project's key for the dataset.
    pub dataset_type: String,
    /// The `DatasetArn` this service minted; what every operation ON a dataset names it by.
    pub arn: String,
    pub creation_timestamp: SystemTime,
    /// The instant from which this dataset's creation is complete
    /// ([`DATASET_CREATION_SPAN`]). Stored rather than waited for: the status every reader
    /// reports is computed from it against the clock.
    pub created_at: SystemTime,
    /// What the dataset holds, counted off the source it was created from.
    pub contents: DatasetContents,
    /// When the deletion `DeleteDataset` started is complete — the instant from which this
    /// dataset is one the account no longer has — and `None` while no call has asked for one.
    /// The instant is STORED rather than waited for, exactly as [`Self::created_at`] is.
    pub deleting_until: Option<SystemTime>,
    /// The tags the create attached to the dataset, and the set the operations that report and
    /// change a resource's tags work on when the ARN they are given is this dataset's.
    pub tags: BTreeMap<String, String>,
}

impl Dataset {
    /// A dataset as `CreateDataset` records it: created now, complete one
    /// [`DATASET_CREATION_SPAN`] later, holding what its source declared and the tags the
    /// create asked for.
    pub fn new(
        dataset_type: String,
        arn: String,
        contents: DatasetContents,
        tags: BTreeMap<String, String>,
        creation_timestamp: SystemTime,
    ) -> Self {
        Self {
            dataset_type,
            arn,
            creation_timestamp,
            created_at: creation_timestamp + DATASET_CREATION_SPAN,
            contents,
            deleting_until: None,
            tags,
        }
    }

    /// Record that the deletion of this dataset started at `now`, due to be complete one
    /// [`DATASET_DELETION_SPAN`] later.
    ///
    /// A dataset already being deleted keeps the instant the first call owed it: asking for a
    /// deletion that is already under way does not postpone it.
    pub fn start_deletion(&mut self, now: SystemTime) {
        self.deleting_until
            .get_or_insert(now + DATASET_DELETION_SPAN);
    }

    /// Whether a deletion of this dataset has been asked for.
    pub fn is_deleting(&self) -> bool {
        self.deleting_until.is_some()
    }

    /// Whether the clock has completed this dataset's deletion by `now`: from that instant the
    /// dataset is one the account no longer has, so no reader reports it and its ARN resolves
    /// to nothing.
    pub fn is_deleted_at(&self, now: SystemTime) -> bool {
        self.deleting_until.is_some_and(|until| now >= until)
    }

    /// This dataset's `DatasetStatus` at `now`: being deleted once a call has asked for its
    /// deletion, still being created until the clock owes its completion, and created from
    /// that instant on. A dataset the clock has finished deleting is reported by nothing, so
    /// no reader sees a status past that instant.
    pub fn status_at(&self, now: SystemTime) -> &'static str {
        if self.is_deleting() {
            DATASET_STATUS_DELETE_IN_PROGRESS
        } else if now >= self.created_at {
            DATASET_STATUS_CREATE_COMPLETE
        } else {
            DATASET_STATUS_CREATE_IN_PROGRESS
        }
    }

    /// The `StatusMessageCode` the dataset's last operation reports: its creation is the only
    /// operation this service performs on one, and a recorded dataset is one whose creation
    /// went through, so the code is the success one.
    pub fn status_message_code(&self) -> &'static str {
        DATASET_STATUS_MESSAGE_CODE_SUCCESS
    }

    /// This dataset as the modelled `DatasetDescription` `DescribeDataset` answers with.
    ///
    /// `LastUpdatedTimestamp` is the dataset's creation instant: nothing has changed it since,
    /// `UpdateDatasetEntries` being the one operation that would and still an unimplemented
    /// 501.
    pub fn described_at(&self, now: SystemTime) -> DatasetDescription {
        DatasetDescription {
            creation_timestamp: Some(::aws_smithy_types::DateTime::from(self.creation_timestamp)),
            last_updated_timestamp: Some(::aws_smithy_types::DateTime::from(
                self.creation_timestamp,
            )),
            status: Some(self.status_at(now).to_string()),
            status_message: None,
            status_message_code: Some(self.status_message_code().to_string()),
            dataset_stats: Some(self.contents.modeled()),
        }
    }

    /// This dataset as the modelled `DatasetMetadata` a `ProjectDescription` carries it as:
    /// the summary of the same record [`Self::described_at`] describes in full.
    pub fn summarised_at(&self, now: SystemTime) -> DatasetMetadata {
        DatasetMetadata {
            creation_timestamp: Some(::aws_smithy_types::DateTime::from(self.creation_timestamp)),
            dataset_type: Some(self.dataset_type.clone()),
            dataset_arn: Some(self.arn.clone()),
            status: Some(self.status_at(now).to_string()),
            status_message: None,
            status_message_code: Some(self.status_message_code().to_string()),
        }
    }
}

/// The ARN of the `dataset_type` dataset of the project named `project_name` in `region`,
/// created at `creation_timestamp`:
/// `arn:aws:rekognition:<region>:<account>:project/<ProjectName>/dataset/(train|test)/<version>`.
///
/// The form is the one `DatasetArn`'s own pattern requires —
/// `project\/[a-zA-Z0-9_.\-]{1,255}\/dataset\/(train|test)\/[0-9]+$`
/// (`smithy_query(query="operation",operation="CreateDataset")`) — so the type segment goes into
/// the path LOWER-CASED, and the trailing numeric segment is the dataset's creation instant in
/// milliseconds.
pub fn dataset_arn(
    region: &str,
    project_name: &str,
    dataset_type: &str,
    creation_timestamp: SystemTime,
) -> String {
    let version = creation_timestamp
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default();
    ::engine::emulator_arn(
        super::ARN_NAMESPACE,
        region,
        &format!(
            "project/{project_name}/dataset/{}/{version}",
            dataset_type.to_ascii_lowercase()
        ),
    )
}

// SPDX-License-Identifier: CC0-1.0

//! The ONE projection of a stored record onto the members a response reports.
//!
//! Every operation that reports a file system or an access point answers through the
//! view below, so a create and every later read project the same stored record the same
//! way instead of each hand-rolling the member list. Two reporting rules live here
//! rather than in each handler:
//!
//! * [`reported_tags`] — a response member reporting the tags a resource carries is
//!   ALWAYS serialized, as an EMPTY list for a resource no write has tagged. `TagList`
//!   is `@length(1, 50)`, which bounds the tag set a tag WRITE may carry; it does not
//!   make the reported collection optional, so a client that iterates the list
//!   unconditionally never reads a null where a list is guaranteed.
//! * [`reported_creation_time`] — a creation timestamp is reported in WHOLE epoch
//!   seconds. `CreateFileSystemResponse$creationTime` is documented "The time when the
//!   file system was created, in seconds since 1970-01-01T00:00:00Z (Unix epoch time)"
//!   and this service's protocol (restJson1) encodes a timestamp as epoch seconds, so
//!   the reported value is exactly what that encoding round-trips: the second the
//!   creation falls in, carrying no sub-second remainder of the emulator's clock.

use std::time::{Duration, SystemTime};

use super::{AccessPoint, FileSystem};
use crate::ops::types::{
    CreateAccessPointOutput, CreateFileSystemOutput, GetAccessPointOutput, GetFileSystemOutput,
    PosixUser, RootDirectory, Tag,
};

/// The `tags` member of a response reporting what a resource carries: the whole stored
/// set, and an empty list when the resource carries none.
pub fn reported_tags(tags: &[Tag]) -> Option<Vec<Tag>> {
    Some(tags.to_vec())
}

/// A stored instant as a reported timestamp: the whole epoch second it falls in.
///
/// A clock reading before the Unix epoch has no epoch-seconds truncation to make, so it
/// is reported as read rather than moved to an instant it never was.
pub fn reported_creation_time(instant: SystemTime) -> ::aws_smithy_types::DateTime {
    let truncated = instant
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(instant, |since_epoch| {
            SystemTime::UNIX_EPOCH + Duration::from_secs(since_epoch.as_secs())
        });
    ::aws_smithy_types::DateTime::from(truncated)
}

/// Every member a file-system response carries, taken off the stored record.
///
/// `CreateFileSystemResponse` and `GetFileSystemResponse` model the identical member
/// set, so both are built from this one view: `GetFileSystem` cannot report a file
/// system differently from the `CreateFileSystem` that stored it.
#[derive(Debug, Clone)]
pub struct FileSystemView {
    pub creation_time: ::aws_smithy_types::DateTime,
    pub file_system_arn: String,
    pub file_system_id: String,
    pub bucket: String,
    pub prefix: String,
    pub client_token: Option<String>,
    pub kms_key_id: Option<String>,
    pub status: String,
    pub status_message: Option<String>,
    pub role_arn: String,
    pub owner_id: String,
    pub tags: Option<Vec<Tag>>,
    pub name: Option<String>,
}

impl From<&FileSystem> for FileSystemView {
    fn from(record: &FileSystem) -> Self {
        Self {
            creation_time: reported_creation_time(record.creation_time),
            file_system_arn: record.file_system_arn.clone(),
            file_system_id: record.file_system_id.clone(),
            bucket: record.bucket.clone(),
            prefix: record.prefix.clone(),
            client_token: record.client_token.clone(),
            kms_key_id: record.kms_key_id.clone(),
            status: record.status.clone(),
            status_message: record.status_message.clone(),
            role_arn: record.role_arn.clone(),
            owner_id: record.owner_id.clone(),
            tags: reported_tags(&record.tags),
            name: record.name.clone(),
        }
    }
}

impl From<FileSystemView> for CreateFileSystemOutput {
    fn from(view: FileSystemView) -> Self {
        Self {
            creation_time: Some(view.creation_time),
            file_system_arn: Some(view.file_system_arn),
            file_system_id: Some(view.file_system_id),
            bucket: Some(view.bucket),
            prefix: Some(view.prefix),
            client_token: view.client_token,
            kms_key_id: view.kms_key_id,
            status: Some(view.status),
            status_message: view.status_message,
            role_arn: Some(view.role_arn),
            owner_id: Some(view.owner_id),
            tags: view.tags,
            name: view.name,
        }
    }
}

impl From<FileSystemView> for GetFileSystemOutput {
    fn from(view: FileSystemView) -> Self {
        Self {
            creation_time: Some(view.creation_time),
            file_system_arn: Some(view.file_system_arn),
            file_system_id: Some(view.file_system_id),
            bucket: Some(view.bucket),
            prefix: Some(view.prefix),
            client_token: view.client_token,
            kms_key_id: view.kms_key_id,
            status: Some(view.status),
            status_message: view.status_message,
            role_arn: Some(view.role_arn),
            owner_id: Some(view.owner_id),
            tags: view.tags,
            name: view.name,
        }
    }
}

/// Every member an access-point response carries, taken off the stored record.
///
/// `CreateAccessPointResponse` and `GetAccessPointResponse` model the identical member
/// set, so — exactly as for [`FileSystemView`] — both are built from this one view.
#[derive(Debug, Clone)]
pub struct AccessPointView {
    pub access_point_arn: String,
    pub access_point_id: String,
    pub client_token: String,
    pub file_system_id: String,
    pub status: String,
    pub owner_id: String,
    pub posix_user: Option<PosixUser>,
    pub root_directory: Option<RootDirectory>,
    pub tags: Option<Vec<Tag>>,
    pub name: Option<String>,
}

impl From<&AccessPoint> for AccessPointView {
    fn from(record: &AccessPoint) -> Self {
        Self {
            access_point_arn: record.access_point_arn.clone(),
            access_point_id: record.access_point_id.clone(),
            client_token: record.client_token.clone(),
            file_system_id: record.file_system_id.clone(),
            status: record.status.clone(),
            owner_id: record.owner_id.clone(),
            posix_user: record.posix_user.clone(),
            root_directory: record.root_directory.clone(),
            tags: reported_tags(&record.tags),
            name: record.name.clone(),
        }
    }
}

impl From<AccessPointView> for CreateAccessPointOutput {
    fn from(view: AccessPointView) -> Self {
        Self {
            access_point_arn: view.access_point_arn,
            access_point_id: view.access_point_id,
            client_token: view.client_token,
            file_system_id: view.file_system_id,
            status: view.status,
            owner_id: view.owner_id,
            posix_user: view.posix_user,
            root_directory: view.root_directory,
            tags: view.tags,
            name: view.name,
        }
    }
}

impl From<AccessPointView> for GetAccessPointOutput {
    fn from(view: AccessPointView) -> Self {
        Self {
            access_point_arn: view.access_point_arn,
            access_point_id: view.access_point_id,
            client_token: view.client_token,
            file_system_id: view.file_system_id,
            status: view.status,
            owner_id: view.owner_id,
            posix_user: view.posix_user,
            root_directory: view.root_directory,
            tags: view.tags,
            name: view.name,
        }
    }
}

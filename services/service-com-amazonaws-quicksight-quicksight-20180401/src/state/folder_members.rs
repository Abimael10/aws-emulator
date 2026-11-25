// SPDX-License-Identifier: CC0-1.0

//! The assets a folder holds: which dashboard, analysis, dataset, data source or topic
//! is a member of which folder.
//!
//! A membership is a child of one folder and names one asset of the same account, so it
//! is keyed by the folder's own key with the member under it,
//! `(AwsAccountId, FolderId, MemberType, MemberId)` — the four path labels a membership
//! call addresses, the way a refresh schedule is keyed by the data set it belongs to. The
//! member type is part of that key because an id identifies an asset only together with
//! the kind of asset it is: a dashboard and a dataset of the same id are two assets, and a
//! folder can hold both.
//!
//! What a membership read reports about a member is its own ARN, built from the kind of
//! asset the `MemberType` names, so a member is addressed by the asset it names rather
//! than by an address of the membership itself.
//!
//! A membership does not outlive the folder holding it: it goes when that folder goes.

use super::*;
use crate::ops::types::{FolderMember, MemberIdArnPair};

impl ComAmazonawsQuicksightQuicksight20180401State {
    /// The membership the folder `(aws_account_id, folder_id)` holds under
    /// `(member_type, member_id)`, or `None` when that folder holds no such member — the
    /// record every membership read answers from.
    pub fn folder_member(
        &self,
        aws_account_id: &str,
        folder_id: &str,
        member_type: &str,
        member_id: &str,
    ) -> Option<&FolderMemberRecord> {
        self.folder_members.get(&folder_member_key(
            aws_account_id,
            folder_id,
            member_type,
            member_id,
        ))
    }

    /// How many members the folder `(aws_account_id, folder_id)` holds — what
    /// [`MAX_FOLDER_MEMBERS_PER_FOLDER`] is checked against.
    pub fn folder_member_count(&self, aws_account_id: &str, folder_id: &str) -> usize {
        self.folder_members
            .keys()
            .filter(|(account, member_folder, _, _)| {
                account == aws_account_id && member_folder == folder_id
            })
            .count()
    }

    /// The memberships the folder `(aws_account_id, folder_id)` holds, ordered by the
    /// member's ARN — the order `ListFolderMembers` pages through.
    ///
    /// The ARN is what orders them because it is the member's whole address: it carries
    /// the kind of asset as well as its id, so two members of the same id and different
    /// kinds hold two distinct, stable positions and a page token naming one of them
    /// names exactly one member.
    pub fn folder_members_in(
        &self,
        aws_account_id: &str,
        folder_id: &str,
    ) -> Vec<&FolderMemberRecord> {
        let mut members: Vec<&FolderMemberRecord> = self
            .folder_members
            .iter()
            .filter(|((account, member_folder, _, _), _)| {
                account == aws_account_id && member_folder == folder_id
            })
            .map(|(_, record)| record)
            .collect();
        members.sort_by(|left, right| left.arn.cmp(&right.arn));
        members
    }

    /// Removes the membership the folder `(aws_account_id, folder_id)` holds under
    /// `(member_type, member_id)`, answering the record that went — `None`, removing
    /// nothing, when that folder holds no such member.
    pub fn remove_folder_member(
        &mut self,
        aws_account_id: &str,
        folder_id: &str,
        member_type: &str,
        member_id: &str,
    ) -> Option<FolderMemberRecord> {
        self.folder_members.remove(&folder_member_key(
            aws_account_id,
            folder_id,
            member_type,
            member_id,
        ))
    }

    /// Removes the memberships the folder `(aws_account_id, folder_id)` holds, answering
    /// how many went: a membership is held inside one folder, so it goes when that folder
    /// does.
    pub fn remove_folder_members_of(&mut self, aws_account_id: &str, folder_id: &str) -> usize {
        let before = self.folder_members.len();
        self.folder_members
            .retain(|(account, member_folder, _, _), _| {
                account != aws_account_id || member_folder != folder_id
            });
        before - self.folder_members.len()
    }
}

/// The key the folder `(aws_account_id, folder_id)` holds the member
/// `(member_type, member_id)` under, so every membership call builds it the same way.
fn folder_member_key(
    aws_account_id: &str,
    folder_id: &str,
    member_type: &str,
    member_id: &str,
) -> (String, String, String, String) {
    (
        aws_account_id.to_string(),
        folder_id.to_string(),
        member_type.to_string(),
        member_id.to_string(),
    )
}

/// A stored folder membership: what `CreateFolderMembership` persisted, which is what
/// every later membership read reports.
#[derive(Debug, Clone)]
pub struct FolderMemberRecord {
    /// The id of the asset this membership names, which together with its type
    /// identifies the membership inside its folder.
    pub member_id: String,
    /// The kind of asset that id names, one member of the model's `MemberType`.
    pub member_type: String,
    /// That asset's own ARN: a folder member IS an asset, so the ARN a membership reports
    /// is the asset's address rather than an address of the membership itself.
    pub arn: String,
}

impl FolderMemberRecord {
    /// This membership as the modelled `FolderMember` structure the membership create
    /// reports.
    pub fn to_folder_member(&self) -> FolderMember {
        FolderMember {
            member_id: Some(self.member_id.clone()),
            member_type: Some(self.member_type.clone()),
        }
    }

    /// This membership as the modelled `MemberIdArnPair` a folder's member listing
    /// reports: the member's id and the ARN of the asset it names.
    pub fn to_member_id_arn_pair(&self) -> MemberIdArnPair {
        MemberIdArnPair {
            member_id: Some(self.member_id.clone()),
            member_arn: Some(self.arn.clone()),
        }
    }
}

/// The `MemberType` member naming an analysis.
/// Source: smithy_query(query="shape", shape="MemberType").
pub const MEMBER_TYPE_ANALYSIS: &str = "ANALYSIS";

/// The `MemberType` member naming a dashboard.
pub const MEMBER_TYPE_DASHBOARD: &str = "DASHBOARD";

/// The `MemberType` member naming a data set.
pub const MEMBER_TYPE_DATASET: &str = "DATASET";

/// The `MemberType` member naming a data source.
pub const MEMBER_TYPE_DATASOURCE: &str = "DATASOURCE";

/// The `MemberType` member naming a Q topic.
pub const MEMBER_TYPE_TOPIC: &str = "TOPIC";

/// The ARN of the asset a folder membership names, or `None` when `member_type` is no
/// member of the model's `MemberType`.
///
/// Each kind of asset has an ARN of its own, so the one a membership reports is built by
/// the builder for that kind rather than from the member type's own spelling: the address
/// a client reads off a folder's member listing is the same address the asset reports
/// everywhere else it appears.
pub fn folder_member_arn(
    region: &str,
    aws_account_id: &str,
    member_type: &str,
    member_id: &str,
) -> Option<String> {
    match member_type {
        MEMBER_TYPE_ANALYSIS => Some(analysis_arn(region, aws_account_id, member_id)),
        MEMBER_TYPE_DASHBOARD => Some(dashboard_arn(region, aws_account_id, member_id)),
        MEMBER_TYPE_DATASET => Some(data_set_arn(region, aws_account_id, member_id)),
        MEMBER_TYPE_DATASOURCE => Some(data_source_arn(region, aws_account_id, member_id)),
        MEMBER_TYPE_TOPIC => Some(topic_arn(region, aws_account_id, member_id)),
        _ => None,
    }
}

/// How many members one folder may hold before `CreateFolderMembership` refuses with the
/// modelled `LimitExceededException`. Same standing as [`MAX_FOLDERS_PER_ACCOUNT`]: a
/// fidelity limit this emulator chooses, not the process bound.
pub const MAX_FOLDER_MEMBERS_PER_FOLDER: usize = 1_000;

/// The TOTAL number of folder memberships this emulator holds, across every account and
/// folder — the process bound.
///
/// Every part of a membership's key comes from the request — all four are `@httpLabel`s
/// the caller chooses — so naming another folder or another account resets one folder's
/// allowance, and the count of folders times the count of members is what a process bound
/// has to cap.
pub const MAX_FOLDER_MEMBERS: usize = 100_000;

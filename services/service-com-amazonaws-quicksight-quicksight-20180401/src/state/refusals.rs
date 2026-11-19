// SPDX-License-Identifier: CC0-1.0

//! The message text QuickSight's conflict and not-found refusals carry.
//!
//! QuickSight identifies the resource a refusal is about the way the service itself
//! does, and every handler that refuses the same error class renders the same
//! template, so the wording a client reads does not depend on which operation it
//! called. These templates cover the resources this crate holds:
//!
//! * a resource addressed by an ARN (a folder, a data source, a data set, a theme)
//!   is named by that ARN — `"<Kind> <arn> already exists"` for a conflict and
//!   `"<Kind> <arn> is not found"` for a miss, neither ending in a period;
//! * a namespace, whose create is refused by the status the namespace already holding
//!   the name is in, is named together with that status and the account it belongs to;
//! * a group or a user, which QuickSight's identity path serves, is named by its name
//!   together with the account it belongs to, and a refusal the account's own
//!   configuration causes is named by that account too — each of them closed by the
//!   labelled fields [`identity_scope_fields`] renders. Those fields are a locator
//!   addressing what the call named, so a refusal that has nothing else to say — a
//!   name the service holds no record of — is that locator alone, with no sentence
//!   before it.
//!
//! A refusal about an allowance names the resource whose allowance ran out, and
//! that resource is the parent the counted children hang on, not the child the
//! refused call named.
//!
//! The ARN is built from the serving region and the request's own path labels by
//! the builders beside these in [`crate::state`], so the resource a message names
//! is the resource the request addressed.

use std::fmt::Write as _;

use super::{data_set_arn, data_source_arn, folder_arn, theme_arn};

/// The resource kind a folder refusal names.
const FOLDER: &str = "Folder";

/// The resource kind a theme refusal names, as the service's own `Theme` shape spells
/// it.
const THEME: &str = "Theme";

/// The resource kind a data-source refusal names — one word, as the service's own
/// `DataSource` shape spells it, not the two words its documentation prose uses.
const DATA_SOURCE: &str = "DataSource";

/// The resource kind a data-set refusal names — one word, as the service's own
/// `DataSet` shape spells it, not the two words its documentation prose uses.
const DATA_SET: &str = "DataSet";

/// `"<kind> <arn> already exists"`: the conflict a create hits when the id it names
/// is already taken by a resource of that kind.
pub fn arn_exists_message(kind: &str, arn: &str) -> String {
    format!("{kind} {arn} already exists")
}

/// `"<kind> <arn> is not found"`: the miss any operation keyed on an absent resource
/// of that kind reports.
pub fn arn_not_found_message(kind: &str, arn: &str) -> String {
    format!("{kind} {arn} is not found")
}

/// The conflict `CreateFolder` reports for a `FolderId` the account already holds.
pub fn folder_exists_message(region: &str, aws_account_id: &str, folder_id: &str) -> String {
    arn_exists_message(FOLDER, &folder_arn(region, aws_account_id, folder_id))
}

/// The miss a folder operation reports for a `FolderId` the account holds no folder
/// under.
pub fn folder_not_found_message(region: &str, aws_account_id: &str, folder_id: &str) -> String {
    arn_not_found_message(FOLDER, &folder_arn(region, aws_account_id, folder_id))
}

/// The miss a data-source operation reports for a `DataSourceId` the account holds
/// no data source under — the same text whether the call reads, updates or deletes.
pub fn data_source_not_found_message(
    region: &str,
    aws_account_id: &str,
    data_source_id: &str,
) -> String {
    arn_not_found_message(
        DATA_SOURCE,
        &data_source_arn(region, aws_account_id, data_source_id),
    )
}

/// The miss a theme operation reports for a `ThemeId` the account holds no theme
/// under — the same text whether the call reads the theme, addresses one of its
/// aliases, or reads or writes its permissions.
pub fn theme_not_found_message(region: &str, aws_account_id: &str, theme_id: &str) -> String {
    arn_not_found_message(THEME, &theme_arn(region, aws_account_id, theme_id))
}

/// The miss a data-set operation reports for a `DataSetId` the account holds no data
/// set under — the same text whether the call reads, deletes, reads or writes the
/// data set's permissions, or addresses a refresh schedule through it.
pub fn data_set_not_found_message(region: &str, aws_account_id: &str, data_set_id: &str) -> String {
    arn_not_found_message(DATA_SET, &data_set_arn(region, aws_account_id, data_set_id))
}

/// The refusal a data-set write reports for a physical table naming a data source
/// whose connection test failed: the data source by the ARN the request carried, and
/// the status that data source is in now. No trailing period.
pub fn data_source_status_message(data_source_arn: &str, status: &str) -> String {
    format!("{DATA_SOURCE} {data_source_arn} is in status {status}")
}

/// The per-data-set ceiling `CreateRefreshSchedule` reports once a data set already
/// holds every refresh schedule it may hold.
///
/// The exhausted resource is the data set the schedules refresh, so the sentence
/// carries the allowance and that data set's ARN — QuickSight's refresh path calls a
/// data set it prepares for SPICE a prepared data source — rather than the schedule
/// the refused call named. No trailing period.
pub fn refresh_schedule_limit_message(
    region: &str,
    aws_account_id: &str,
    data_set_id: &str,
    max: usize,
) -> String {
    format!(
        "Reached max limit of {max} refresh schedules for preparedDataSource {}",
        data_set_arn(region, aws_account_id, data_set_id)
    )
}

/// The miss a refresh-schedule operation reports for a `ScheduleId` the data set holds
/// no schedule under — the same text whether the call reads the schedule or deletes it.
///
/// A refresh schedule is a child addressed through the data set it refreshes and
/// `ExceptionResourceType` has no member naming one, so the refusal names it by the id the
/// call carried rather than by a resource kind and an ARN.
pub fn refresh_schedule_not_found_message(schedule_id: &str) -> String {
    format!("Refresh schedule with ID {schedule_id} not found.")
}

/// The labelled fields QuickSight's identity path closes a refusal with: the account
/// the call addressed, then one field per part of the address naming what inside that
/// account the refusal is about, ending in a period and a space.
///
/// The sentence before them already ends in its own punctuation, so these fields follow
/// it directly — the caller joins the two with no separator of its own — and a refusal
/// with no sentence of its own is these fields alone.
fn identity_scope_fields(aws_account_id: &str, fields: &[(&str, &str)]) -> String {
    let mut rendered = format!("AWS account id: {aws_account_id}");
    for (label, value) in fields {
        // Writing into a `String` cannot fail.
        let _ = write!(rendered, ", {label}: {value}");
    }
    rendered.push_str(". ");
    rendered
}

/// The conflict `CreateGroup` reports for a group name its namespace already holds.
///
/// QuickSight's identity path names the group rather than an ARN, and reports the
/// account it belongs to and the group name as labelled fields after the sentence,
/// which ends in a period and a space.
pub fn group_exists_message(group_name: &str, aws_account_id: &str) -> String {
    format!(
        "Group {group_name} already exists. {}",
        identity_scope_fields(aws_account_id, &[("groupName", group_name)])
    )
}

/// The miss a group operation reports for a `GroupName` the namespace holds no group
/// under — the same text whether the call reads the group or addresses one of its
/// members through it.
///
/// A group is reached by its name inside a namespace of an account, so the refusal names
/// those three: the address the call presented, which it can report whether or not a
/// group was ever held there.
pub fn group_not_found_message(aws_account_id: &str, namespace: &str, group_name: &str) -> String {
    format!("Group {group_name} not found in namespace {namespace} in account {aws_account_id}.")
}

/// The miss a group-membership read reports for a member the group does not hold: the
/// identity path's locator alone, with no sentence before it.
///
/// Nothing is said to have stopped existing, because nothing is known to have existed —
/// the same reading [`user_not_found_message`] makes of a user name: the only thing the
/// service can report is the address the call asked about, which for a membership is the
/// account, the namespace, the group and the member the call named.
pub fn group_membership_not_found_message(
    aws_account_id: &str,
    namespace: &str,
    group_name: &str,
    member_name: &str,
) -> String {
    identity_scope_fields(
        aws_account_id,
        &[
            ("Namespace", namespace),
            ("groupName", group_name),
            ("memberName", member_name),
        ],
    )
}

/// The miss a role-membership delete reports for a group the role does not hold: the
/// identity path's locator alone, with no sentence before it.
///
/// A role membership is held under the role it belongs to with the group's name under it,
/// and nothing is known to have existed at that address, so — as for a group's members
/// ([`group_membership_not_found_message`]) — the only thing the service can report is the
/// address the call asked about: the account, the namespace, the role and the member it
/// named.
pub fn role_membership_not_found_message(
    aws_account_id: &str,
    namespace: &str,
    role: &str,
    member_name: &str,
) -> String {
    identity_scope_fields(
        aws_account_id,
        &[
            ("Namespace", namespace),
            ("Role", role),
            ("memberName", member_name),
        ],
    )
}

/// The refusal a role-membership call on an account that holds no QuickSight identity
/// store of its own reports.
///
/// A role is held by a group the account manages itself, so what forbids the call is not
/// the caller's permissions but the identity store the ACCOUNT has — the one its
/// authentication method gave it — and that is what the sentence states: the operation is
/// not supported for an account whose identity store is that one. The store is read from
/// the account's own subscription rather than fixed here, so the sentence names the store
/// the refused account actually has, and the identity path's labelled fields then report
/// the account, the namespace the call addressed and the role inside it.
pub fn role_membership_not_authorized_message(
    identity_store: &str,
    aws_account_id: &str,
    namespace: &str,
    role: &str,
) -> String {
    format!(
        "Operation is not supported for {identity_store} identity store.{}",
        identity_scope_fields(aws_account_id, &[("Namespace", namespace), ("Role", role)])
    )
}

/// The conflict `CreateNamespace` reports for a `Namespace` the account already holds.
///
/// What forbids the create is the status the namespace holding that name is in, so the
/// sentence states that status — whichever `NamespaceStatus` the existing namespace
/// reports as of this call, not a constant — and the clause after it names the create
/// that failed: the requested name and the account it was requested in, both as the
/// request carried them. The message ends in a period and a space.
pub fn namespace_exists_message(status: &str, namespace: &str, aws_account_id: &str) -> String {
    format!(
        "Cannot create a namespace when status is {status}. Failed to create namespace \
         {namespace} for account {aws_account_id}. "
    )
}

/// The refusal a registration of an identity type the addressed account's
/// authentication method holds no identity store for reports.
///
/// What forbids the registration is a fact about the ACCOUNT — the authentication
/// method its QuickSight subscription chose — so the sentence names that account and the
/// identity type it may not create users of, spelled the way QuickSight writes that
/// type's name in prose rather than as the enum member
/// (`IdentityType`, [`super::IDENTITY_TYPE_QUICKSIGHT_NAME`]). The identity path's
/// labelled fields then report the account again and the namespace the registration
/// addressed.
pub fn identity_type_not_authorized_message(
    aws_account_id: &str,
    namespace: &str,
    identity_type_name: &str,
) -> String {
    format!(
        "Account {aws_account_id} is not authorized to create {identity_type_name} \
         IdentityType user.{}",
        identity_scope_fields(aws_account_id, &[("Namespace", namespace)])
    )
}

/// The conflict `RegisterUser` reports for a user name its namespace already holds.
///
/// A user is a resource of the same identity path a group is, so it is named the same
/// way: the user name in the sentence, then the account it belongs to and that name as
/// labelled fields, ending in a period and a space.
pub fn user_exists_message(user_name: &str, aws_account_id: &str) -> String {
    format!(
        "User {user_name} already exists. {}",
        identity_scope_fields(aws_account_id, &[("userName", user_name)])
    )
}

/// The labelled fields a refusal about a user closes with: the account, the namespace
/// the user lives in and the name it is known by there.
///
/// Those three are exactly the path labels a user operation addresses its user with, so
/// the fields locate the user the request named and nothing in them comes from stored
/// state — a miss can report the address even when it holds no record of it.
fn user_scope_fields(aws_account_id: &str, namespace: &str, user_name: &str) -> String {
    identity_scope_fields(
        aws_account_id,
        &[("Namespace", namespace), ("Username", user_name)],
    )
}

/// The miss a user operation reports for a `UserName` the namespace holds no record of
/// at all: the locator alone, with no sentence before it.
///
/// Nothing is said to have stopped existing, because nothing is known to have existed:
/// the only thing the service can report is the address the call asked about.
pub fn user_not_found_message(aws_account_id: &str, namespace: &str, user_name: &str) -> String {
    user_scope_fields(aws_account_id, namespace, user_name)
}

/// The miss a user operation reports for a `UserName` whose user this service deleted:
/// the existence sentence about that user, then the locator directly after it.
///
/// A deletion is something the service knows happened, so the refusal states it — which
/// is what tells a caller a user it once held is gone apart from a name that never named
/// a user ([`user_not_found_message`]).
pub fn deleted_user_not_found_message(
    aws_account_id: &str,
    namespace: &str,
    user_name: &str,
) -> String {
    format!(
        "User {user_name} does not exist.{}",
        user_scope_fields(aws_account_id, namespace, user_name)
    )
}

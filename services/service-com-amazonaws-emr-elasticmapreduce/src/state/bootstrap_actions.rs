// SPDX-License-Identifier: CC0-1.0

//! The bootstrap actions a cluster was LAUNCHED with — what `ListBootstrapActions`
//! reports.
//!
//! One concern of this service's state: `RunJobFlow` persists the
//! `BootstrapActionConfig` list its launch carried onto the cluster record
//! ([`super::ClusterRecord::bootstrap_actions`]), and the listing PROJECTS those stored
//! configurations onto the wire `Command` ([`bootstrap_action_command`]) rather than
//! building anything from the reading request. Beside the projection sits the page token
//! the listing pages on, which is this module's because nothing else mints one.

use crate::ops::types::Command;

use super::*;

/// `ListBootstrapActions` page size.
///
/// CHOSEN, not documented: the operation paginates on `Marker` ("the pagination token
/// that indicates the next set of results to retrieve",
/// `ListBootstrapActionsInput$Marker` doc) but AWS publishes no page size for it. It is
/// deliberately below the 16 bootstrap actions a cluster may be launched with (Amazon EMR
/// Management Guide, "Create bootstrap actions to install additional software"), so a
/// cluster can carry more actions than one page reports and the pagination contract — a
/// `Marker` that resumes the listing where the page stopped — is reachable rather than
/// dead, the same reason [`super::LIST_INSTANCE_GROUPS_PAGE_SIZE`] sits below its own
/// ceiling.
pub const LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE: usize = 10;

/// The marker every `ListBootstrapActions` page token carries, before the cluster it is
/// bound to and the position it resumes at. It is what makes a token VERIFIABLE: a string
/// that merely happens to be base64 is not a resume point, and one minted for another
/// cluster's listing is not one either — the same reading
/// [`super::encode_release_label_token`] makes of its own token.
const BOOTSTRAP_ACTION_MARKER_PREFIX: &str = "emr/v1/bootstrapActions/";

/// The opaque token that resumes `cluster_id`'s bootstrap-action listing at `cursor`, the
/// position of the first action the page just answered did not report.
///
/// A bootstrap action carries no id of its own — `Command` is "an entity describing an
/// executable that runs on a cluster" (`Command` doc) and names no identifier — so the
/// resume point is the POSITION in the cluster's own launch-ordered list, which is the
/// order every read of it reports. The cluster is bound into the token, with its length
/// ahead of it so no two cluster ids render the same string.
pub fn encode_bootstrap_action_marker(cluster_id: &str, cursor: usize) -> String {
    ::aws_smithy_types::base64::encode(format!(
        "{BOOTSTRAP_ACTION_MARKER_PREFIX}{}:{cluster_id}/{cursor}",
        cluster_id.len()
    ))
}

/// The position a `marker` resumes `cluster_id`'s listing at, or `None` when this listing
/// minted no such token for this cluster.
pub fn decode_bootstrap_action_marker(marker: &str, cluster_id: &str) -> Option<usize> {
    let bytes = ::aws_smithy_types::base64::decode(marker).ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    decoded
        .strip_prefix(BOOTSTRAP_ACTION_MARKER_PREFIX)
        .and_then(|rest| rest.strip_prefix(&format!("{}:{cluster_id}/", cluster_id.len())))
        .and_then(|cursor| cursor.parse().ok())
}

/// The wire `Command` for one stored bootstrap action.
///
/// "An entity describing an executable that runs on a cluster" (`Command` doc): a
/// bootstrap action IS such an executable, so the stored
/// `BootstrapActionConfig` — "the name of the bootstrap action", "the script run by the
/// bootstrap action" with "the location of the script to run" and "a list of command line
/// arguments to pass to the bootstrap action script" — is reported member for member, as
/// the launch submitted it.
///
/// `Args` is "arguments for Amazon EMR to pass to the command for execution"
/// (`Command$Args` doc) and is ALWAYS reported: an action the launch submitted no argument
/// for carries the EMPTY list rather than leaving the member out, so every element of one
/// response has the same members and a caller reading `Args` positionally never meets an
/// absent collection where another element carries one. A launch that named no `Args` and
/// one that named an empty list are therefore indistinguishable in the report, which is
/// what "no arguments" means for an executable.
pub fn bootstrap_action_command(config: &BootstrapActionConfig) -> Command {
    Command {
        name: Some(config.name.clone()),
        script_path: Some(config.script_bootstrap_action.path.clone()),
        args: Some(
            config
                .script_bootstrap_action
                .args
                .clone()
                .unwrap_or_default(),
        ),
    }
}

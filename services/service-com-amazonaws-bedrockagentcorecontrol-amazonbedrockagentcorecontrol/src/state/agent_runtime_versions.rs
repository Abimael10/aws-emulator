// SPDX-License-Identifier: CC0-1.0

//! The versions ONE agent runtime holds, as a listing of them reports each one.
//!
//! `ListAgentRuntimeVersions` reports `AgentRuntimes` — the same `AgentRuntime`
//! structure `ListAgentRuntimes` reports, one entry per published version rather
//! than one per runtime — so the projection lives here, beside the record, and
//! both listings and every read of the runtime render a version through it. That
//! is what keeps the version, the description, the instant and the status one
//! runtime reports the same whichever call asked.

use super::*;

/// One agent-runtime version's key in the version-ordered map a listing pages
/// over: the version number zero-padded to the five digits `AgentRuntimeVersion`
/// allows (pattern `^([1-9][0-9]{0,4})$`), so ascending KEY order is ascending
/// VERSION order for every version the wire can carry — which is what makes the
/// shared page walk's "resume strictly after the key the last page ended on"
/// cursor sound for a listing of versions.
pub fn agent_runtime_version_key(version: u32) -> String {
    format!("{version:05}")
}

impl AgentRuntimeRecord {
    /// This runtime AT one of its published versions, as the `AgentRuntime`
    /// structure every listing of this family reports.
    ///
    /// The identity members are the runtime's own and the version-scoped ones are
    /// that version's, status included — which is the clock's, so it is resolved
    /// at `now` rather than frozen into the record, and reports `DELETING` for
    /// every version of a runtime on its way out.
    pub fn version_entry(
        &self,
        version: &AgentRuntimeVersionRecord,
        now: SystemTime,
    ) -> AgentRuntime {
        AgentRuntime {
            agent_runtime_arn: self.agent_runtime_arn.clone(),
            agent_runtime_id: self.agent_runtime_id.clone(),
            agent_runtime_version: version.version_string(),
            agent_runtime_name: self.agent_runtime_name.clone(),
            // `@required` on the structure, so a version published without a
            // description reports the empty string rather than omitting it.
            description: version.description.clone().unwrap_or_default(),
            last_updated_at: timestamp(version.last_updated_at),
            status: self.status_of(version, now).to_string(),
        }
    }

    /// Every version this runtime holds, keyed by [`agent_runtime_version_key`]
    /// so the map's own order IS ascending version order — which is what lets the
    /// shared page walk resume strictly after the key a page ended on.
    ///
    /// The versions are the ones this record PERSISTS: a create publishes the
    /// first and each update another, and a version that was deleted is not among
    /// them — a listing never reports a range of numbers nothing was stored for.
    /// The records are borrowed rather than projected here, so a page renders
    /// [`Self::version_entry`] only for the versions it actually reports.
    pub fn versions_by_page_key(&self) -> BTreeMap<String, &AgentRuntimeVersionRecord> {
        self.versions
            .values()
            .map(|version| (agent_runtime_version_key(version.version), version))
            .collect()
    }
}

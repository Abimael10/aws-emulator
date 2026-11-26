Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 7d5f5571d94544e1e0d6279d2f57c51740cc17a4

# CreateComputeNodeGroup persists slurmConfiguration.gresCustomSettings unbounded, the one stored list UpdateComputeNodeGroup does bound

_Filed: 2026-11-26T12:13:41Z_
_Operation: CreateComputeNodeGroup | Class: validation_

> _Note: this issue was identified against the code base at commit `7d5f5571d94544e1e0d6279d2f57c51740cc17a4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Every create in this crate puts the caller-sized lists it would PERSIST through `crate::state::stored_list_violation` before anything is stored, and the state module states that as an invariant of the whole store. `CreateComputeNodeGroup` checks three lists — `subnetIds`, `instanceConfigs` and `slurmConfiguration.slurmCustomSettings` — but NOT `slurmConfiguration.gresCustomSettings`, even though the create request shape carries that member and the handler persists it verbatim (`slurm_configuration: input.slurm_configuration.map(Into::into)`, create_compute_node_group.rs:511, via `From<ComputeNodeGroupSlurmConfigurationRequest>` in state.rs:2103-2111 which copies `gres_custom_settings` straight through).

Its own sibling, `UpdateComputeNodeGroup`, DOES hold that exact member to the ceiling — the member name string `"slurmConfiguration.gresCustomSettings"` appears in the update handler's `stored_lists` array and nowhere in the create handler's. So one `CreateComputeNodeGroup` can store a `gresCustomSettings` list of any length (each entry itself an unbounded map), while the very next `UpdateComputeNodeGroup` on the same record refuses a list one entry past `MAX_STORED_LIST_ENTRIES`. The create path is the one the record is born on, so the ceiling the update path defends is reachable around it.

## Evidence

MODEL — the member exists on the create request and the model bounds it with nothing:
`smithy_query(query="shape", shape="ComputeNodeGroupSlurmConfigurationRequest")` ->
  "gresCustomSettings": {"target": "GresCustomSettings", "type": "list", "doc": "<p>The additional Slurm <code>gres.conf</code> records for the compute node group..."}
`smithy_query(query="shape", shape="GresCustomSettings")` ->
  {"type": "list", "member": {"target": "GresCustomSettingMap", "type": "map"}}   (no `length` trait)

CRATE SIDE A — the create handler, which omits it (services/service-com-amazonaws-pcs-awsparallelcomputingservice/src/ops/create_compute_node_group.rs:240-255):
```
// The caller-sized lists this create would PERSIST. The model bounds neither
// `StringList` nor `InstanceList`, so one request could otherwise size one
// stored record arbitrarily. ...
let slurm = input.slurm_configuration.as_ref();
let stored_lists: [(&str, usize); 3] = [
    ("subnetIds", input.subnet_ids.len()),
    ("instanceConfigs", input.instance_configs.len()),
    (
        "slurmConfiguration.slurmCustomSettings",
        slurm.map_or(0, |slurm| {
            slurm.slurm_custom_settings.as_ref().map_or(0, Vec::len)
        }),
    ),
];
```

CRATE SIDE B — the sibling update handler, which includes it (src/ops/update_compute_node_group.rs:221-239):
```
// The caller-sized lists this update would PERSIST, bounded for the same reason the
// create's are: the model bounds none of them, so one request could otherwise size
// one stored record arbitrarily.
let stored_lists: [(&str, usize); 3] = [
    ("subnetIds", input.subnet_ids.as_ref().map_or(0, Vec::len)),
    (
        "slurmConfiguration.slurmCustomSettings", ...
    ),
    (
        "slurmConfiguration.gresCustomSettings",
        slurm.map_or(0, |slurm| {
            slurm.gres_custom_settings.as_ref().map_or(0, Vec::len)
        }),
    ),
];
```

CRATE SIDE C — the invariant the create breaks (src/state.rs:27-36):
```
//! # Every store here is bounded
//!
//! Nothing a caller can repeat grows this state without a ceiling:
//! ...
//! * record SIZE — the caller-sized lists a create persists are capped at
//!   [`MAX_STORED_LIST_ENTRIES`] ([`stored_list_violation`]);
```

The member IS present on the create input type: src/ops/types/create_compute_node_group_types.rs:38 `pub gres_custom_settings: Option<Vec<std::collections::HashMap<String, String>>>,`

TEST COVERAGE — the update ceiling is pinned, the create ceiling for this member is not: tests/all/cluster_update_and_endpoints/compute_node_group_update.rs:620-624 pins only `"slurmConfiguration.slurmCustomSettings"` past the ceiling on the UPDATE path; `rg gres tests/` finds `gresCustomSettings` only at compute_node_group_update.rs:241-245 (a round-trip case, not a ceiling case), and no test exercises a create-path list ceiling for a compute node group at all.

## Steps to Reproduce

1. Create a cluster and let it settle to ACTIVE.
2. Call CreateComputeNodeGroup with a valid request plus `slurmConfiguration.gresCustomSettings` holding MAX_STORED_LIST_ENTRIES + 1 entries (e.g. 201 maps of {"Name":"gpu"}).
3. Observe HTTP 200: the group is created and the whole oversized list is persisted on the record.
4. Call GetComputeNodeGroup and confirm `computeNodeGroup.slurmConfiguration.gresCustomSettings` carries all 201 entries.
5. Now call UpdateComputeNodeGroup on that same group with the identical 201-entry `slurmConfiguration.gresCustomSettings`.
6. Observe the client-error status with ValidationException naming `slurmConfiguration.gresCustomSettings` — the same list the create accepted is refused by the sibling handler.

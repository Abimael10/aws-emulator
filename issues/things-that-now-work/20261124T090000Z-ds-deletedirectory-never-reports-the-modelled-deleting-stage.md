Issue-Severity: MEDIUM
Issue-Gap-Class: lifecycle

# DeleteDirectory takes the directory away inside the call, so no caller ever observes the modelled `Deleting` stage or the terminal `Deleted` one

_Filed: 2026-11-24T09:00:00Z_
_Operation: DeleteDirectory | Class: lifecycle_

## Resolution

Served: the owning crate now takes the directory out of service on the call and
lets the clock walk the rest of the modelled chain — `Deleting`, then the
terminal `Deleted`, then the record leaving the account
(`services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/state/directory_deletion.rs`,
`retire_directory` / `advance_directory_teardowns`), pinned by that crate's
`tests/all/directory_deletion.rs` and by the cross-service walk in
`core/router/tests/all/ds_trust_and_region_control_plane_walks.rs`. The report
below is the divergence as it stood when it was filed.

## Description

`DeleteDirectory` releases everything the directory was given and drops the
directory's own record within the call
(services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/ops/delete_directory.rs,
module doc and `crate::state::tear_down_directory`), so the FIRST
`DescribeDirectories` after the delete already answers an empty page for the id.

AWS runs that teardown on the clock: the modelled `DirectoryStage` enum carries
`Deleting` and `Deleted` beside the serving `Active`
(`smithy_query(query="shape", shape="DirectoryStage")` lists
`Active, Created, Creating, Deleted, Deleting, Failed, Impaired, Inoperable,
Requested, RestoreFailed, Restoring, Updating`), and a client that reads the
directory straight after the delete sees `Deleting`, then the chain's terminal
stage, before the record leaves the account's inventory. With the in-call
teardown neither value is reachable from any request, so a waiter written against
the stage chain — the same shape of waiter this emulator already serves for a
trust's `Deleting`, and for a share's `Deleting` — cannot be exercised, and a
client cannot tell "the delete is still running" from "this account never held
the id".

## Evidence

The stages the model declares, none of which this service's directories ever
report: services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/ops/types/shared_types.rs
```
pub const DIRECTORY_STAGE_VALUES: &[&str] = &["Active", "Created", "Creating", "Deleted", "Deleting", "Failed", "Impaired", "Inoperable", "Requested", "RestoreFailed", "Restoring", "Updating"];
```

The same crate already drives a teardown on the clock for the resources that
hang off a directory, so the mechanism exists: `DeleteTrust` records the modelled
transient `Deleting` and lets the clock finish the job
(src/ops/delete_trust.rs, around the `Deleting` comment), and `UnshareDirectory`
does the same for a share (src/ops/unshare_directory.rs). Only the directory
itself skips the chain.

What pins the current behaviour, and what must move with the fix: the crate's own
teardown suite
(tests/all/directory_deletion.rs::a_delete_takes_the_directory_away_in_the_call)
asserts the first read after the delete narrows the page to nothing, and the
suites that delete a directory and then immediately expect the not-found refusal
(tests/all/directory_network_release.rs, tests/all/directory_security_group.rs,
tests/all/directory_server_interfaces.rs,
tests/all/refusal_classification_rules.rs, tests/all/alias_reassignment.rs) do
the same. A fix that stages the teardown has to move those expectations onto the
chain — the record stays, in `Deleting`, until the clock makes its removal due —
rather than leave two suites of the same crate disagreeing.

## Steps to Reproduce

1. Boot the emulator and create a directory (`CreateDirectory`), waiting for it
   to report `Active`.
2. Call `DeleteDirectory` for it.
3. Call `DescribeDirectories` filtered by that id straight after.
4. Observe an empty `DirectoryDescriptions` page, instead of one description
   reporting `Stage: "Deleting"` and then the chain's terminal stage before the
   record leaves the account.

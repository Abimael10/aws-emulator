# Local additions to the Smithy model snapshot

`com-amazonaws-redshift-redshiftserviceversion20121201.json` in this directory is
a snapshot of the published Amazon Redshift model and is kept EXACTLY as shipped:
it is a third-party artefact consumed at build time by
`#[smithy_macros::smithy]`, so an edit there would be invisible to a reader of the
crate's source and lost on the next refresh.

AWS answers some operations with members that snapshot does not declare. Each is
declared in this crate's own source instead, from the small overlay model
`local-additions.json` beside the snapshot, so the declaration is generated the
same way every modelled shape is — one member list, one set of traits, no
hand-written schema or serializer. The same mechanism is used by
`services/service-com-amazonaws-wisdom-wisdomservice`
(`smithy-model/local-additions.json` +
`src/ops/types/quick_response_data_types.rs`) and by
`services/service-com-amazonaws-workspaces-workspacesservice`.

## `Cluster$AvailabilityZoneRelocation`, `Cluster$AzDisasterRecoveryStatus` and `Cluster$IsMaintenanceWindowActivated`

- **What AWS does**: every answer that carries a `Cluster` — `CreateCluster`,
  every `Modify*`, `RestoreFromClusterSnapshot`, `DescribeClusters` — reports
  these three beside the members the snapshot declares. Recorded in the
  `simparity-wire-aws-v1` wire fixtures `redshift-aqua-configuration-modify-transition`
  (step `task#3`) and `redshift-create-cluster-lifecycle` (steps `task#1`,
  `task#2`), where a cluster created with Availability Zone relocation enabled,
  and with no maintenance deferral on it, reports `enabled`, `enabled` and
  `true`.
  - `AvailabilityZoneRelocation` is the relocation OPTION's enablement, which the
    snapshot's `AvailabilityZoneRelocationStatus` reports as well: one value, two
    elements.
  - `AzDisasterRecoveryStatus` is the enablement of the cluster's
    Availability-Zone-level recovery. Relocating a provisioned cluster into
    another zone is that recovery — "the option to enable relocation for an
    Amazon Redshift cluster between Availability Zones after the cluster is
    created" (`smithy_query(query="doc", shape="CreateClusterMessage",
    member="AvailabilityZoneRelocation")`) — so it carries the same enablement.
  - `IsMaintenanceWindowActivated` is whether the cluster's preferred maintenance
    window is in force. A cluster has one from the moment it is created ("if you
    do not specify a maintenance window, Amazon Redshift assigns a 30-minute
    maintenance window"), and the one thing that suspends it is a deferral, which
    `ModifyClusterMaintenance` sets and clears.
- **What the snapshot declares**: `com.amazonaws.redshift#Cluster` with
  sixty-four members, none of them these three. It declares nothing of any of
  those names anywhere.
- **Where they are declared**: `src/ops/types/cluster_types.rs`, whose `Cluster`
  is the single type every cluster write and read reports. It is generated from
  `local-additions.json`, which restates the sixty-four modelled members — with
  their own targets, so every scalar's type and every list's item element name are
  captured exactly as the snapshot spells them — and adds the three.
- **What else is in the overlay**: only the shapes those members target, and only
  as much of each as codegen reads. The structures (`Endpoint`,
  `PendingModifiedValues`, `AquaConfiguration` and the rest) are resolved through
  the Rust field's type rather than through the model, so the overlay declares the
  shape without restating its members; the modelled types in `shared_types.rs` are
  what those members read and write. The lists restate their item target and its
  `@xmlName`, which is what the awsQuery writer reads off them.
- **Where the values come from**: `crate::state::populate_derived_members`, the
  one rule every path that writes a cluster record runs, so the create, modify,
  restore and read paths cannot report different values for one cluster.

`tests/all/cluster_relocation_and_maintenance_members.rs` pins the three members
on the wire, over a table of relocation options and of deferral forms, and its
last test pins this overlay against the snapshot: every restated shape is compared
declaration for declaration, and `Cluster` must differ from the snapshot's by
exactly those three members. If the snapshot is ever refreshed with them, with a
member the overlay does not restate, or with a changed target or list item name,
that test fails — and the local declaration should then give way to the modelled
shape.

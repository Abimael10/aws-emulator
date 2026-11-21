# Local additions to the Smithy model snapshot

`com-amazonaws-arczonalshift-percdataplane.json` in this directory is a snapshot
of the published ARC zonal shift model and is kept EXACTLY as shipped: it is a
third-party artefact consumed at build time by `#[smithy_macros::smithy]`, so an
edit there would be invisible to a reader of the crate's source and lost on the
next refresh.

AWS answers some operations with members that snapshot does not declare, and
names one of them differently from the way the snapshot declares it. Each is
declared in this crate's own source instead, from a small overlay model beside
the snapshot (`local-additions.json`, `access-denial.json`), so the declaration
is generated the same way every modelled shape is — one member list, one set of
constraint traits, no hand-written schema or serializer. The same mechanism is
used by `services/service-com-amazonaws-workspaces-workspacesservice`
(`smithy-model/local-additions.json` +
`src/ops/types/workspace_properties_types.rs`).

## `azGroupId` on every zonal shift ARC reports

- **What AWS does**: every zonal-shift object a response carries names the
  Availability Zone group of the Region the shifted resource lives in, in the
  member `azGroupId` — recorded in the `simparity-wire-aws-v1` wire fixture
  `arc-zonal-shift-nlb-zone-weight-shift-and-cancel`, whose `StartZonalShift`
  (201), `GetManagedResource` (200), `ListZonalShifts` (200) and
  `CancelZonalShift` (200) bodies each carry it for a load balancer in one
  Region, with the same value on all four.
- **What the snapshot declares**: `ZonalShift`, `ZonalShiftSummary`,
  `ZonalShiftInResource`, `StartPracticeRunResponse` and
  `CancelPracticeRunResponse` with the members `zonalShiftId`,
  `resourceIdentifier`, `awayFrom`, `expiryTime`, `startTime`, `comment` (and
  `status`, `appliedStatus`, `shiftType`, `practiceRunOutcome` variously), none
  of them `azGroupId`, and no shape of that name at all.
- **Where it is declared**: `src/ops/types/zonal_shift_types.rs`, which holds
  every type this crate reports a zonal shift through — the `ZonalShift` output
  of `StartZonalShift`, `CancelZonalShift` and `UpdateZonalShift`, the
  `ZonalShiftSummary` of `ListZonalShifts`, the `ZonalShiftInResource` of
  `GetManagedResource` and `ListManagedResources`, and the
  `StartPracticeRunResponse` and `CancelPracticeRunResponse` of the practice run
  operations. It is generated from
  `local-additions.json`, which restates the members the snapshot declares —
  with their own targets, so every length, pattern and enum constraint is
  captured exactly as the snapshot spells it — and adds `azGroupId`. An open
  `string`, because nothing publishes a value set for it; the value itself is
  derived from the resource's Region (`crate::state::az_group_id`).
- **What else is in the overlay**: only the shapes those members target, and
  only as much of each as codegen reads — the enums carry their values, the
  strings their length and pattern, the timestamps their type.

`tests/all/zonal_shift_az_group.rs` pins both halves: the member on the wire,
over a set of resources in different Regions and through every operation that
reports a shift; and the overlay against the snapshot, shape by shape (type,
members, enum values and constraint traits, documentation aside). If the
snapshot is ever refreshed with `azGroupId`, with a member the overlay does not
restate, or with a changed target, value set or constraint, that suite fails —
and the local declaration should then give way to the modelled shape.

## `Message` on the access denial

- **What AWS does**: ARC decides the caller's authorization on a call's target in
  the layer in front of the operation and answers the denial from there, in that
  layer's envelope rather than the operation's: the recorded 403s name the class
  in `x-amzn-errortype` as a shape of that framework rather than as a shape of
  this service's model, and the document they carry holds the denial text under
  the member `Message` — recorded in the `simparity-wire-aws-v1` wire fixtures
  `arc-zonal-shift-zonal-shift-error-paths` (`StartZonalShift`) and
  `arc-zonal-shift-practice-run-configuration-validation-errors`
  (`CreatePracticeRunConfiguration`), with no `__type` discriminator, and with an
  empty document on the denials that state no reason.
- **What the snapshot declares**: `AccessDeniedException` as a client error with
  `@httpError` 403 and the single optional member `message`, with no `@jsonName`.
- **Where it is declared**: `src/ops/types/access_denied_types.rs`, generated from
  `access-denial.json`, which restates the shape exactly as the snapshot declares
  it and adds the `@jsonName` the recorded wire carries. Leaving the discriminator
  out is the other half, and belongs to the error serialization rather than to a
  member list: `src/service/denial_envelope.rs`.
- **Why it is its own overlay file**: `local-additions.json` is restated against
  the snapshot shape by shape by `tests/all/zonal_shift_az_group.rs`, which is
  what keeps the `azGroupId` overlay honest; this shape differs from the snapshot
  by a trait ON a member it restates, so it is declared beside it rather than
  inside it.

`tests/all/access_denial_envelope.rs` pins the member and the member SET on the
wire, over every operation of this service that can be driven to a denial, and
pins the boundary: an error the operation itself answers keeps the protocol's own
envelope. If the snapshot is ever refreshed with a `@jsonName` on that member,
the local declaration should give way to the modelled shape.

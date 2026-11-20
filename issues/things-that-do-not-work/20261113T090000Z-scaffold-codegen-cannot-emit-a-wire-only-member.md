Issue-Severity: MEDIUM
Issue-Gap-Class: missing

# The shape derive cannot emit a member the wire carries that the model does not declare, so a service hand-writes the whole shape

_Filed: 2026-11-13T09:00:00Z_
_Operation: scaffold-codegen-wire-only-members | Class: missing_

## Description

`#[smithy_macros::smithy(shape = "…")]` builds a shape's `aws_smithy_schema::Schema`,
`SerializableStruct` and `DeserializableStruct` from the members the Smithy shape
declares, and refuses any field that corresponds to none:

- `vendor/smithy-macros/src/lib.rs::validate_struct_against_shape` ends with
  "Field '<name>' does not correspond to any Smithy member. Remove it or add
  "<Name>" to the unsupported list.";
- `unsupported = [...]` only DROPS a declared member, so there is no inverse of it —
  no way to ADD one.

Several AWS endpoints put members on a 200 response that their published model does
not declare. A service crate that must answer with such a member therefore cannot use
the derive at all for that shape: it has to hand-write the member schemas, the struct
schema, `SerializableStruct` and `DeserializableStruct`, mirroring what the derive
would have emitted for every declared member, just to append one more.

## Evidence

- `services/service-com-amazonaws-route53recoverycontrolconfig-route53recoverycontrolconfig/src/ops/types/shared_types.rs`
  hand-writes four shapes — `ControlPanel`, `RoutingControl`, `AssertionRule` and
  `GatingRule` — for one extra member each. The Recovery Control Configuration
  endpoint reports a control panel's and a safety rule's ARN under a generic `Arn`
  member beside the typed `ControlPanelArn`/`SafetyRuleArn` one, and reports a routing
  control's `Route53HealthCheckSequenceNumber`;
  `smithy_query(query="shape",shape="ControlPanel")` (and the three others) declares
  none of them.
- The same shape of workaround already exists for a different derive gap in
  `services/service-com-amazonaws-chimesdkmediapipelines-chimesdkmediapipelinesservice/src/ops/types/shared_types.rs`
  (`MediaCapturePipeline`, filed as
  `issues/things-that-do-not-work/20261021T223000Z-scaffold-codegen-drops-timestampformat-from-member-schemas.md`),
  and in
  `services/service-com-amazonaws-medialive-medialive/src/ops/types/shared_types.rs`.
- Every such block pins member order, `member_index` values and shape ids by hand, so
  it can drift from the model; the route53-recovery-control-config block is pinned by
  a unit test that reads the shipped model, which is a test every copy has to repeat.
- Another open issue needs the same escape hatch:
  `issues/things-that-do-not-work/20261111T123309Z-athena-capacity-reservation-read-and-lifecycle-paths-diverge.md`
  records `CapacityReservation.CapacityLockTime` and
  `CapacityReservation.LastCapacityAllocation`, neither of which Athena's model
  declares.

## Steps to Reproduce

1. Take any shape the derive owns and add a field for a member the model does not
   declare — for example `pub arn: Option<String>` on a shape whose model lists only a
   typed ARN member.
2. Build the crate: the derive fails with "Field 'arn' does not correspond to any
   Smithy member", and there is no attribute that admits it.
3. The only way to answer with the member is to delete the attribute and write the
   shape's schemas and codecs out by hand.

## Required change

In `vendor/smithy-macros` (agent-immutable from a service crate, so this needs a
core-library change), give the attribute the inverse of `unsupported`: a list of
members the WIRE carries that the model does not, each with its wire name and kind —
for example
`#[smithy_macros::smithy(shape = "…", wire_only = ["Arn: string", "Route53HealthCheckSequenceNumber: long"])]`
— emitted as member schemas after the declared ones, so the declared members keep the
indices they have today and the Rust fields are `Option<T>` like any other
non-`@required` member. Then delete the hand-written blocks named above and let the
derive produce those shapes again.

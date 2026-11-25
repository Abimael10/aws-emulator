# Local additions to the Smithy model snapshot

`com-amazonaws-applicationsignals-applicationsignals.json` in this directory is a
snapshot of the published CloudWatch Application Signals model and is kept
EXACTLY as shipped: it is a third-party artefact consumed at build time by
`#[smithy_macros::smithy]`, so an edit there would be invisible to a reader of
the crate's source and lost on the next refresh.

AWS answers two of this service's reads with member sets NARROWER than that
snapshot declares: a member the snapshot marks `@required` is absent from the
recorded body. Each such shape is declared in this crate's own source instead,
from a small overlay model beside the snapshot (`local-additions.json`), so the
declaration is generated the same way every modelled shape is — one member list,
one set of constraint traits, no hand-written schema or serializer. The same
mechanism is used by `services/service-com-amazonaws-arczonalshift-percdataplane`
(`smithy-model/local-additions.json` + `src/ops/types/zonal_shift_types.rs`) and
by `services/service-com-amazonaws-workspaces-workspacesservice`.

## `Service.KeyAttributes` and `Service.MetricReferences` on an undiscovered service

- **What AWS does**: Application Signals DISCOVERS a service from the telemetry
  an instrumented application sends; it has no create. `GetService` for key
  attributes nothing reported telemetry for in the requested window is answered
  200 with a `Service` object carrying NEITHER `KeyAttributes` nor
  `MetricReferences` — recorded in the `simparity-wire-aws-v1` wire fixture
  `application-signals-discovery-and-service-topology` (step `task#3`), whose
  200 body has no value for either member. It does not echo the requested key
  attributes back as a discovered service, and it does not serialize an empty
  metric list: "nothing observed" and "observed, and there are no metrics" are
  different answers.
- **What the snapshot declares**: `Service` with `KeyAttributes` (target
  `Attributes`, `@length 1..4`) and `MetricReferences` (target
  `MetricReferences`) both `@required`, beside the optional `AttributeMaps`,
  `ServiceGroups` and `LogGroupReferences`.
- **Where it is declared**: `src/ops/types/discovered_service_types.rs`,
  generated from `local-additions.json`, which restates the shape with those two
  members optional and against the snapshot's own targets, so every length and
  pattern constraint is captured exactly as the snapshot spells it. What the
  members CARRY is `crate::state::…::observed_service`.

## `RecurrenceRule.Expression` on a one-shot exclusion window

- **What AWS does**: every exclusion window a read reports carries its
  `RecurrenceRule` structure, built from the window's stored recurrence — so a
  window added with no cron or rate expression is reported with the member
  PRESENT and no `Expression` in it. Recorded in the `simparity-wire-aws-v1` wire
  fixture `application-signals-slo-exclusion-windows` (steps `task#4` and
  `task#6`), whose listed window carries `RecurrenceRule` beside `Window`,
  `StartTime` and `Reason`.
- **What the snapshot declares**: `RecurrenceRule` with the single member
  `Expression` (target `Expression`, `@length 1..1024`) marked `@required`.
- **Where it is declared**: `src/ops/types/recurrence_rule_types.rs`, generated
  from `local-additions.json`, which restates the shape with that member optional
  against the snapshot's own target, so the expression's length bound is
  unchanged. A rule with no expression is this service's one statement of "this
  window recurs on no schedule", on the way out and on the way in alike
  (`crate::state::recurrence_expression`).

## What else is in the overlay

Only the shapes those members target, and only as much of each as codegen reads:
the maps and lists with their own key, value and element targets and their length
traits, the strings with their length and pattern. `ServiceGroup` and
`MetricReference` are the two element shapes reached ONLY as list elements, where
codegen takes the member list from the element's own Rust type — generated from
the snapshot, in `src/ops/types/shared_types.rs` and
`src/ops/types/list_services_types.rs` — so the overlay declares them as the
structures they are and restates no member of either.

`tests/all/undiscovered_service_members.rs` and
`tests/all/exclusion_window_recurrence_rule.rs` pin the two halves on the wire:
the member set of an undiscovered `GetService` read and the recurrence rule of a
listed exclusion window, each over a set of inputs, beside the discovered and
recurring cases where the same members are present. If the snapshot is ever
refreshed without those `@required` traits, the local declarations should give way
to the modelled shapes.

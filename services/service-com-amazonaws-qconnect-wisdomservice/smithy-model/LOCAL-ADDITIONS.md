# Local additions to the Smithy model snapshot

`com-amazonaws-qconnect-wisdomservice.json` in this directory is a snapshot of the
published Amazon Q in Connect model and is kept EXACTLY as shipped: it is a
third-party artefact consumed at build time by `#[smithy_macros::smithy]`, so an
edit there would be invisible to a reader of the crate's source and lost on the
next refresh.

AWS answers some operations with members that snapshot does not declare. Each is
declared in this crate's own source instead, from the small overlay model
`local-additions.json` beside the snapshot, so the declaration is generated the
same way every modelled shape is — one member list, one set of traits, no
hand-written schema or serializer. The same mechanism is used by
`services/service-com-amazonaws-workspaces-workspacesservice`
(`smithy-model/local-additions.json` +
`src/ops/types/workspace_properties_types.rs`).

## `QuickResponseData$attributeList`

- **What AWS does**: every 200 answer that carries a `QuickResponseData` —
  `CreateQuickResponse`, `GetQuickResponse`, `UpdateQuickResponse` — reports
  `attributeList`, the contact attributes the quick response's content names, and
  reports it as the EMPTY list for a content that names none. Recorded in the
  `simparity-wire-aws-v1` wire fixtures `wisdom-quick-response-error-paths` (step
  `setup#3`) and `wisdom-quick-response-lifecycle` (steps `task#3`, `task#4`,
  `task#6`, `task#7`). `QuickResponseSummary` carries no such member, and the
  recorded `ListQuickResponses` answer carries none either, so the overlay leaves
  the summary shape alone.
- **What the snapshot declares**: `com.amazonaws.qconnect#QuickResponseData` with
  eighteen members, none of them `attributeList`. The keys themselves are
  modelled: `ContactAttributeKeys` (a `@uniqueItems` list of
  `ContactAttributeKey`) is what `QuickResponseSearchResultData` reports a
  search's resolved and unresolved attributes as, so the overlay member targets
  that shape rather than inventing one.
- **Where it is declared**: `src/ops/types/quick_response_data_types.rs`, whose
  `QuickResponseData` is the single type every quick-response write and read
  reports. It is generated from `local-additions.json`, which restates the
  eighteen modelled members — with their own targets, so every length, pattern,
  enum value and the `@timestampFormat` of both timestamps is captured exactly as
  the snapshot spells it — and adds the nineteenth.
- **What else is in the overlay**: only the shapes those nineteen members target,
  and only as much of each as codegen reads. `QuickResponseContents` and
  `GroupingConfiguration` are structures, which codegen resolves through the Rust
  field's type rather than through the model, so the overlay declares the shape
  without restating its members; the modelled types in `shared_types.rs` are what
  those members read and write.

`tests/all/quick_response_optional_members.rs` pins the member on the wire,
alongside the rest of the quick-response answer, and its last test pins this
overlay against the snapshot: every restated shape is compared declaration for
declaration, and `QuickResponseData` must differ from the snapshot's by exactly
`attributeList`. If the snapshot is ever refreshed with `attributeList`, with a
member the overlay does not restate, or with a changed target, value set,
constraint or timestamp format, that test fails — and the local declaration
should then give way to the modelled shape.

Issue-Source: trace-sweep-divergence
Issue-Trace: resiliencehub-app-version-draft-composition
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resiliencehub-app-version-draft-composition resiliencehub-app-version-publish
Issue-Shape-Id: com.amazonaws.resiliencehub#AwsResilienceHub
Issue-Key: resiliencehub|ResolveAppVersionResources|2xx->2xx|$.resolutionId
Issue-Commit: 2c275d8e7c049de119f415d052ef726706c4f0b6

# resiliencehub ResolveAppVersionResources mints a resolutionId whose shape AWS does not use

> _Note: this issue was identified against the code base at commit `2c275d8e7c049de119f415d052ef726706c4f0b6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ResolveAppVersionResources

## Description
One cause, reported by the replay as one cluster covering two fixtures (the draft-composition sequence and the publish sequence), neither owned by an open issue and with no stub.

Cause 1 of 1 — the server-generated `resolutionId` ResolveAppVersionResources answers is a canonical hyphenated version-4 UUID, which is not the shape AWS mints for a resource resolution handle.
AWS: Smithy `ResolveAppVersionResources` output member `resolutionId` targets `String255` (@length 1..255) and is @required, documented 'The identifier for a specific resolution'; the same member is @required on `DescribeAppVersionResourcesResolutionStatus` and `ListAppVersionResources`, so it is the handle a client carries from the resolve call into every later read of that resolution (smithy_query query="operation" operation="ResolveAppVersionResources,DescribeAppVersionResourcesResolutionStatus" on services/service-com-amazonaws-resiliencehub-awsresiliencehub/smithy-model/com-amazonaws-resiliencehub-awsresiliencehub.json; query="doc" shape="ResolveAppVersionResourcesResponse" member="resolutionId"). Recorded behavior only for the FORM of the value: the model constrains only its length, and the corpus does not assert the recorded id literally (it is a per-run, server-minted handle) but DOES assert its SHAPE, and the emulator's value fails that shape check in both recordings. The requirement is therefore the shape of the handle this operation mints, not any one recorded id. Hypothesis: AWS's resolution handle is an opaque token whose character form is not the 8-4-4-4-12 hyphenated UUID form (the two recordings agree on a shape the hyphenated UUID does not satisfy); a fix is confirmed by the shape assertion passing, never by matching a captured id value.
Emulator: HTTP 200 (status is correct) whose `resolutionId` is a fresh hyphenated v4 UUID — e.g. the replay's value shaped like `xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`. The handler takes it straight from the record minted by `ResourceResolution::started_at`, whose minter is `mint_resolution_id`, implemented as `Uuid::new_v4().hyphenated().to_string()`. Note the same service already uses a SECOND, differently shaped id for the same member elsewhere (a SHA-256 digest from `mapping_resolution_id`, which ListAppVersionResources synthesizes), so neither form was derived from the recording; that synthesizing fallback is owned by open issue 20261103T115316Z-resiliencehub-listappversionresources-reports-the-raw-draft-resource-map.md, which does NOT cover the shape of the id this operation mints.
Why: `resolutionId` is the one handle a client holds for a resolution it started, and clients validate or store it by form — a fixed-width column, a UUID-typed field, a regex on an opaque-token pattern, or a strict schema over a captured response. Against AWS the handle has one shape; against this emulator it has another (and a third on the listing path), so a client that round-trips the id through typed storage or validation fails against the emulator while succeeding against AWS, and a client written against the emulator bakes in a UUID assumption AWS does not honour. Because the value also feeds DescribeAppVersionResourcesResolutionStatus requests, the mis-shaped handle propagates into the resolution-polling loop.
Fix: the handle minter for a resource resolution in this service's state (`mint_resolution_id`, the only writer, called from `ResourceResolution::started_at`), so every reader of a resolution reports the same AWS-shaped handle; the ResolveAppVersionResources handler itself needs no change beyond taking the minted value. The two id namespaces for this one member must also be reconciled so the resolve call and the resolution reads agree on one shape.
Traces: resiliencehub-app-version-draft-composition#task#12 (ResolveAppVersionResources on the composed draft) and resiliencehub-app-version-publish#task#6 (ResolveAppVersionResources in the publish sequence) — the same cluster, same divergence key `service=resiliencehub|ResolveAppVersionResources|status=200->200|keys=$.resolutionId`.

No HTTP status, error code or error message is involved: both the expected and the actual response are 200.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the booted emulator (emulator_up true, score 83.7%, 14 mismatched steps, 7 divergence clusters). This cluster reports owned_by: [] and stub_path: "" — it is not owned by any open issue and no unimplemented operation blocks it.

Fixture resiliencehub-app-version-draft-composition, step task#12, operation ResolveAppVersionResources, expected status 200, actual status 200:
  $.resolutionId: value_mismatch (expected '<shaped-not-asserted>', got '85187598-9117-496f-8139-58ce9d053bb3')

Affected cluster members (same divergence key): resiliencehub-app-version-draft-composition#task#12, resiliencehub-app-version-publish#task#6.

Reading of the expectation: '<shaped-not-asserted>' is how the corpus records a value it does not compare literally but whose SHAPE it asserts (the same expectation form the already-filed assessment/app timestamp divergences carry). A value_mismatch against it means the emulator's value does not satisfy the recorded shape class — here, a canonical hyphenated v4 UUID where AWS's recorded resolution handle has a different form. The particular UUID above is a per-run value and is NOT part of the requirement.

Model cited: smithy_query query="operation" operation="ResolveAppVersionResources,DescribeAppVersionResourcesResolutionStatus" and query="doc" shape="ResolveAppVersionResourcesResponse" member="resolutionId" on services/service-com-amazonaws-resiliencehub-awsresiliencehub/smithy-model/com-amazonaws-resiliencehub-awsresiliencehub.json — `resolutionId` -> String255, @required, @length 1..255, 'The identifier for a specific resolution'; the model states no pattern, so the form is recorded behavior only.

Emulator source read while grounding the cause: services/service-com-amazonaws-resiliencehub-awsresiliencehub/src/ops/resolve_app_version_resources.rs lines 193-201 (`store.start_resolution(..)`, then `resolution_id: resolution.resolution_id.clone()`); services/service-com-amazonaws-resiliencehub-awsresiliencehub/src/state.rs lines 420-438 (`ResourceResolution::started_at` -> `mint_resolution_id()`) and lines 1286-1293 (`fn mint_resolution_id() -> String { ::uuid::Uuid::new_v4().hyphenated().to_string() }`), plus lines 1096-1112 (`mapping_resolution_id`, the SHA-256 handle the listing path synthesizes for the same member).

## Steps to Reproduce
1. Boot the resiliencehub emulator from this checkout and replay fixture resiliencehub-app-version-draft-composition, step task#12 (and resiliencehub-app-version-publish, step task#6, which diverges identically).
2. By hand: CreateApp, then compose its DRAFT version (CreateAppVersionAppComponent plus CreateAppVersionResource), then call ResolveAppVersionResources with appArn=<the app's ARN> and appVersion='draft'.
3. Observe the 200 response's `resolutionId` is a canonical hyphenated version-4 UUID (8-4-4-4-12 hex with the v4 version nibble), freshly minted per call.
4. Compare against the recorded AWS response for the same step: the corpus asserts the handle's shape, and the hyphenated-UUID form fails that assertion. Confirm a fix by the $.resolutionId shape assertion passing on both fixture steps, not by matching any captured id value.
5. Also call ListAppVersionResources on the same resolved version and compare the `resolutionId` it reports: the emulator answers a SHA-256-derived handle there, so the two paths of this one @required member do not even agree with each other.

Issue-Source: trace-sweep-divergence
Issue-Trace: resiliencehub-app-lifecycle
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resiliencehub-app-lifecycle resiliencehub-app-version-draft-composition resiliencehub-app-version-publish
Issue-Shape-Id: com.amazonaws.resiliencehub#AwsResilienceHub
Issue-Key: resiliencehub|UpdateApp|2xx->2xx|$.app.tags.resourceArn
Issue-Key: resiliencehub|ResolveAppVersionResources|2xx->2xx|$.resolutionId
Issue-Commit: 4caa4cc5e4e99526db0f9a1110584524dc75eb12

# resiliencehub app-plane responses diverge: UpdateApp omits the application's tag entries and ResolveAppVersionResources mints a resolutionId the recording does not accept

> _Note: this issue was identified against the code base at commit `4caa4cc5e4e99526db0f9a1110584524dc75eb12`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateApp

## Description
Two net-new causes in the resiliencehub app plane, both 200-status response-body divergences replayed from the wire corpus. This issue bundles BOTH; both must be closed.

Cause 1 of 2 — UpdateApp's returned App carries no tag entries (fixture resiliencehub-app-lifecycle, step task#5).
AWS: UpdateApp returns the whole application record, `app` (shape App, required) — Smithy: UpdateApp output member `app` -> App; App$tags targets TagMap (@sensitive map, @length 1..50, key TagKey @length 1..128 pattern ^[^\x00-\x1f\x22]+$, value TagValue @length 0..256). An update does not touch the tag set, so the App that UpdateApp answers reports exactly the tags the application carries at that moment — i.e. every key/value pair put there by CreateApp$tags or by a later TagResource — and a read of the same application (DescribeApp, ListTagsForResource) agrees with it. The tag KEY set is the requirement; a tag's value is whatever the request/state put there (in this flow the harness tags the application with its own app ARN, a per-run value, so only the presence of the entry is a requirement, not the ARN text).
Emulator: the replayed UpdateApp answers 200 with the right status, but the tag entry the application carries is absent from the response: `$.app.tags.<key>` is missing (expected the stored value, got None) where the recording reports it. The tag key itself is gone from the map, not merely carrying a different value.
Why: a client that updates an application and then reads `app.tags` off the UpdateApp response (the common pattern for "update and re-check the resource") concludes the application is untagged, or that the update wiped its tags. Tag-driven logic — cost allocation, ownership lookup, any ABAC-ish branch keyed on a tag — then takes the wrong branch, and a diff against a prior DescribeApp read shows a spurious tag removal that no call made.
Fix: UpdateApp's handler and the App projection it answers with — services/service-com-amazonaws-resiliencehub-awsresiliencehub/src/ops/update_app.rs (`handle` returns `store.reported_app(...)`) together with `reported_app`/`tags_of` in src/state.rs: the returned record must carry the stored tag map that the preceding tag write in the same flow left on the application. Hypothesis: either the UpdateApp response path drops App$tags, or the tag write earlier in this flow never lands on the stored application, so the member is empty by the time UpdateApp reads it; replaying the fixture end-to-end distinguishes the two.
Traces: resiliencehub-app-lifecycle#task#5.

Cause 2 of 2 — ResolveAppVersionResources' resolutionId is a freshly minted bare UUID the recording does not accept (fixture resiliencehub-app-version-draft-composition, step task#12; also resiliencehub-app-version-publish#task#6).
AWS: recorded behavior only for the handle's form — Smithy constrains ResolveAppVersionResources output member `resolutionId` only as String255, required, @length 1..255, documented as "The identifier for a specific resolution"; the operation answers 200 with appArn, appVersion, resolutionId and status (ResourceResolutionStatusType: Failed | InProgress | Pending | Success). The recording asserts resolutionId structurally rather than literally, and the token the emulator answers does not satisfy that assertion, while appArn, appVersion and status all match.
Emulator: the handler answers 200 and mints a brand-new bare hyphenated v4 UUID per call — `mint_resolution_id()` in src/state.rs returns `Uuid::new_v4().hyphenated().to_string()` and ResourceResolution::started_at stamps it on every resolution — and the replayed value (a 8-4-4-4-12 UUID) is reported as a value_mismatch against the recording's resolution handle on both the draft and the release path.
Why: the resolutionId is the handle a client carries from ResolveAppVersionResources into DescribeAppVersionResourcesResolutionStatus and ListAppVersionResources. A client that validates the handle's form, keys state on it, or expects the handle for an app version that is already resolved to be the one the service is reporting, is handed a token of the wrong form/lineage: resolution-scoped reads it issues with a handle obtained elsewhere in the flow then address a resolution that does not exist, or it re-resolves instead of reading the resolution it already has.
Fix: the resolution-handle minter and the resolve handler's reporting of it — src/state.rs (`mint_resolution_id`, `ResourceResolution::started_at`, `start_resolution`) and src/ops/resolve_app_version_resources.rs. Hypothesis: AWS's resolution handle either has a form other than a bare UUID, or is NOT fresh per call for an app version that already has a current resolution (a resolve of an unchanged version reports the existing resolution's handle); the fixture's recorded resolutionId for task#12/task#6 states which, so read it before changing the minter.
Traces: resiliencehub-app-version-draft-composition#task#12, resiliencehub-app-version-publish#task#6.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200; 86 steps, 77 passed, score 89.5%).

Cause 1 — service=resiliencehub, operation UpdateApp, verdict mismatch, expected status 200, actual status 200:
  divergence_key: service=resiliencehub|UpdateApp|status=200->200|keys=$.app.tags.resourceArn
  detail: "$.app.tags.resourceArn: missing (expected 'arn:aws:resiliencehub:us-west-2:000000000000:app/bab98b0e-71f2-40de-b999-2f708471f043', got None)"
  fixture_id: resiliencehub-app-lifecycle, step_key: task#5 (affected: resiliencehub-app-lifecycle#task#5)
  Reading: the status is right and every other App member matches; the tag entry is absent (got None), so the response's tags map lacks the key. The expected VALUE shown is the application's own ARN — a per-run value, not a requirement; the requirement is that the entry the application carries appears in `app.tags`.

Cause 2 — service=resiliencehub, operation ResolveAppVersionResources, verdict mismatch, expected status 200, actual status 200:
  divergence_key: service=resiliencehub|ResolveAppVersionResources|status=200->200|keys=$.resolutionId
  detail: "$.resolutionId: value_mismatch (expected '<shaped-not-asserted>', got '333b5dc1-d6dd-4d45-b3ac-f14322cc3639')"
  fixture_id: resiliencehub-app-version-draft-composition, step_key: task#12 (affected: resiliencehub-app-version-draft-composition#task#12, resiliencehub-app-version-publish#task#6)
  Reading: appArn, appVersion and status pass; only the resolution handle diverges, and it diverges on both the draft-composition and the publish flow. The emulator's value is the bare v4 UUID produced by mint_resolution_id (src/state.rs); the id text itself is per-run, the divergence is that the handle does not satisfy the recording's structural assertion.

Neither cluster carries a stub_path or an owned_by, so neither is covered by an already-filed issue. No open issue names resiliencehub (the existing resiliencehub* issues are all for the separate resiliencehubv2 service).

## Steps to Reproduce
1. git clone the project and boot the emulator built from that checkout.
2. Replay the wire corpus of trace set simparity-wire-aws-v1.
3. Cause 1: replay fixture resiliencehub-app-lifecycle and stop at step task#5 (UpdateApp). Equivalent by hand: CreateApp; apply a tag to the returned appArn as the fixture's earlier steps do; call UpdateApp on that appArn (e.g. changing description). Inspect the 200 response: `$.app.tags` does not contain the tag key the application carries, while a DescribeApp/ListTagsForResource read of the same appArn is expected to report it.
4. Cause 2: replay fixture resiliencehub-app-version-draft-composition step task#12, and resiliencehub-app-version-publish step task#6 (ResolveAppVersionResources). Equivalent by hand: CreateApp, compose/publish an app version as the fixtures do, then POST /resolve-app-version-resources with that appArn and appVersion. The 200 response's resolutionId is a fresh bare 8-4-4-4-12 UUID and does not match the resolution handle the recording asserts; calling the operation twice shows a brand-new handle each time.

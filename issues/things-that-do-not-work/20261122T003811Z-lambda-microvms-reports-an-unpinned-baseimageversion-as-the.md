Issue-Source: trace-sweep-divergence
Issue-Trace: lambdamicrovms-customer-image-lifecycle
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambdamicrovms-customer-image-lifecycle lambdamicrovms-image-version-status-and-update
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda-microvms|ListMicrovmImageVersions|2xx->2xx|$.items[*].baseImageVersion
Issue-Key: lambda-microvms|UpdateMicrovmImageVersion|2xx->2xx|$.baseImageVersion
Issue-Key: lambda-microvms|UpdateMicrovmImage|2xx->2xx|$.baseImageVersion
Issue-Key: lambda-microvms|ListMicrovmImageVersions|2xx->2xx|$.items[*]
Issue-Commit: 23cfdf267b7e96fa94653a5b0d201ffb4c78b3ac

# lambda-microvms reports an unpinned baseImageVersion as the catalogue's bare integer generation instead of the resolved version label

> _Note: this issue was identified against the code base at commit `23cfdf267b7e96fa94653a5b0d201ffb4c78b3ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListMicrovmImageVersions

## Description
This issue bundles 4 distinct causes in the lambda-microvms emulator, all observed on replay of wire corpus simparity-wire-aws-v1 and all of ONE fault: when a create or a configuration update pins no baseImageVersion, this emulator resolves the member from its own managed base-image catalogue, which publishes generations as BARE INTEGERS ("1", "0"), and stores that bare integer on the version record. Every later projection of that record therefore reports baseImageVersion '1', where AWS reports the dotted version label the named base image publishes (recorded in this corpus as '1.0', '2.0' and '3.1' in different runs/images). A fixer must close all four (Cause 1 of 4 .. Cause 4 of 4) before submitting; each cause names the fixture step it must be re-replayed on, and all four clusters are reported by the replay with an EMPTY stub_path and an EMPTY owned_by.

WHAT IS AND IS NOT THE REQUIREMENT: the particular recorded labels ('2.0' at lambdamicrovms-customer-image-lifecycle, '1.0' at lambdamicrovms-image-version-status-and-update) are that run's resolved values and must NOT be hard-coded. The requirement is (a) that the member reports the version RESOLVED from the base image the request named, and (b) that the label has the form AWS publishes a base-image version in — a dotted major.minor label, never a bare integer generation. The emulator's own catalogue table is part of the fault: MANAGED_BASE_IMAGE_VERSIONS (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/state.rs:582-588) publishes "1" and "0", so the resolution has no dotted label to return, while the service mints its OWN image versions in major.minor form (FIRST_IMAGE_VERSION = "1.0", src/state.rs:347-353, documented "Versions are named in this service's major.minor form").

RELATED, NOT COVERING THESE: open issue 20261117T040517Z-lambda-microvms-answers-microvm-and-image-version-reads.md carries this defect class (its Cause 4) but only on the CreateMicrovmImage and GetMicrovmImageVersion keys — the replay reports those two clusters as owned by it and reports the four clusters below with an empty owned_by. Open issue 20261108T125129Z-lambda-microvms-image-version-and-build-read-paths.md is named by that issue as holding the listing row of the OLD '1.0' constant; it does not cover the bare-integer value this checkout now serves on the listing, the status update or the configuration update. Sibling bundle filed in this same sweep: the lambda-service copy of this fault (fixture lambda-microvms-image-lifecycle).

=== Cause 1 of 4: ListMicrovmImageVersions rows report baseImageVersion '1' for a version built from a base image with no pinned version ===
AWS: Smithy ListMicrovmImageVersions GET /2025-09-09/microvm-images/{imageIdentifier}/versions -> 200, output items: MicrovmImageVersionSummaryList (@required); each row carries baseImageVersion targeting Version (@length 1..2048, pattern ^[^\s]+$), documented "The specific version of the base MicroVM image to use". The model fixes no value, so for the value this is recorded behavior only: the recording shows the listing row reporting the SAME resolved, dotted base-image version the create response reported for that version record (recorded '2.0').
Emulator: the row reports baseImageVersion '1'. Replay (200 vs 200): `$.items[0].baseImageVersion: value_mismatch (expected '2.0', got '1')`.
Why: a client that audits base-layer pedigree across an image's versions reads the listing, not each version individually. Every row claims base version '1' — a label that is not even in the form the version-addressed calls take back — so a reproducible-build gate or a re-base sweep is told a build sits on a base layer it does not, and a client that feeds the label onward as a version string carries a malformed value.
Fix: the base-image version resolution in the service state (ComAmazonawsLambdamicrovmsLambdamicrovmsState::resolve_base_image_version, src/state/regions.rs:248-261, which returns the catalogue's bare generation) together with the catalogue labels it resolves from (MANAGED_BASE_IMAGE_VERSIONS, src/state.rs:582-588) — not the listing projection, which faithfully echoes the stored record.
Traces: lambdamicrovms-customer-image-lifecycle#task#7 (ListMicrovmImageVersions, 200/200, divergence_key service=lambda-microvms|ListMicrovmImageVersions|status=200->200|keys=$.items[0].baseImageVersion).

=== Cause 2 of 4: UpdateMicrovmImageVersion echoes the same bare integer on the status-change response ===
AWS: Smithy UpdateMicrovmImageVersion PATCH /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion} -> 200; UpdateMicrovmImageVersionResponse$baseImageVersion targets Version (@length 1..2048, pattern ^[^\s]+$), "The specific version of the base MicroVM image to use". The call changes only status, so the member must report the version record's stored base-image pedigree unchanged; recorded behavior: '1.0' for that image's version.
Emulator: answers 200 with baseImageVersion '1'. Replay: `$.baseImageVersion: value_mismatch (expected '1.0', got '1')`.
Why: a client that flips a version ACTIVE/INACTIVE uses this response as the post-change view of the version. It is told the version's base pedigree is '1', so a promotion check that compares the activated version's base layer against the one it qualified refuses or mis-promotes, and a client caching this response records the wrong pedigree for the version it just activated.
Fix: the stored value, i.e. the same resolution above (resolve_base_image_version, src/state/regions.rs:248-261, reached through the unwrap_or_else at src/ops/create_microvm_image.rs:388-390 and src/ops/update_microvm_image.rs:373-377); the status handler only projects the record.
Traces: lambdamicrovms-image-version-status-and-update#task#4 (UpdateMicrovmImageVersion, 200/200, divergence_key service=lambda-microvms|UpdateMicrovmImageVersion|status=200->200|keys=$.baseImageVersion).

=== Cause 3 of 4: UpdateMicrovmImage resolves the new version's unpinned baseImageVersion to the same bare integer ===
AWS: Smithy UpdateMicrovmImage PUT /2025-09-09/microvm-images/{imageIdentifier} -> 200; UpdateMicrovmImageResponse$baseImageVersion targets Version (@length 1..2048, pattern ^[^\s]+$), "The specific version of the base MicroVM image to use"; the input member of the same name is optional, so a PUT that omits it has the service RESOLVE one from the required baseImageArn it names. Recorded behavior: the 200 reports the resolved dotted label '1.0'.
Emulator: answers 200 with baseImageVersion '1'. Replay: `$.baseImageVersion: value_mismatch (expected '1.0', got '1')`.
Why: the update response is the only place a client sees the configuration the service actually accepted for the version it just minted. A client that re-sends that configuration (PUT semantics: it must send every required field back) reads '1' and either pins a bare integer the next time or cannot reproduce the configuration at all, so a configuration round-trip is not idempotent against this emulator.
Fix: same resolution and catalogue labels (src/state/regions.rs:248-261 and src/state.rs:582-588); the update handler's unwrap_or_else at src/ops/update_microvm_image.rs:373-377 correctly honors a PINNED value and must keep doing so.
Traces: lambdamicrovms-image-version-status-and-update#task#5 (UpdateMicrovmImage, 200/200, divergence_key service=lambda-microvms|UpdateMicrovmImage|status=200->200|keys=$.baseImageVersion).

=== Cause 4 of 4: the version listing of an updated image reports the bare integer on every row ===
AWS: as Cause 1 (ListMicrovmImageVersions -> 200, items[].baseImageVersion, Version @length 1..2048, pattern ^[^\s]+$). Recorded behavior: each row reports the dotted base-image version its version record was built from ('1.0' in this fixture).
Emulator: the row reports baseImageVersion '1'. Replay: `$.items[0]: missing/extra` with the asserted member differing as `baseImageVersion: '1.0' -> '1'`. NOTE: the same row's createdAt/updatedAt differ only as clock instants of the replay run and versionStateTimeBucket is recorded shaped-not-asserted; neither is a requirement and neither is part of this cause.
Why: same client harm as Cause 1, on the path a client uses after an update to confirm which base layer the new version landed on — the listing is the paging view, so the wrong pedigree is what a sweep over many images sees.
Fix: same resolution and catalogue labels as Cause 1.
Traces: lambdamicrovms-image-version-status-and-update#task#7 (ListMicrovmImageVersions, 200/200, divergence_key service=lambda-microvms|ListMicrovmImageVersions|status=200->200|keys=$.items[0],$.items[1]).

## Evidence
Wire replay of trace set simparity-wire-aws-v1 (emulator_up true, "/__admin/health answered 200 and the :34375 data listener accepts"; 13 fixtures replayed, 104 steps, 89 passed, 15 mismatched, 0 errored, 0 blocked, score 85.6%, 13 divergence clusters, bootstrap_signal false). Expected = recorded AWS response, Actual = emulator. All four clusters carry stub_path EMPTY and owned_by EMPTY.

Cause 1 — lambdamicrovms-customer-image-lifecycle#task#7, ListMicrovmImageVersions, status 200 vs 200:
  $.items[0].baseImageVersion: value_mismatch (expected '2.0', got '1')
  divergence_key: service=lambda-microvms|ListMicrovmImageVersions|status=200->200|keys=$.items[0].baseImageVersion

Cause 2 — lambdamicrovms-image-version-status-and-update#task#4, UpdateMicrovmImageVersion, status 200 vs 200:
  $.baseImageVersion: value_mismatch (expected '1.0', got '1')
  divergence_key: service=lambda-microvms|UpdateMicrovmImageVersion|status=200->200|keys=$.baseImageVersion

Cause 3 — lambdamicrovms-image-version-status-and-update#task#5, UpdateMicrovmImage, status 200 vs 200:
  $.baseImageVersion: value_mismatch (expected '1.0', got '1')
  divergence_key: service=lambda-microvms|UpdateMicrovmImage|status=200->200|keys=$.baseImageVersion

Cause 4 — lambdamicrovms-image-version-status-and-update#task#7, ListMicrovmImageVersions, status 200 vs 200:
  $.items[0]: missing (expected {... 'baseImageVersion': '1.0', 'imageVersion': '2.0', 'state': 'FAILED', 'status': 'INACTIVE' ...}, got {... 'baseImageVersion': '1', 'imageVersion': '2.0', 'state': 'FAILED', 'status': 'INACTIVE' ...})
  divergence_key: service=lambda-microvms|ListMicrovmImageVersions|status=200->200|keys=$.items[0],$.items[1]
  (The row's createdAt 1791829806.635 -> 1795307500.058 and updatedAt 1791829850.181 -> 1795307505.08 are per-run clock instants and versionStateTimeBucket is recorded as '<shaped-not-asserted>'; the asserted member that diverges is baseImageVersion.)

Model grounding (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json, protocol restJson1): CreateMicrovmImage POST /2025-09-09/microvm-images -> 201 with input baseImageVersion OPTIONAL (Version, @length 1..2048, pattern ^[^\s]+$, "The specific version of the base MicroVM image to use") and the same member on the output; UpdateMicrovmImage PUT /2025-09-09/microvm-images/{imageIdentifier} -> 200, same optional input member and output member; UpdateMicrovmImageVersion PATCH /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion} -> 200 (input: imageIdentifier, imageVersion, status only) with output baseImageVersion; ListMicrovmImageVersions GET /2025-09-09/microvm-images/{imageIdentifier}/versions -> 200 with items @required.

Code sightings (orientation only): src/state/regions.rs:248-261 (resolve_base_image_version — returns the catalogue's bare generation via latest_managed_base_image_version); src/state.rs:582-588 (MANAGED_BASE_IMAGE_VERSIONS publishes "1" and "0"); src/state.rs:347-353 (FIRST_IMAGE_VERSION = "1.0", the service's documented major.minor version form); src/ops/create_microvm_image.rs:388-390 and src/ops/update_microvm_image.rs:373-377 (the unwrap_or_else into that resolution, storing the result on the version record).

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. base = ListManagedMicrovmImages()['items'][0]['imageArn']  (the AWS-managed al2023-1 base image).
3. CreateMicrovmImage(name=<unique>, baseImageArn=base, buildRoleArn=<an IAM role arn>, codeArtifact={'uri': 's3://<unique>/code.zip'}) with NO baseImageVersion, and let the build settle.
4. Cause 1/Cause 4: ListMicrovmImageVersions(imageIdentifier=<imageArn>) — AWS reports each row's baseImageVersion as the dotted label resolved from `base` (recorded '2.0' / '1.0'); the emulator reports '1' on every row.
5. Cause 3: UpdateMicrovmImage(imageIdentifier=<imageArn>, baseImageArn=base, buildRoleArn=..., codeArtifact={'uri': 's3://<unique>/code2.zip'}) with NO baseImageVersion — the 200 reports '1' instead of the resolved dotted label. (Pass baseImageVersion explicitly and the emulator does echo it, which isolates the resolution rather than the handler.)
6. Cause 2: UpdateMicrovmImageVersion(imageIdentifier=<imageArn>, imageVersion='1.0', status='INACTIVE') — the 200 echoes baseImageVersion '1' from the stored record.
7. Re-replay to confirm: lambdamicrovms-customer-image-lifecycle step task#7, and lambdamicrovms-image-version-status-and-update steps task#4, task#5 and task#7 — each must report baseImageVersion as the version resolved from the named base image, in the dotted label form the catalogue publishes.

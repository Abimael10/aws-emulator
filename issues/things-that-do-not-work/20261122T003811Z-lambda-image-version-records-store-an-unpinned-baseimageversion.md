Issue-Source: trace-sweep-divergence
Issue-Trace: lambda-microvms-image-lifecycle
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-microvms-image-lifecycle
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda|CreateMicrovmImage|2xx->2xx|$.baseImageVersion
Issue-Key: lambda|UpdateMicrovmImageVersion|2xx->2xx|$.baseImageVersion
Issue-Commit: 23cfdf267b7e96fa94653a5b0d201ffb4c78b3ac

# lambda image-version records store an unpinned baseImageVersion as a bare integer generation, so create and status reads report the wrong base pedigree

> _Note: this issue was identified against the code base at commit `23cfdf267b7e96fa94653a5b0d201ffb4c78b3ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMicrovmImage

## Description
This issue bundles 2 distinct causes seen on the lambda service token, both observed on replay of wire corpus simparity-wire-aws-v1 and both of ONE fault: a MicroVM-image version whose create/update pinned no baseImageVersion has the member resolved from this emulator's managed base-image catalogue, which publishes generations as BARE INTEGERS ("1", "0"), and that bare integer is stored on the version record and echoed by every projection of it. A fixer must close both (Cause 1 of 2 and Cause 2 of 2) before submitting; each cause names the fixture step it must be re-replayed on, and both clusters are reported by the replay with an EMPTY stub_path and an EMPTY owned_by.

WHAT IS AND IS NOT THE REQUIREMENT: the recorded labels ('1.0' at task#2, '3.1' at task#18) are the values this run's base image resolved to and must NOT be hard-coded. The requirement is that the member reports the version RESOLVED from the base image the request named, in the dotted major.minor label form a base-image version is published in — never a bare integer generation. The emulator's catalogue table is part of the fault: MANAGED_BASE_IMAGE_VERSIONS (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/state.rs:582-588) publishes "1" and "0", so the resolution has no dotted label to return, while the service mints its own image versions in major.minor form (FIRST_IMAGE_VERSION = "1.0", src/state.rs:347-353).

RELATED, NOT COVERING THESE: open issue 20261117T040517Z-lambda-microvms-answers-microvm-and-image-version-reads.md holds this defect class as its Cause 4, but only on the lambda-microvms keys for CreateMicrovmImage and GetMicrovmImageVersion in fixture lambdamicrovms-customer-image-lifecycle; the replay reports the two clusters below (fixture lambda-microvms-image-lifecycle, service token lambda) with an empty owned_by. A sibling bundle for the lambda-microvms token is filed in this same sweep.

=== Cause 1 of 2: CreateMicrovmImage answers baseImageVersion '1' for a create that pinned none ===
AWS: Smithy CreateMicrovmImage POST /2025-09-09/microvm-images -> 201; input baseImageVersion is OPTIONAL (Version, @length 1..2048, pattern ^[^\s]+$, documented "The specific version of the base MicroVM image to use"), and CreateMicrovmImageResponse$baseImageVersion carries the same shape, so a create that pins none has the service RESOLVE one from the @required baseImageArn it named. The model fixes no value: recorded behavior only — the 201 reports the dotted version label resolved for the named base image (recorded '1.0' here).
Emulator: answers 201 with baseImageVersion '1'. Replay: `$.baseImageVersion: value_mismatch (expected '1.0', got '1')`.
Why: the create response is the only place a client sees the configuration the service actually accepted. Every image this emulator creates claims a base version of '1' whatever base image it was built on, and in a label form that is not the one the version-addressed calls take back, so a reproducible-build gate, a CVE re-base sweep or a promotion check reads a pedigree the build does not have and never learns a re-base is due.
Fix: the base-image version resolution in the service state (ComAmazonawsLambdamicrovmsLambdamicrovmsState::resolve_base_image_version, src/state/regions.rs:248-261) and the catalogue labels it resolves from (MANAGED_BASE_IMAGE_VERSIONS, src/state.rs:582-588) — the handler's unwrap_or_else (src/ops/create_microvm_image.rs:388-390) correctly honors a PINNED value and must keep doing so.
Traces: lambda-microvms-image-lifecycle#task#2 (CreateMicrovmImage, 201/201, divergence_key service=lambda|CreateMicrovmImage|status=201->201|keys=$.baseImageVersion).

=== Cause 2 of 2: UpdateMicrovmImageVersion echoes that bare integer instead of the version record's resolved pedigree ===
AWS: Smithy UpdateMicrovmImageVersion PATCH /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion} -> 200; its input carries only imageIdentifier, imageVersion and status, so UpdateMicrovmImageVersionResponse$baseImageVersion (Version, @length 1..2048, pattern ^[^\s]+$) must report the addressed version record's stored base-image pedigree unchanged. Recorded behavior: the 200 reports the dotted label that version was built from (recorded '3.1' at this step, a value resolved from the base image that create named — not a constant of the API).
Emulator: answers 200 with baseImageVersion '1'. Replay: `$.baseImageVersion: value_mismatch (expected '3.1', got '1')`.
Why: a client flipping a version ACTIVE/INACTIVE takes this response as the post-change view of the version. It reports base version '1' for a version built on a different base generation, so a promotion gate comparing the activated version's base layer with the one it qualified mis-promotes or refuses, and any audit record written from this response is wrong.
Fix: the stored value — the same resolution as Cause 1 (src/state/regions.rs:248-261 plus the catalogue labels at src/state.rs:582-588), persisted by create (src/ops/create_microvm_image.rs:388-390) and by the configuration update (src/ops/update_microvm_image.rs:373-377); the status handler only projects the stored record.
Traces: lambda-microvms-image-lifecycle#task#18 (UpdateMicrovmImageVersion, 200/200, divergence_key service=lambda|UpdateMicrovmImageVersion|status=200->200|keys=$.baseImageVersion).

## Evidence
Wire replay of trace set simparity-wire-aws-v1 (emulator_up true, "/__admin/health answered 200 and the :34375 data listener accepts"; 13 fixtures replayed, 104 steps, 89 passed, 15 mismatched, 0 errored, 0 blocked, score 85.6%, 13 divergence clusters, bootstrap_signal false). Expected = recorded AWS response, Actual = emulator. Both clusters carry stub_path EMPTY and owned_by EMPTY.

Cause 1 — lambda-microvms-image-lifecycle#task#2, CreateMicrovmImage, status 201 vs 201:
  $.baseImageVersion: value_mismatch (expected '1.0', got '1')
  divergence_key: service=lambda|CreateMicrovmImage|status=201->201|keys=$.baseImageVersion

Cause 2 — lambda-microvms-image-lifecycle#task#18, UpdateMicrovmImageVersion, status 200 vs 200:
  $.baseImageVersion: value_mismatch (expected '3.1', got '1')
  divergence_key: service=lambda|UpdateMicrovmImageVersion|status=200->200|keys=$.baseImageVersion

That the emulator answers the SAME '1' for two different images and two different operations is the signature of the shared fault: the value comes from the catalogue's newest bare generation rather than from the named base image's published version label.

Model grounding (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json, protocol restJson1): CreateMicrovmImage POST /2025-09-09/microvm-images -> 201, input baseImageVersion optional (Version, @length 1..2048, pattern ^[^\s]+$, "The specific version of the base MicroVM image to use"), output baseImageVersion same shape, baseImageArn @required; UpdateMicrovmImageVersion PATCH /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion} -> 200, input {imageIdentifier, imageVersion, status} only, output baseImageVersion same shape.

Code sightings (orientation only): src/state/regions.rs:248-261 (resolve_base_image_version — returns the managed catalogue's bare generation); src/state.rs:582-588 (MANAGED_BASE_IMAGE_VERSIONS publishes "1" and "0"); src/state.rs:347-353 (FIRST_IMAGE_VERSION = "1.0", the documented major.minor form); src/ops/create_microvm_image.rs:388-390 and src/ops/update_microvm_image.rs:373-377 (the unwrap_or_else into that resolution, whose result is stored on the version record).

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. base = ListManagedMicrovmImages()['items'][0]['imageArn']  (the AWS-managed al2023-1 base image).
3. Cause 1: CreateMicrovmImage(name=<unique>, baseImageArn=base, buildRoleArn=<an IAM role arn>, codeArtifact={'uri': 's3://<unique>/code.zip'}) with NO baseImageVersion. AWS: the 201 reports the version resolved for `base` as a dotted label (recorded '1.0'). Emulator: '1', for every create and every base image.
4. Let the version's build settle, then Cause 2: UpdateMicrovmImageVersion(imageIdentifier=<imageArn>, imageVersion='1.0', status='INACTIVE') — the 200 echoes baseImageVersion '1' from the stored version record instead of the pedigree the version was built from.
5. Re-replay fixture lambda-microvms-image-lifecycle steps task#2 and task#18 to confirm: both must report baseImageVersion as the version resolved from the named base image, in the dotted label form a base-image version is published in.

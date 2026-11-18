Issue-Source: trace-sweep-divergence
Issue-Trace: lambdamicrovms-customer-image-lifecycle
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambdamicrovms-customer-image-lifecycle
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda-microvms|ListMicrovmImageVersions|2xx->2xx|$.items[*].baseImageVersion
Issue-Key: lambda-microvms|ListMicrovmImageBuilds|2xx->2xx|$.items[*]
Issue-Commit: 6ad539f79015bc27d2d0777d4c5c145ae7d45c50

# lambda-microvms image version and build listing rows are projected from emulator defaults rather than the resolved base-image version and AWS's timestamp form

> _Note: this issue was identified against the code base at commit `6ad539f79015bc27d2d0777d4c5c145ae7d45c50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListMicrovmImageVersions

## Description
This issue bundles 2 distinct root causes in the lambda-microvms emulator, both observed on replay of wire corpus simparity-wire-aws-v1 and both of one defect class: a row of an image-version / image-build LISTING carries a member the service itself is supposed to supply (the base-image version the version was built from, and the instant the build was recorded at), and the emulator fills it from its own default or its own clock format instead of from the resolution AWS performs and the form AWS serializes. A fixer must close BOTH causes (Cause 1 of 2 and Cause 2 of 2) before submitting; each cause names the fixture step it must be re-replayed on. Both clusters are reported by the replay with an EMPTY stub_path and an EMPTY owned_by.

=== Cause 1 of 2: every ListMicrovmImageVersions row reports baseImageVersion as the constant '1.0' instead of the base-image version the version resolved to ===
Fixture/step: lambdamicrovms-customer-image-lifecycle#task#7 (ListMicrovmImageVersions, 200 vs 200).
AWS: Smithy ListMicrovmImageVersions GET /2025-09-09/microvm-images/{imageIdentifier}/versions -> 200, output items @required MicrovmImageVersionSummaryList; MicrovmImageVersionSummary$baseImageVersion is optional, targets Version (@length 1..2048, pattern ^[^\s]+$) and is documented "The specific version of the base MicroVM image." The member reports the base-image version the addressed image VERSION was actually built from: the version the create/update request pinned, and for a request that pinned none the version the service RESOLVES for the named baseImageArn. For the resolved value itself this is recorded behavior only: the corpus records a create against the managed base image arn:aws:lambda:us-west-2:aws:microvm-image:al2023-1 with NO baseImageVersion, and the subsequent listing row reports the base version resolved for that base image (this run's recorded literal is '2.0' — that literal is this run's catalogue generation, not the requirement; the requirement is that the row reports whatever version the named base image resolves to, in the label form the catalogue publishes it in, rather than a constant).
Emulator: the listing row answers baseImageVersion '1.0' for every version, whatever base image it was built on — replay reports `$.items[0].baseImageVersion: value_mismatch (expected '2.0', got '1.0')` at 200/200. The value is the one stored on the version record at create time: CreateMicrovmImage resolves an omitted baseImageVersion through crate::state::...::resolve_base_image_version (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/state/regions.rs), which does not consult the managed base-image catalogue and returns the FIRST_IMAGE_VERSION constant ("1.0") for every managed base-image reference; ListMicrovmImageVersions then projects that stored value verbatim through its row projection (src/ops/list_microvm_image_versions.rs, summary_of). The emulator's own catalogue cannot supply the recorded label either, because MANAGED_BASE_IMAGE_VERSIONS publishes plain integer generations.
Why: baseImageVersion is how a client pins and audits the base layer of an image, and the LISTING is where a client sweeps every version of an image at once. Every row claims base version 1.0, so a reproducible-build gate, a CVE re-base sweep or a promotion check that enumerates an image's versions is told each build pinned a base layer it did not, and never learns a re-base is due — a client that filters rows by base version to decide what to rebuild selects nothing, or everything, incorrectly.
Fix: the base-image version RESOLUTION in the service state (resolve_base_image_version in src/state/regions.rs) must resolve an unpinned reference from the named base image — for an AWS-managed reference, from the catalogue that publishes it — and the version record must store that resolved value so the listing row, the single-version read and the create response all report one pedigree; the catalogue's published version labels must be the labels AWS publishes so the resolution has the recorded label to return. The listing handler's projection itself is faithful and does not need changing.
RELATED, NOT COVERING THIS: open issue 20261117T040517Z-lambda-microvms-answers-microvm-and-image-version-reads.md carries this same defect as its Cause 4, but ONLY on CreateMicrovmImage#setup#2 and GetMicrovmImageVersion#task#6 — it holds no key for the ListMicrovmImageVersions cluster, which the replay reports with an empty owned_by. Issue 20261108T125129Z-lambda-microvms-image-version-and-build-read-paths.md did carry the listing row, but it is RESOLVED (status fixed), so it covers nothing: the divergence is live on this checkout.
Traces: lambdamicrovms-customer-image-lifecycle#task#7 (divergence_key service=lambda-microvms|ListMicrovmImageVersions|status=200->200|keys=$.items[0].baseImageVersion).

=== Cause 2 of 2: ListMicrovmImageBuilds rows serialize createdAt with full sub-second clock precision instead of AWS's millisecond-precision epoch-seconds ===
Fixture/step: lambdamicrovms-customer-image-lifecycle#task#8 (ListMicrovmImageBuilds, 200 vs 200, both rows of the page).
AWS: Smithy ListMicrovmImageBuilds GET /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}/builds -> 200, output items @required MicrovmImageBuildSummaries; MicrovmImageBuildSummary$createdAt is @required Timestamp, documented "The timestamp when the build was created." The protocol is restJson1 with no timestampFormat override on the member, so a Timestamp serializes as epoch-seconds. The INSTANT is a per-run value and is NOT the requirement; the serialized FORM is: every timestamp this corpus records for this service is epoch-seconds with exactly three fractional digits, i.e. millisecond precision (recorded in this corpus: 1793052797.597 on these build rows, 1781715610.862 and 1790702978.629 on the managed-image catalogue). Recorded behavior only for the precision rule — the model states no format override.
Emulator: both build rows of the page answer createdAt with six fractional digits — replay reports each expected row as `missing` and the page's rows differ from the recording in createdAt alone (`'createdAt': 1793052797.597` recorded vs `'createdAt': 1795018028.325499` served); every other asserted member of both rows (imageArn, imageVersion, buildState FAILED, architecture ARM_64, chipset GRAVITON, stateReason 'The S3 bucket does not exist.', terminationReasonCode null) matches, and buildId/chipsetGeneration are recorded as shaped, non-asserted. The row projects build.created_at verbatim (src/ops/list_microvm_image_builds.rs, payload_of -> created_at: build.created_at); the instant is recorded by crate::state::now_datetime (src/state.rs, `DateTime::from(SystemTime::now())`), which keeps the system clock's full sub-second precision, and the output serializer writes it out unrounded.
Why: the EXTRA precision, not the instant, is what breaks a client. A caller that round-trips a build's createdAt — keys a cache or a de-duplication record on it, writes it into an ETag-style string, compares a listing row's instant with the one the single-build read returned, or re-serializes it — cannot reproduce the value AWS would have produced, so an equality or ordering check against a millisecond-truncated copy of the same instant fails. A strict SDK or schema that models the member as millisecond epoch-seconds truncates or rejects the response silently.
Fix: the timestamp the service records and serializes for Timestamp members (crate::state::now_datetime in src/state.rs and/or the Timestamp output serialization) must emit epoch-seconds at the millisecond precision AWS emits, for every Timestamp member of this service so that no two reads of one record disagree. The listing handler and its row projection are faithful.
RELATED, NOT COVERING THIS: open issue 20261117T040517Z-lambda-microvms-answers-microvm-and-image-version-reads.md carries the same precision defect as its Cause 2, but only for RunMicrovm/GetMicrovm $.startedAt and $.terminatedAt; it holds no key for ListMicrovmImageBuilds, which the replay reports with an empty owned_by.
Traces: lambdamicrovms-customer-image-lifecycle#task#8 (divergence_key service=lambda-microvms|ListMicrovmImageBuilds|status=200->200|keys=$.items[0],$.items[1]).

## Evidence
Wire replay of trace set simparity-wire-aws-v1 (emulator_up true, "/__admin/health answered 200 and the :38051 data listener accepts"; 13 fixtures replayed, 104 steps, 92 passed, 12 mismatched, 0 errored, 0 blocked, score 88.5%, 10 divergence clusters, bootstrap_signal false). Expected = recorded AWS response, Actual = emulator. Both clusters below carry stub_path EMPTY and owned_by EMPTY.

Cause 1 — lambdamicrovms-customer-image-lifecycle#task#7, ListMicrovmImageVersions, status 200 vs 200:
  $.items[0].baseImageVersion: value_mismatch (expected '2.0', got '1.0')
  divergence_key: service=lambda-microvms|ListMicrovmImageVersions|status=200->200|keys=$.items[0].baseImageVersion
  (The image version under test was created at setup#2 against the managed base image arn:aws:lambda:us-west-2:aws:microvm-image:al2023-1 with NO baseImageVersion in the request; the recorded '2.0' is this run's resolved catalogue generation, the requirement is that the row report the version resolved for the named base image rather than a constant.)

Cause 2 — lambdamicrovms-customer-image-lifecycle#task#8, ListMicrovmImageBuilds, status 200 vs 200:
  $.items[0]: missing (expected {'architecture': 'ARM_64', 'buildId': '<shaped-not-asserted>', 'buildState': 'FAILED', 'chipset': 'GRAVITON', 'chipsetGeneration': '<shaped-not-asserted>', 'createdAt': 1793052797.597, 'imageArn': 'arn:aws:lambda:us-west-2:000000000000:microvm-image:sim2real-probe-mvmimg-8661b5c1', 'imageVersion': '1.0', 'stateReason': 'The S3 bucket does not exist.', 'terminationReasonCode': None}, got {... 'buildId': 'build-0000000000000000', ... 'chipsetGeneration': '2', ... 'createdAt': 1795018028.325499, ...})
  $.items[1]: missing (same expected row; got {... 'buildId': 'build-0000000000000001', ... 'chipsetGeneration': '3', ... 'createdAt': 1795018028.325499, ...})
  divergence_key: service=lambda-microvms|ListMicrovmImageBuilds|status=200->200|keys=$.items[0],$.items[1]
  (buildId and chipsetGeneration are recorded as shaped, non-asserted values and are not asserted here; every value-asserted member except createdAt matches. The divergence is the serialized PRECISION of createdAt — six fractional digits served against the three fractional digits every recorded timestamp of this corpus carries, e.g. 1793052797.597, 1781715610.862, 1790702978.629. The absolute instant is a per-run value and is explicitly NOT claimed as the requirement.)

Model grounding (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json):
  ListMicrovmImageVersions GET /2025-09-09/microvm-images/{imageIdentifier}/versions -> 200; output items @required MicrovmImageVersionSummaryList; MicrovmImageVersionSummary$baseImageVersion optional Version @length 1..2048 pattern ^[^\s]+$ ("The specific version of the base MicroVM image.").
  ListMicrovmImageBuilds GET /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}/builds -> 200; output items @required MicrovmImageBuildSummaries; MicrovmImageBuildSummary$createdAt @required Timestamp ("The timestamp when the build was created."), $buildId @required NonBlankString, $chipsetGeneration @required NonBlankString; protocol restJson1, no timestampFormat override on these members.

Code sightings (orientation only): src/ops/list_microvm_image_versions.rs (handle -> summary_of row projection of the stored version record); src/state/regions.rs resolve_base_image_version (returns the FIRST_IMAGE_VERSION constant for a managed base-image reference and does not consult the catalogue) with FIRST_IMAGE_VERSION = "1.0" and MANAGED_BASE_IMAGE_VERSIONS in src/state.rs; src/ops/list_microvm_image_builds.rs payload_of (`created_at: build.created_at`); crate::state::now_datetime in src/state.rs (`DateTime::from(SystemTime::now())`, full sub-second precision).

## Steps to Reproduce
Cause 1 (ListMicrovmImageVersions baseImageVersion):
1. Boot the emulator from this checkout.
2. base = ListManagedMicrovmImages()['items'][0]['imageArn']  (the managed al2023-1 base image).
3. CreateMicrovmImage(name=<unique>, baseImageArn=base, buildRoleArn=<an IAM role arn>, codeArtifact={'uri': 's3://<unique>/code.zip'}) with NO baseImageVersion in the request.
4. ListMicrovmImageVersions(imageIdentifier=<the new imageArn>) and read items[0]['baseImageVersion'].
   AWS: the version resolved for `base` (recorded in this corpus: '2.0').
   Emulator: '1.0' for every version, whatever base image it was built on.
5. Re-replay fixture lambdamicrovms-customer-image-lifecycle step task#7 to confirm the fix.

Cause 2 (ListMicrovmImageBuilds createdAt precision):
6. With the image from step 3 (its first build settles FAILED because the code artifact's S3 bucket does not exist), call ListMicrovmImageBuilds(imageIdentifier=<imageArn>, imageVersion='1.0').
7. Inspect the RAW JSON of each row's createdAt with repr()/a regex on the fractional part, not by eye:
   AWS: epoch-seconds with exactly three fractional digits (millisecond precision), e.g. 1793052797.597.
   Emulator: six fractional digits, e.g. 1795018028.325499.
   Do not assert the instant itself — it is per-run; assert only that the fractional part is at most three digits, and that the same build's createdAt read back through GetMicrovmImageBuild serializes identically.
8. Re-replay fixture lambdamicrovms-customer-image-lifecycle step task#8 to confirm the fix: both rows of the page must match the recorded rows on every value-asserted member, createdAt included at the recorded precision.

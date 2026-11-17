Issue-Source: trace-sweep-divergence
Issue-Trace: lambda-microvms-run-suspend-resume-terminate-microvm
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-microvms-run-suspend-resume-terminate-microvm lambdamicrovms-customer-image-lifecycle
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda-microvms|RunMicrovm|2xx->2xx|$.endpoint
Issue-Key: lambda-microvms|RunMicrovm|2xx->2xx|$.microvmId
Issue-Key: lambda-microvms|RunMicrovm|2xx->2xx|$.startedAt
Issue-Key: lambda-microvms|GetMicrovm|2xx->2xx|$.endpoint
Issue-Key: lambda-microvms|GetMicrovm|2xx->2xx|$.startedAt
Issue-Key: lambda-microvms|GetMicrovm|2xx->2xx|$.terminatedAt
Issue-Key: lambda-microvms|CreateMicrovmImage|2xx->2xx|$.baseImageVersion
Issue-Key: lambda-microvms|GetMicrovmImageVersion|2xx->2xx|$.baseImageVersion
Issue-Commit: d66b49b5d20b841ed555a4c1167817e25b409ab6

# lambda-microvms answers MicroVM and image-version reads with emulator-chosen values instead of the forms and resolutions AWS serves

> _Note: this issue was identified against the code base at commit `d66b49b5d20b841ed555a4c1167817e25b409ab6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunMicrovm

## Description
This issue bundles 4 distinct root causes in the lambda-microvms emulator, all observed on replay of wire corpus simparity-wire-aws-v1 and all of one defect class: a member whose value the SERVICE supplies (the MicroVM's published address, its identity, its lifecycle instants, and the base-image version an unpinned build resolves to) is answered from something this emulator chose for itself rather than in the form AWS serves or from the resolution AWS performs. A fixer must close all four (Cause 1 of 4 .. Cause 4 of 4) before submitting; each cause names the fixture step it must be re-replayed on, and all five clusters below are reported with an EMPTY stub_path and an EMPTY owned_by.

=== Cause 1 of 4: the published MicroVM endpoint is a loopback IP:port URL carrying the MicroVM id in a PATH, a URL form AWS does not publish ===
Fixtures/steps: lambda-microvms-run-suspend-resume-terminate-microvm#task#7 (RunMicrovm, 200/200), #task#8, #task#10, #task#12 (GetMicrovm, 200/200) and #task#14 (GetMicrovm, 200/200).
AWS: Smithy RunMicrovmResponse$endpoint and GetMicrovmResponse$endpoint are @required, target String with @length 1..2048, and are documented "The HTTPS endpoint URL for communicating with the MicroVM. Include a valid authentication token in the X-aws-proxy-auth header when sending requests." The model fixes no value, so for the value this is recorded behavior only: the corpus records this member as a SHAPED, non-asserted value — the recorded URL's text is deliberately not the requirement (it is a live-run address), but its FORM is, and the replay's shape check is what the emulator fails. The recorded form is an https URL whose authority is a resolvable DNS host of the service's own domain and which carries no control-plane-style resource path; Hypothesis: AWS carries the MicroVM's identity in that hostname rather than in a path segment, which is why a path-bearing loopback URL does not satisfy the member's recorded shape.
Emulator: every one of the five steps answers 200 with endpoint 'https://127.0.0.1:<port>/microvms/<microvmId>' (replay: 'https://127.0.0.1:35025/microvms/mvm-0000000000000000'); the replay reports `$.endpoint: value_mismatch (expected '<shaped-not-asserted>', got ...)`. The address is constructed deliberately as the emulator's own loopback HTTPS listener with the MicroVM named in the path (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/state/microvm_endpoint.rs, module header: "this emulator publishes one it answers on itself — the core's own loopback HTTPS listener ... with the MicroVM named in the path — rather than a hostname that resolves off this machine", MICROVM_PATH_SEGMENT = "microvms").
NOTE on what is NOT the requirement: the particular host, the ephemeral port and the MicroVM id in the recorded URL are environment values and must not be copied. The emulator must keep publishing an address it actually answers on; what must change is the URL's FORM, so that the value satisfies the shape the corpus records for this member.
Why: a client takes this member and appends a guest path to it (`<endpoint><path>`), stores it, logs it and hands it to another process. Against a URL whose authority is a literal loopback address and whose path already carries a resource segment, a client that parses the endpoint to extract the MicroVM's host, that re-signs or re-resolves it, or that joins a guest path with URL-join semantics (which replaces the last path segment) addresses something different from what it addresses against AWS — so code that works live breaks here, in the one member the API gives a client to reach the MicroVM at all.
Fix: the published-address construction in the MicroVM endpoint state (src/state/microvm_endpoint.rs), which RunMicrovm and GetMicrovm both project — not the two handlers, which agree with each other, and not the listener's transport.

=== Cause 2 of 4: MicroVM lifecycle timestamps are serialized with full sub-second precision instead of AWS's millisecond-precision epoch-seconds ===
Fixtures/steps: lambda-microvms-run-suspend-resume-terminate-microvm#task#7 ($.startedAt), #task#8/#task#10/#task#12 ($.startedAt) and #task#14 ($.startedAt and $.terminatedAt).
AWS: Smithy RunMicrovmResponse$startedAt (@required Timestamp, "The timestamp when the MicroVM first started") and $terminatedAt (Timestamp, "The timestamp when the MicroVM terminated"), same members on GetMicrovmResponse; restJson1 serializes a Timestamp as epoch-seconds. The instant itself is a per-run value and the corpus asserts this member as SHAPED, non-asserted — but the FORM is a requirement, and every timestamp this corpus records is epoch-seconds with exactly three fractional digits, i.e. millisecond precision (recorded elsewhere in the same corpus: 1781715610.862, 1790702978.629, 1793052797.597). Recorded behavior only for the precision rule; the model states no timestampFormat override.
Emulator: the shape check fails on startedAt at all five steps and additionally on terminatedAt at task#14 — replay: `$.startedAt ... got 1794887699.286565`, `$.terminatedAt ... got 1794887714.3481312`: six and seven fractional digits. The record's instants come from crate::state::now_datetime (src/state.rs:2334-2337, `DateTime::from(SystemTime::now())`), which keeps the system clock's full sub-second precision, and the output serializer writes it out unrounded.
Why: the EXTRA precision, not the instant, is what breaks a client. A caller that round-trips the value — keys a cache or an idempotency record on it, writes it into an ETag-style string, compares it to a timestamp it received from another API call or re-serializes it — cannot reproduce the value AWS would have produced, and an equality or ordering check against a millisecond-truncated copy of the same instant fails. A strict SDK or schema that models the member as millisecond epoch-seconds also rejects or truncates the response silently.
Fix: the timestamp the service records and serializes for these members — crate::state::now_datetime (src/state.rs:2334-2337) and/or the Timestamp serialization for the MicroVM output members — must emit epoch-seconds at the millisecond precision AWS emits, for every Timestamp member of this service so no two reads of one record disagree.

=== Cause 3 of 4: microvmId is minted as a zero-padded sequential counter, which does not satisfy the recorded shape of a MicroVM identifier ===
Fixture/step: lambda-microvms-run-suspend-resume-terminate-microvm#task#7 (RunMicrovm, 200/200).
AWS: Smithy RunMicrovmResponse$microvmId is @required, targets MicrovmIdentifier (string, @length 1..256, "The ARN or ID of the MicroVm"), documented "The unique identifier of the MicroVM". The id's text is a per-run value and the corpus records it as a SHAPED, non-asserted value, so the recorded id must not be copied — but the shape the recording fixes for a minted MicroVM id is a requirement, and the replay's shape check fails on the emulator's value. Recorded behavior only: AWS mints an opaque, non-sequential identifier under the 'mvm-' prefix.
Emulator: `$.microvmId: value_mismatch (expected '<shaped-not-asserted>', got 'mvm-0000000000000000')`. The id is a zero-padded hexadecimal store counter: ComAmazonawsLambdamicrovmsLambdamicrovmsState::next_microvm_id (src/state.rs:2262-2268) returns `format!("mvm-{seq:016x}")`, so the first MicroVM of every store is always 'mvm-0000000000000000'. The same cluster is why GetMicrovm at the later steps reports the same id (the control plane echoes the stored value), so the defect is in the minting rather than in a projection.
Why: a client that treats the id as opaque is fine, but one that validates the id's form before re-sending it in a URI path, that distinguishes two MicroVMs created in separate runs/stores (the counter restarts, so two different MicroVMs share an id across runs), or that relies on the ids not being guessable addresses another MicroVM's control-plane path by incrementing a counter. This is the same defect class the already-filed open issue 20261110T022430Z-lambda-microvms-getmicrovmimagebuild-does-not-echo-the-addressed.md found for buildId ('build-' + zero-padded counter); that issue holds no key for RunMicrovm and does not exercise this step.
Fix: the MicroVM id allocation in the service state (ComAmazonawsLambdamicrovmsLambdamicrovmsState::next_microvm_id, src/state.rs:2262-2268) must mint ids in the form the recording shows for a microvmId, while keeping the creation ordering ListMicrovms pages by (which must then come from the record's own ordering rather than from the id's text).

=== Cause 4 of 4: an unpinned baseImageVersion is recorded as the constant '1.0', so the create response and the version read report the wrong base-image pedigree ===
Fixtures/steps: lambdamicrovms-customer-image-lifecycle#setup#2 (CreateMicrovmImage, 201/201) and #task#6 (GetMicrovmImageVersion, 200/200).
AWS: Smithy CreateMicrovmImageResponse$baseImageVersion and GetMicrovmImageVersionOutput$baseImageVersion are optional, target Version (@length 1..2048, pattern ^[^\s]+$), documented "The specific version of the base MicroVM image to use" / "The specific version of the base MicroVM image" — the base-image version the image version was actually built from: the version the request pinned, and for a request that pinned none the version the service RESOLVES for the named baseImageArn. Recorded behavior for a create against the managed base image arn:aws:lambda:us-west-2:aws:microvm-image:al2023-1 with no baseImageVersion: both the 201 create response and the 200 single-version read report baseImageVersion '2.0' — that literal is this run's resolved value, not the requirement; the requirement is that the member reports the version resolved from the named base image (for the managed catalogue, the generation the catalogue currently publishes, in the label form the catalogue publishes it in) rather than a constant.
Emulator: both steps answer with baseImageVersion '1.0' (`$.baseImageVersion: value_mismatch (expected '2.0', got '1.0')`). CreateMicrovmImage resolves an omitted member through ComAmazonawsLambdamicrovmsLambdamicrovmsState::resolve_base_image_version (src/state/regions.rs:232-254, called at src/ops/create_microvm_image.rs:382-384), which deliberately does NOT consult the managed catalogue and returns FIRST_IMAGE_VERSION ("1.0", src/state.rs:336) for every managed base-image reference; the value is then stored on the version record and echoed by every later read, which is why GetMicrovmImageVersion reports it too. The emulator's own catalogue cannot produce the recorded label either: MANAGED_BASE_IMAGE_VERSIONS (src/state.rs:564-570) publishes plain integer generations "1"/"0", so the version labels the catalogue publishes are part of this cause.
Why: baseImageVersion is how a client pins and audits the base layer of an image. Every image this emulator creates claims base version 1.0 whatever base image it was built on, so a reproducible-build gate, a CVE re-base sweep or a promotion check that reads the create response (the only place the client sees the configuration the service actually accepted) or the version read is told its build pinned a base layer it did not, and never learns a re-base is due.
Fix: the base-image version resolution in the service state (resolve_base_image_version, src/state/regions.rs:232-254) must resolve an unpinned reference from the named base image — for an AWS-managed reference, from the catalogue it is published by — and the version record must store that resolved value, so the create response and every later version read report one pedigree. The catalogue's own version labels (MANAGED_BASE_IMAGE_VERSIONS) must be the labels AWS publishes, so the resolution has the recorded label to return.
Related, NOT covering this: open issue 20261108T125129Z-lambda-microvms-image-version-and-build-read-paths.md carries the SAME default on the LISTING row only (its Cause 1 is keyed on lambdamicrovms-customer-image-lifecycle#task#7, ListMicrovmImageVersions) and holds no key for CreateMicrovmImage#setup#2 or GetMicrovmImageVersion#task#6 — the replay reports both of those clusters with an empty owned_by. Issue 20261102T072953Z-lambda-microvms-image-version-records-are-written-from.md named the create step but is RESOLVED (status fixed), so it covers nothing: the divergence is live on this checkout.

## Evidence
Wire replay of trace set simparity-wire-aws-v1 (emulator_up true, "/__admin/health answered 200 and the :37645 data listener accepts"; 13 fixtures replayed, 104 steps, 92 passed, 12 mismatched, 0 errored, 0 blocked, score 88.5%, 10 divergence clusters, bootstrap_signal false). Expected = recorded AWS response, Actual = emulator. All five clusters below carry stub_path EMPTY and owned_by EMPTY.

Cause 1 + Cause 2 + Cause 3 — lambda-microvms-run-suspend-resume-terminate-microvm#task#7, RunMicrovm, status 200 vs 200:
  $.endpoint: value_mismatch (expected '<shaped-not-asserted>', got 'https://127.0.0.1:35025/microvms/mvm-0000000000000000')
  $.microvmId: value_mismatch (expected '<shaped-not-asserted>', got 'mvm-0000000000000000')
  $.startedAt: value_mismatch (expected '<shaped-not-asserted>', got 1794887699.286565)
  divergence_key: service=lambda-microvms|RunMicrovm|status=200->200|keys=$.endpoint,$.microvmId,$.startedAt

Cause 1 + Cause 2 — lambda-microvms-run-suspend-resume-terminate-microvm#task#8 (cluster also covers #task#10 and #task#12), GetMicrovm, status 200 vs 200:
  $.endpoint: value_mismatch (expected '<shaped-not-asserted>', got 'https://127.0.0.1:35025/microvms/mvm-0000000000000000')
  $.startedAt: value_mismatch (expected '<shaped-not-asserted>', got 1794887699.286565)
  divergence_key: service=lambda-microvms|GetMicrovm|status=200->200|keys=$.endpoint,$.startedAt (affected_count 3)

Cause 1 + Cause 2 — lambda-microvms-run-suspend-resume-terminate-microvm#task#14, GetMicrovm, status 200 vs 200:
  $.endpoint: value_mismatch (expected '<shaped-not-asserted>', got 'https://127.0.0.1:35025/microvms/mvm-0000000000000000')
  $.startedAt: value_mismatch (expected '<shaped-not-asserted>', got 1794887699.286565)
  $.terminatedAt: value_mismatch (expected '<shaped-not-asserted>', got 1794887714.3481312)
  divergence_key: service=lambda-microvms|GetMicrovm|status=200->200|keys=$.endpoint,$.startedAt,$.terminatedAt
  (The member is recorded as a shaped, non-asserted value, so the step fails the corpus's SHAPE assertion for the member, not an equality on a recorded instant/URL/id. For reference, the millisecond precision AWS serializes is visible in the same corpus's value-asserted timestamps: 1781715610.862, 1790702978.629, 1793052797.597 — three fractional digits — against the emulator's 1794887699.286565 / 1794887714.3481312.)

Cause 4 — lambdamicrovms-customer-image-lifecycle#setup#2, CreateMicrovmImage, status 201 vs 201:
  $.baseImageVersion: value_mismatch (expected '2.0', got '1.0')
  divergence_key: service=lambda-microvms|CreateMicrovmImage|status=201->201|keys=$.baseImageVersion

Cause 4 — lambdamicrovms-customer-image-lifecycle#task#6, GetMicrovmImageVersion, status 200 vs 200:
  $.baseImageVersion: value_mismatch (expected '2.0', got '1.0')
  divergence_key: service=lambda-microvms|GetMicrovmImageVersion|status=200->200|keys=$.baseImageVersion

Model grounding (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json): RunMicrovm POST /2025-09-09/microvms -> 200; RunMicrovmResponse$endpoint @required String @length 1..2048 ("The HTTPS endpoint URL for communicating with the MicroVM..."), $microvmId @required MicrovmIdentifier @length 1..256 ("The unique identifier of the MicroVM"), $startedAt @required Timestamp, $terminatedAt Timestamp; GetMicrovm GET /2025-09-09/microvms/{microvmIdentifier} -> 200 with the same members; CreateMicrovmImage POST /2025-09-09/microvm-images -> 201 with output baseImageVersion (optional Version, @length 1..2048, pattern ^[^\s]+$); GetMicrovmImageVersion GET /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion} -> 200 with the same member; protocol restJson1 (Timestamps as epoch-seconds, no timestampFormat override on these members).

Code sightings (orientation only): src/state/microvm_endpoint.rs:1-14 and :77-80 (the loopback published address and MICROVM_PATH_SEGMENT); src/state.rs:2334-2337 (now_datetime, full sub-second precision); src/state.rs:2262-2268 (next_microvm_id, `mvm-{seq:016x}`); src/state/regions.rs:232-254 (resolve_base_image_version, returns FIRST_IMAGE_VERSION for a managed reference and deliberately does not consult the catalogue); src/state.rs:336 (FIRST_IMAGE_VERSION = "1.0"); src/state.rs:564-570 (MANAGED_BASE_IMAGE_VERSIONS publishes generations "1"/"0"); src/ops/create_microvm_image.rs:382-384 (the unwrap_or_else into that resolution).

## Steps to Reproduce
Causes 1, 2 and 3 (MicroVM run/read path):
1. Boot the emulator from this checkout and create an image whose version settles successfully (CreateMicrovmImage with a code artifact that resolves in the emulator's S3), then RunMicrovm(imageIdentifier=<imageArn>).
2. Read the 200/201 body of RunMicrovm:
   - endpoint: AWS publishes an https URL in the form the corpus records (a resolvable host, no resource path); the emulator publishes 'https://127.0.0.1:<port>/microvms/<microvmId>'.
   - microvmId: AWS mints an opaque identifier; the emulator mints 'mvm-0000000000000000' for the first MicroVM of the store, incrementing by one per run (restart the emulator and the same id is minted again).
   - startedAt: AWS serializes epoch-seconds at millisecond precision (three fractional digits); the emulator serializes 1794887699.286565-style values with six or seven fractional digits. Assert with `repr()` / a regex on the fractional part, not by eye.
3. Call GetMicrovm(microvmIdentifier=<microvmId>) while PENDING/RUNNING/SUSPENDED and again after TerminateMicrovm: endpoint and startedAt diverge on every read, and terminatedAt diverges the same way as startedAt once set.
4. Re-replay fixture lambda-microvms-run-suspend-resume-terminate-microvm steps task#7, task#8, task#10, task#12 and task#14 to confirm: each must pass with $.endpoint, $.microvmId, $.startedAt and $.terminatedAt satisfying the corpus's shape for those members.

Cause 4 (unpinned base-image version):
5. base = ListManagedMicrovmImages()['items'][0]['imageArn']  (the managed al2023-1 base image).
6. CreateMicrovmImage(name=<unique>, baseImageArn=base, buildRoleArn=<an IAM role arn>, codeArtifact={'uri': 's3://<unique>/code.zip'}) with NO baseImageVersion.
   AWS: the 201 reports baseImageVersion as the version resolved for `base` (recorded: '2.0').
   Emulator: '1.0' for every create, whatever base image was named.
7. GetMicrovmImageVersion(imageIdentifier=<imageArn>, imageVersion='1.0'): the same wrong value is echoed from the stored version record.
8. Re-replay fixture lambdamicrovms-customer-image-lifecycle steps setup#2 and task#6 to confirm the fix.

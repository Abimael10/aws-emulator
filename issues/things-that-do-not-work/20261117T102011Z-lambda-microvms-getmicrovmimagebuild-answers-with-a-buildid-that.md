Issue-Source: trace-sweep-divergence
Issue-Trace: lambdamicrovms-customer-image-lifecycle
Issue-Step: task#9
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambdamicrovms-customer-image-lifecycle
Issue-Shape-Id: com.amazonaws.lambdamicrovms#LambdaMicrovms
Issue-Key: lambda-microvms|GetMicrovmImageBuild|2xx->2xx|$.buildId
Issue-Commit: 39d351fb1e8d061b1c0616e3b1effc9c5ba40bfd

# lambda-microvms GetMicrovmImageBuild answers with a buildId that does not match the recorded identifier of the build the request addressed

> _Note: this issue was identified against the code base at commit `39d351fb1e8d061b1c0616e3b1effc9c5ba40bfd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMicrovmImageBuild

## Description
One root cause, observed on replay of wire corpus simparity-wire-aws-v1 against the emulator (emulator up, /__admin/health 200; 104 steps, 92 passed, 12 mismatched, score 88.5%). A fixer must close Cause 1 of 1 before submitting.

=== Cause 1 of 1: the single-build read path reports a build identifier that is not byte-for-byte the identifier recorded for the addressed build ===
Fixture lambdamicrovms-customer-image-lifecycle, step task#9 (GetMicrovmImageBuild, HTTP 200 on both sides; the only diverging key is $.buildId).

AWS: Smithy GetMicrovmImageBuild is GET /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}/builds/{buildId}, 200, and GetMicrovmImageBuildOutput$buildId is a REQUIRED NonBlankString (@length 1..2048, pattern ^[^\s]+$) documented 'The build request ID.' The read is @readonly and addresses exactly one build by its buildId path label, so the identifier the response reports is the identifier of the build that was addressed, in the same form the build-creating path and the build listing published it. Nothing else about the operation is at issue here: every other member of the 200 agrees with the recording.

Emulator: the 200 carries a buildId the replay comparator rejects against the recorded one for the same addressed build. Both values RENDER identically in the replay detail — expected 'build-0000000000000000', got 'build-0000000000000000' — so the divergence is not in the semantics of which build was returned but in the exact byte form of the identifier the emulator mints and echoes (padding width / digit encoding of the generated id). The emulator mints build ids from one global sequence formatted as 'build-{seq:016x}' (crate::state::ComAmazonawsLambdamicrovmsLambdamicrovmsState::next_build_id) and the single-build read echoes the stored field verbatim (build.build_id), so whatever form that minting chooses is the form every build-addressed read, listing and path label agrees on — and it is not the recorded one. A fixer MUST diff the two values at the byte level (length and character-by-character) rather than by eye: the rendered strings are visually indistinguishable, which is itself the signature of a form divergence rather than a wrong-record divergence.

Why: buildId is the only handle a client has on one build of one image version — it is the path label for this read, the correlation key between a build row in ListMicrovmImageBuilds and the build read back, and the value a pipeline stores to re-poll a build later. A client that reads a build back and asserts the response's buildId equals the id it asked for, or that joins a stored build id to a listing row, gets a non-equal comparison against the emulator even though the same build was served: the assertion fails, the join finds nothing, and the build-tracking branch of the client is unreachable in emulation while it works against AWS.

Fix: the build identifier minted into the build record in the service state, and the projection of that field on the single-build read, must agree with the recorded identifier form (crate::state::next_build_id, which formats the id, and the GetMicrovmImageBuildOutput.build_id projection in the operation's handler); the same minting feeds ListMicrovmImageBuilds' rows and the buildId path label, so the identifier form must be changed in ONE place and remain identical across the build-creating path, the listing and this read. Do not special-case the read to rewrite the id it was asked for: that would hide the minting form from the listing path.

Related open issue, which does NOT carry this replay key (the replay reports this cluster with an EMPTY owned_by and an EMPTY stub_path): '20261108T125129Z-lambda-microvms-image-version-and-build-read-paths.md'. Its Cause 2 is about the build ROW's missing recorded member terminationReasonCode in ListMicrovmImageBuilds and remarks in passing that GetMicrovmImageBuild projects the same member set; it is keyed on lambdamicrovms-customer-image-lifecycle#task#8 and asserts nothing about buildId, which that fixture step records as a non-asserted shaped value. Closing it does not exercise task#9 and would not surface this identifier-form divergence.

Traces: lambdamicrovms-customer-image-lifecycle#task#9 (GetMicrovmImageBuild).

## Evidence
Wire replay of trace set simparity-wire-aws-v1 (trace_set_id simparity-wire-aws-v1, emulator_up true, '/__admin/health answered 200 and the :41547 data listener accepts'; steps_total 104, steps_passed 92, steps_mismatched 12, divergence_count 10, score_pct 88.5). Expected = recorded AWS response, Actual = emulator response.

Cause 1 — lambdamicrovms-customer-image-lifecycle#task#9, GetMicrovmImageBuild, status 200 vs 200, verdict mismatch, stub_path EMPTY, owned_by EMPTY:
  detail: "$.buildId: value_mismatch (expected 'build-0000000000000000', got 'build-0000000000000000')"
  divergence_key: service=lambda-microvms|GetMicrovmImageBuild|status=200->200|keys=$.buildId
  affected: ['lambdamicrovms-customer-image-lifecycle#task#9'] (affected_count 1)
  NOTE for the fixer: the two values are rendered identically by the replay detail, so the difference is below the rendering — compare the raw bytes (length, digit count, encoding) of the recorded identifier and of the one the emulator mints. No other member of this 200 diverges.

Model grounding (services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/smithy-model/com-amazonaws-lambdamicrovms-lambdamicrovms.json): GetMicrovmImageBuild — http GET /2025-09-09/microvm-images/{imageIdentifier}/versions/{imageVersion}/builds/{buildId}, code 200, @readonly; input buildId/imageVersion @httpLabel required NonBlankString (@length 1..2048, pattern ^[^\s]+$), imageIdentifier @httpLabel required MicrovmImageIdentifier (@length 1..256); output buildId REQUIRED NonBlankString, doc 'The build request ID.'; errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400.

Code sightings (orientation only, commit of this checkout): services/service-com-amazonaws-lambdamicrovms-lambdamicrovms/src/state.rs:2469-2476 (next_build_id: format!("build-{seq:016x}") off the single global next_build_seq) and src/state.rs:2498-2510 (the per-target fan-out that inserts each build under that id); src/ops/get_microvm_image_build.rs:223-245 (the addressed-build lookup and the build_id projection into GetMicrovmImageBuildOutput).

## Steps to Reproduce
1. mvm = client('lambda-microvms'); base = ListManagedMicrovmImages()['items'][0]['imageArn'].
2. CreateMicrovmImage(name=<unique>, baseImageArn=base, buildRoleArn=<an IAM role arn>, codeArtifact={'uri': 's3://<unique>-absent/code.zip'}) and let the asynchronous build settle.
3. builds = ListMicrovmImageBuilds(imageIdentifier=<imageArn>, imageVersion='1.0')['items']; bid = builds[0]['buildId'].
4. got = GetMicrovmImageBuild(imageIdentifier=<imageArn>, imageVersion='1.0', buildId=bid).
   AWS: got['buildId'] is the identifier of the addressed build, byte-for-byte the identifier the listing row and the request path carried.
   Emulator: the 200 is served for the right build, but $.buildId does not compare equal to the recorded identifier for that build — the replay reports value_mismatch on $.buildId alone (both sides render as 'build-0000000000000000'). Compare the raw bytes/length of got['buildId'] against the recorded value rather than by eye.

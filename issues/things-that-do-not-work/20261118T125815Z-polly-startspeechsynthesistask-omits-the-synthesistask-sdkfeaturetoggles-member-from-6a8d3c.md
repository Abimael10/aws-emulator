Issue-Source: trace-sweep-divergence
Issue-Trace: polly-speech-synthesis-task-lifecycle
Issue-Step: task#2
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: polly-speech-synthesis-task-lifecycle
Issue-Shape-Id: com.amazonaws.polly#Parrot_v1
Issue-Key: polly|StartSpeechSynthesisTask|2xx->2xx|$.SynthesisTask.OutputUri
Issue-Related: 20261103T042343Z-polly-startspeechsynthesistask-omits-the-synthesistask-sdkfeaturetoggles-member-from.md
Issue-Commit: 52c6ce8007799ff86ff53305c58867703d8518fa

# Polly StartSpeechSynthesisTask omits the SynthesisTask SdkFeatureToggles member from its response body

> _Note: this issue was identified against the code base at commit `52c6ce8007799ff86ff53305c58867703d8518fa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartSpeechSynthesisTask

## Description
StartSpeechSynthesisTask answers 200 with a SynthesisTask whose members all agree with the recorded task record, but the serialized object is missing one member the recorded AWS response carries, so the response body is not shape-complete.

Cause 1 of 1 (fixture polly-speech-synthesis-task-lifecycle, step task#2): the response's $.SynthesisTask has no SdkFeatureToggles member.
AWS: recorded behavior only. The service's Smithy model (services/service-com-amazonaws-polly-parrot-v1/smithy-model/com-amazonaws-polly-parrot-v1.json) binds StartSpeechSynthesisTask as POST /v1/synthesisTasks -> 200 with StartSpeechSynthesisTaskOutput { SynthesisTask }, and com.amazonaws.polly#SynthesisTask declares Engine, TaskId, TaskStatus, TaskStatusReason, OutputUri, CreationTime, RequestCharacters, SnsTopicArn, LexiconNames, OutputFormat, SampleRate, SpeechMarkTypes, TextType, VoiceId and LanguageCode — smithy_query(query="shape", shape="SdkFeatureToggles") returns 'shape not found', so the member is NOT modeled. The recording is therefore the only authority for it: the recorded 200 body for this operation serializes SynthesisTask with an additional SdkFeatureToggles member, present and set to an empty list for a request that enables no toggles. Presence of the member (as a list, empty when nothing is toggled) is the requirement; no particular element is, since the recording's value carries none.
Emulator: for the same request the member is absent from the serialized object entirely (replay: "$.SynthesisTask.SdkFeatureToggles: missing (expected [], got None)"). Status (200), framing and every other SynthesisTask member match, and the task-lifecycle reads that follow in the same fixture pass.
Why: a client that round-trips the response body — a strict schema/contract check, a JSON-diff fixture assertion, a hand-rolled or non-SDK parser that requires the field, or any consumer that reads SynthesisTask.SdkFeatureToggles to learn which toggles the service applied — sees a missing key rather than an empty list, so it either fails validation or must special-case a null that real AWS never returns. Because the member is not in the Smithy model, no generated SDK shape covers it, which is exactly why a strictly model-driven serializer drops it.
Fix: the response serialization of this operation — the SynthesisTask projection shared by the asynchronous task paths (crate::state::synthesis_task, called at services/service-com-amazonaws-polly-parrot-v1/src/ops/start_speech_synthesis_task.rs:503 and returned at :522-524) and the serializer that writes StartSpeechSynthesisTaskOutput. The emitted object must include SdkFeatureToggles as a list, empty when the request enables no toggles, alongside the modeled members.
Traces: polly-speech-synthesis-task-lifecycle#task#2.

Not filed from this cluster: the same step also reports $.SynthesisTask.OutputUri as differing (recorded 'https://s3.us-west-2.amazonaws.com/<bucket>/<task-id>.mp3' vs emulator 'https://127.0.0.1:<port>/<bucket>/<task-id>.mp3'). Both are the same path-style <scheme>://<host>/<bucket>/<key> address over the same bucket name, and they differ only in the host:port the object storage is reached on and in the generated task id that forms the key — per-environment values the emulator deliberately announces (crate::state::announced_output_uri, cited at start_speech_synthesis_task.rs:460-469), not a behavioral requirement.

Related open issue: 20261030T071447Z-polly-synthesizespeech-output-stream-diverges-from-the-rendered.md owns the SynthesizeSpeech payload/speech-mark divergences of this corpus; it concerns the synchronous audio path and does not cover this response-shape gap.

## Evidence
Fixture polly-speech-synthesis-task-lifecycle, step task#2, operation StartSpeechSynthesisTask (service polly): expected HTTP 200, actual HTTP 200 — verdict mismatch, divergence key 'service=polly|StartSpeechSynthesisTask|status=200->200|keys=$.SynthesisTask.OutputUri,$.SynthesisTask.SdkFeatureToggles'.
Filed cause: '$.SynthesisTask.SdkFeatureToggles: missing (expected [], got None)' — the recorded body carries the member with an empty-list value; the emulator's body has no such key.
Model facts used: smithy_query(query="operation", operation="StartSpeechSynthesisTask") -> POST /v1/synthesisTasks, 200, output StartSpeechSynthesisTaskOutput{SynthesisTask}; smithy_query(query="shape", shape="SynthesisTask") -> 15 members, none named SdkFeatureToggles; smithy_query(query="shape", shape="SdkFeatureToggles") -> shape not found. No modeled error applies: the request is accepted and answered 200, only the serialized shape is short a member.
Excluded as per-environment: '$.SynthesisTask.OutputUri: resource_not_identified' — differs only in host:port and in the generated task id inside the key, with the bucket name and the path-style URL form identical.

## Steps to Reproduce
1. Boot the emulator from this checkout (the polly service answers; score for this corpus is 91.8%).
2. Replay fixture polly-speech-synthesis-task-lifecycle step task#2: StartSpeechSynthesisTask with the fixture's Text/VoiceId/OutputFormat and OutputS3BucketName.
3. Inspect the 200 response body's SynthesisTask object: the emulator emits only the modeled members, while the recorded body also carries 'SdkFeatureToggles': [].
4. Expected: SynthesisTask includes SdkFeatureToggles as a list (empty when the request enables no toggles). Re-replay polly-speech-synthesis-task-lifecycle#task#2 to confirm the fix; ignore the OutputUri host/port and task-id differences, which are per-environment.

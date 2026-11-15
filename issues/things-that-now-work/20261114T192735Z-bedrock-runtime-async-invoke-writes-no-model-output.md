Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-runtime.async-invoke-output-written-to-configured-destination-serverside bedrock-runtime.async-invoke-output-retention-window bedrock-runtime.converse-stop-sequence-halts-generation
Issue-Shape-Id: com.amazonaws.bedrockruntime#AmazonBedrockFrontendService
Issue-Key: bedrock-runtime.async-invoke-output-written-to-configured-destination-serverside|run#18|AssertionError|assert any(k.endswith("output.mp4") for k in keys), keys
Issue-Key: bedrock-runtime.async-invoke-output-retention-window|run#10|Timeout|time.sleep(2)
Issue-Key: bedrock-runtime.converse-stop-sequence-halts-generation|run#6|AssertionError|assert text.strip().endswith("7"), f"expected generation to end right at the stop sequence, got: {text!r}"
Issue-Commit: 470e644ed8a1d9f9695329376c11ab8f2895bf9a

# bedrock-runtime async invoke writes no model output artifact to S3 and never stays resolvable over the observation window; Converse ignores inferenceConfig.stopSequences and returns a constant emulator notice (3 causes)

> _Note: this issue was identified against the code base at commit `470e644ed8a1d9f9695329376c11ab8f2895bf9a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Resolution

Cause 1 is FIXED. A finished video invocation now delivers the GENERATED MEDIA object of the requested
modality — `output.mp4` — under its own segment of the submitted prefix, beside the manifest it already
wrote, both written before the status flips to `Completed`
(`services/service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice/src/state/async_invoke_video.rs`,
`src/state/async_invoke_output.rs`). The object is a real video file: its frames are rendered here and
encoded by the maintained H.264 encoder and MP4 writer the crate now depends on, at the geometry and the
length the submitted `modelInput` configured (`videoGenerationConfig.dimension` / `durationSeconds`, or
`resolution` / `aspect_ratio` / `duration`), so a client that polls to `Completed` and reads its own bucket
has a decodable artifact. No video model runs in this emulator, so what the frames SAY is exactly that,
beside the facts of the invocation served — nothing renders the scene the prompt describes and no pixel is
derived from the prompt. The manifest additionally names the submitting call (`requestId`). The check's
replay key passes.

Cause 2's behaviour is FIXED and pinned, and its replay key still times out for a reason outside the
emulator. A `Completed` invocation now stays resolvable with an unchanged
`outputDataConfig.s3OutputDataConfig.s3Uri`, each `GetAsyncInvoke` answers in milliseconds without
blocking, and the artifact stays present and byte-identical however often it is re-read
(`tests/all/async_invoke_output_artifact.rs`). The cause's own hypotheses — a record that never leaves
`InProgress`, or a `Get`/`List` that does not answer — are both false now: the sibling check
`bedrock-runtime.async-invoke-output-written-to-configured-destination-serverside` runs the same
start -> poll-to-terminal -> list sequence and PASSES. What is left is the check's own shape: it observes
the retention window for ~60 s in `time.sleep(2)` steps, which is the harness's whole 60 s check budget,
so it is killed in a sleep whatever the emulator answers.

Cause 3 is NOT fixed and is NOT closed here: the stop-sequence rule itself is already implemented and
covered (`src/state.rs` `halt_at_stop_sequence`, pinned by `tests/all/inference_stop_sequences.rs`, whose
first row is this check's own digit and budget — the served text is cut at the first configured sequence
and the turn reports `stopReason='stop_sequence'`). What the check needs beyond that is a generation
containing the digits to cut, which is the constant-notice defect: nothing in this emulator can run a
language model. Its replay key now lives on the open report that owns that defect,
`issues/things-that-do-not-work/20261028T194707Z-bedrock-runtime-generation-stage-serves-a-constant-emulator.md`
(which this report's own Cause 3 named as the one describing it).

## Operation
StartAsyncInvoke

## Description
Three net-new root causes in the bedrock-runtime data plane, all filed together because they belong to one service. Each is numbered and carries its own failing check ids; all three must be closed before this issue is submitted.

Cause 1 of 3 — a Completed StartAsyncInvoke video job leaves no generated artifact under the configured S3 prefix (check: bedrock-runtime.async-invoke-output-written-to-configured-destination-serverside).
AWS: Smithy (bedrock-runtime, AmazonBedrockFrontendService): StartAsyncInvoke POST /async-invoke, input members modelId (required, @pattern ^[a-zA-Z_\.\-/0-9:]+$), modelInput (required, document) and outputDataConfig (required, AsyncInvokeOutputDataConfig -> s3OutputDataConfig.s3Uri); output invocationArn (required, @pattern ...:async-invoke/[a-z0-9]{12}$). GetAsyncInvoke GET /async-invoke/{invocationArn} returns status (AsyncInvokeStatus enum) and outputDataConfig, both required. The model does not name the objects written, so for the artifact layout this is recorded behavior only: once a video-capable async model job reaches status Completed, the SERVICE itself (the client uploads nothing) has written BOTH the generated media object and a non-empty manifest.json under <s3Uri>/<invocationId>/, where <invocationId> is the last ARN path segment minted by StartAsyncInvoke. Names and sizes are per-run values; the requirement is that the generated media object for the requested modality exists and is non-empty alongside the manifest.
Emulator: after the job reports Completed, listing the prefix <s3Uri>/<invocationId>/ returns the manifest object ONLY — e.g. ['video-out/<invocationId>/manifest.json'] — and the assertion that some key ends with 'output.mp4' fails. No media object of any name is present under that prefix.
Why: a client that polls GetAsyncInvoke to Completed and then reads the generated media from the location it configured finds nothing to read. Completed is the contract that the output is available, so the client either errors on a missing key or silently processes an empty result set; pipelines that hand the artifact to a downstream consumer break with no error anywhere in the AWS calls.
Fix: the StartAsyncInvoke completion path in the bedrock-runtime service — the async-invoke worker/state transition that marks the record Completed must write the model's output artifact to the resolved s3Uri prefix (together with the manifest it already writes) before, or atomically with, flipping status to Completed.
Traces: bedrock-runtime.async-invoke-output-written-to-configured-destination-serverside.

Cause 2 of 3 — the async-invoke record and its artifact are not re-readable across the post-completion observation window; the check never returns (check: bedrock-runtime.async-invoke-output-retention-window).
AWS: Smithy: GetAsyncInvoke is @readonly and returns status, outputDataConfig (both required) for an existing invocationArn; it is a cheap point read. Retention is undocumented, so recorded behavior only: a Completed invoke stays resolvable with an UNCHANGED outputDataConfig.s3OutputDataConfig.s3Uri, and its S3 artifact stays present, for at least the ~60s the recording observes; repeated GetAsyncInvoke + ListObjectsV2 calls over that window each answer promptly.
Emulator: the check does not reach any assertion — it is killed with 'Check timed out after 60.0s' with no AWS error returned, i.e. the sequence start -> poll to terminal -> re-read never completes within the harness budget. Either the record never leaves InProgress so the poll loop never terminates, or a Get/List in the re-read loop does not answer. Hypothesis: the same completion-transition defect as Cause 1 leaves the job non-terminal; the open issue 20261024T094806Z-bedrock-runtime-async-invoke-startasyncinvoke-over-validates-modelinput.md describes jobs never reaching Completed but does not carry this check's replay key, so it does not cover it.
Why: every client of async invoke is a poll loop. A job that never reaches a terminal status, or a Get that stalls, makes the API unusable: callers hang until their own timeout and can never collect an output, and code that asserts the output location is stable after completion cannot be exercised at all.
Fix: the async-invoke state machine and GetAsyncInvoke handler in the bedrock-runtime service — the record must reach a terminal AsyncInvokeStatus and then remain readable with an unchanged s3Uri, and each GetAsyncInvoke must answer without blocking.
Traces: bedrock-runtime.async-invoke-output-retention-window.

Cause 3 of 3 — Converse ignores inferenceConfig.stopSequences and serves a constant emulator-authored notice instead of generated text (check: bedrock-runtime.converse-stop-sequence-halts-generation).
AWS: Smithy: Converse input inferenceConfig (InferenceConfiguration) carries stopSequences, maxTokens and temperature; output.message.content carries the generated ContentBlocks and stopReason reports why the turn ended. Recorded behavior: a Nova Converse turn asked to emit the integers 1..100 with stopSequences ['7'] comes back with text that is the generation TRUNCATED at the first occurrence of the stop sequence — it contains the pre-stop text, ends at the stop sequence, and never reaches '100'. The stop sequence is a value from the request; the requirement is that generation is cut at it, not any one recording's digits.
Emulator: output.message.content holds a fixed emulator notice that is not generation at all and is unaffected by stopSequences: 'No language model runs in this emulator. amazon.nova-lite-v1:0 generated no text for this request. The request carried 1 message(s) and 26 input token(s).' (the model id, message count and token count are the only parts that vary). It does not end at the stop sequence, so the check fails. The open issues 20261028T194707Z-bedrock-runtime-generation-stage-serves-a-constant-emulator.md (constant notice string) and 20261111T032149Z-bedrock-runtime-converse-reports-stopreason-stop-sequence-where.md (stopReason for a halted turn) describe adjacent symptoms but neither carries this check's replay key.
Why: a client that configures a stop sequence to bound or delimit a model's output receives text that contains neither its delimiter nor any prompt-responsive content, so parsers keyed on the stop sequence find no boundary and any assertion on the generated text fails against the emulator while passing against AWS.
Fix: the Converse generation stage in the bedrock-runtime service — the component that produces output.message.content must honour inferenceConfig.stopSequences by truncating the emitted text at the first stop sequence instead of emitting a fixed notice.
Traces: bedrock-runtime.converse-stop-sequence-halts-generation.

## Evidence
Cause 1 — expected: keys under s3://<bucket>/video-out/<invocationId>/ include a non-empty generated media object (output.mp4) AND manifest.json after GetAsyncInvoke reports status='Completed'. Actual: AssertionError on `any(k.endswith("output.mp4") for k in keys)` with keys == ['video-out/190d0258f537/manifest.json'] (the invocation id is a per-run value); only the manifest was written.

Cause 2 — expected: after the job reaches Completed, repeated GetAsyncInvoke calls keep answering status='Completed' with an unchanged outputDataConfig.s3OutputDataConfig.s3Uri and ListObjectsV2 keeps showing a non-empty object under that prefix, for the ~60s window. Actual: 'Check timed out after 60.0s' — no assertion reached, no AWS error returned; the start -> poll-to-terminal -> re-read sequence never completes.

Cause 3 — expected: Converse(modelId=amazon.nova-lite-v1:0, inferenceConfig={'maxTokens':400,'temperature':0,'stopSequences':['7']}) returns text that contains the pre-stop generation, ends at the stop sequence '7', and does not contain '100'. Actual: AssertionError 'expected generation to end right at the stop sequence, got: \'No language model runs in this emulator. amazon.nova-lite-v1:0 generated no text for this request. The request carried 1 message(s) and 26 input token(s).\'' — a constant emulator notice; the fixed text is the 'No language model runs in this emulator. ... generated no text for this request. The request carried N message(s) and M input token(s).' sentence pair, with only the model id, message count and token count varying.

## Steps to Reproduce
Cause 1: create an S3 bucket in the emulator's region; call StartAsyncInvoke(modelId='luma.ray-v2:0', modelInput={'prompt':...,'aspect_ratio':'16:9','duration':'5s','resolution':'540p'}, outputDataConfig={'s3OutputDataConfig':{'s3Uri':'s3://<bucket>/video-out'}}); poll GetAsyncInvoke(invocationArn) until status != 'InProgress'; with status == 'Completed', ListObjectsV2(Bucket=<bucket>, Prefix='video-out/<last ARN path segment>/') and observe that no generated media object is present, only manifest.json.

Cause 2: same StartAsyncInvoke as above; poll GetAsyncInvoke to a terminal status, then for ~60s re-call GetAsyncInvoke every 2s and ListObjectsV2 on the output prefix, asserting status stays 'Completed', s3Uri is unchanged and a non-empty object remains. Observe the sequence never completes inside the 60s check budget.

Cause 3: call Converse(modelId='amazon.nova-lite-v1:0', messages=[{'role':'user','content':[{'text':'Output exactly the following, separated by single spaces and nothing else, no punctuation, no preamble: count from 1 to 100'}]}], inferenceConfig={'maxTokens':400,'temperature':0,'stopSequences':['7']}) and inspect output.message.content: the text is the constant emulator notice rather than generation truncated at '7'.

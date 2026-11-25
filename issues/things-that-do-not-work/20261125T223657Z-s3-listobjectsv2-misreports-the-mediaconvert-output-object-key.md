Issue-Source: trace-sweep-divergence
Issue-Trace: mediaconvert-job-transcodes-to-complete-with-output
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: mediaconvert-job-transcodes-to-complete-with-output
Issue-Shape-Id: com.amazonaws.mediaconvert#MediaConvert
Issue-Key: s3|ListObjectsV2|2xx->2xx|$.ListBucketResult.Contents.Key
Issue-Related: 20261117T042544Z-s3-listobjectsv2-reports-checksumalgorithm-crc32-for-a-firehose.md
Issue-Commit: 8d1b1b750e0b1340c298c8ebe49c21e166bf4879

# s3 ListObjectsV2 misreports the MediaConvert output object: key loses the input basename and ChecksumAlgorithm is wrong

> _Note: this issue was identified against the code base at commit `8d1b1b750e0b1340c298c8ebe49c21e166bf4879`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListObjectsV2

## Description
Two causes, both in the same ListObjectsV2 response that enumerates the object a completed MediaConvert job wrote to its output destination. Both are body-member mismatches on a 200/200 step: the listing is served, but the object it describes is described wrongly.

Cause 1 of 2 — the listed object key drops the derived base name, so the output object is named `output/-out.mp4` instead of `output/<input-basename>-out.mp4`.

AWS: Smithy (services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json) models ListObjectsV2 output member Contents as a list of com.amazonaws.s3#Object, whose Key member targets ObjectKey with @length min 1 and is documented "The name that you assign to an object. You use the object key to retrieve the object." The key is therefore whatever the writer actually stored under; S3 reports it back verbatim. The writer here is the emulator's own MediaConvert job output, and for the naming rule the model says nothing: recorded behavior only. The recording shows the completed job's output object listed under the destination prefix as `output/black_video-out.mp4` — i.e. the file name is the input file's base name (`black_video`, taken from the job's Input.FileInput S3 key) with the output group's NameModifier (`-out`) appended and the container extension (`.mp4`) applied. The base name is a value derived from the request (the job's input key); only the modifier and extension come from the job's output settings.

Emulator: the same ListObjectsV2 answers 200 but reports `$.ListBucketResult.Contents.Key` as `output/-out.mp4`. The base-name component is empty: the emulator appends the NameModifier and extension but never substitutes the input file's base name, so every output of every job collapses to `<prefix>/<NameModifier><ext>`.

Why: a client that submits a MediaConvert job and then addresses the result by the key AWS would produce — recomputing it from its own input file name plus the NameModifier it set, which is the documented, deterministic way to locate a transcode output — issues GetObject/HeadObject on `output/<basename>-out.mp4` and gets 404 NoSuchKey against the emulator, because the only object present is `output/-out.mp4`. Worse, two jobs with different inputs but the same output group settings produce the SAME key, so one job's output silently overwrites another's instead of landing side by side.

Fix: the MediaConvert job's output writer — the component that composes the destination key (Destination + base name + NameModifier + container extension) before it puts the object into S3 state. The S3 listing itself is faithful to what was stored; the stored key is what is wrong, so the naming must derive the base name from the job's input key. ListObjectsV2 must keep echoing the stored key verbatim.

Cause 2 of 2 — the listed object's ChecksumAlgorithm is CRC64NVME where the recording reports SHA256.

AWS: Smithy models Object.ChecksumAlgorithm as ChecksumAlgorithmList (@xmlFlattened, member enum ChecksumAlgorithm with values CRC32, CRC32C, SHA1, SHA256, CRC64NVME, SHA512, MD5, XXHASH64, XXHASH3, XXHASH128), documented "The algorithm that was used to create a checksum of the object." So the member is not a free default: it must report the algorithm under which the object was actually checksummed when it was written. Which algorithm the writer chooses for a MediaConvert transcode output is recorded behavior only — the recording shows the object listed with ChecksumAlgorithm SHA256.

Emulator: the same listing reports `$.ListBucketResult.Contents.ChecksumAlgorithm` as `CRC64NVME` — the emulator's blanket default for objects written without an explicit x-amz-sdk-checksum-algorithm — rather than the algorithm the writing path used. Hypothesis: the internal put that materialises the job output passes no checksum algorithm, so the S3 state applies its default instead of the SHA256 the service-side writer uses.

Why: a client that reads Contents.ChecksumAlgorithm to decide which checksum header to request (x-amz-checksum-sha256 vs x-amz-checksum-crc64nvme) on a follow-up GetObject/HeadObject, or that validates a transferred object against the algorithm the listing advertises, asks for a checksum the object does not carry and either fails integrity validation or sees the header absent. Any integrity gate downstream of a transcode is driven off this member, so it is wrong in a way the client cannot detect except by the mismatch.

Fix: the writer that materialises the MediaConvert output object into S3 state must record the checksum algorithm it computed (SHA256 for this path) alongside the object, and the ListObjectsV2 serializer must project that stored algorithm into the flattened Contents.ChecksumAlgorithm element instead of falling back to the state's default. Both halves live in the object-write path and the ListObjectsV2 projection, not in the listing's XML shape, which is otherwise correct.

Neither cause is owned by an open issue: the replay cluster reports owned_by empty and stub_path empty. The only open issue on this same fixture (20261121T060913Z-mediaconvert-getjob-omits-the-job-warnings-list-for.md, mediaconvert GetJob omits job.warnings at step task#6) covers a different step, a different service and a different member, and does not cover this ListObjectsV2 step.

Traces: mediaconvert-job-transcodes-to-complete-with-output#task#7 (s3 ListObjectsV2, affected_count 1) carries both causes.

## Evidence
Fixture mediaconvert-job-transcodes-to-complete-with-output, step task#7, service s3, operation ListObjectsV2, verdict mismatch, expected_status 200 / actual_status 200.

Diverging body members (replay detail, verbatim):
  $.ListBucketResult.Contents.Key: value_mismatch (expected 'output/black_video-out.mp4', got 'output/-out.mp4')
  $.ListBucketResult.Contents.ChecksumAlgorithm: value_mismatch (expected 'SHA256', got 'CRC64NVME')

divergence_key: service=s3|ListObjectsV2|status=200->200|keys=$.ListBucketResult.Contents.ChecksumAlgorithm,$.ListBucketResult.Contents.Key

Expected (Cause 1): Contents.Key is the output object's stored key, whose file-name component is the job input's base name + the output group's NameModifier + the container extension. In the recorded run that renders as 'output/black_video-out.mp4' — 'black_video' is a value taken from the job's Input.FileInput key, '-out' is the NameModifier from the job's output settings, '.mp4' the container; the prefix 'output/' comes from the output group Destination. The requirement is the composition rule, not those literal values.
Actual (Cause 1): 'output/-out.mp4' — the base-name component is empty, so the key is the modifier and extension alone.

Expected (Cause 2): Contents.ChecksumAlgorithm reports the algorithm the object was actually checksummed with when written; the recording reports 'SHA256' for a MediaConvert transcode output.
Actual (Cause 2): 'CRC64NVME', the emulator's default-on-write algorithm.

Model citations: com.amazonaws.s3#Object.Key -> ObjectKey, @length min 1, "The name that you assign to an object. You use the object key to retrieve the object."; com.amazonaws.s3#Object.ChecksumAlgorithm -> ChecksumAlgorithmList (@xmlFlattened), member enum com.amazonaws.s3#ChecksumAlgorithm with values CRC32, CRC32C, SHA1, SHA256, CRC64NVME, SHA512, MD5, XXHASH64, XXHASH3, XXHASH128, "The algorithm that was used to create a checksum of the object." Output-key naming and the chosen checksum algorithm for a MediaConvert output: recorded behavior only.

## Steps to Reproduce
1. Replay the wire corpus: replay_wire_corpus() against the emulator built from this checkout.
2. Fixture mediaconvert-job-transcodes-to-complete-with-output: the fixture creates a MediaConvert job whose input is an S3 .mp4 object and whose output group writes to an 'output/' destination prefix with NameModifier '-out', lets the job reach status COMPLETE, then (step task#7) calls S3 ListObjectsV2 on the destination bucket/prefix to enumerate what the job wrote.
3. Read the 200 ListBucketResult body. Contents.Key is 'output/-out.mp4' (the input file's base name is missing from the composed key) where the recording has 'output/<input-basename>-out.mp4'.
4. In the same Contents entry, ChecksumAlgorithm is 'CRC64NVME' where the recording has 'SHA256'.
5. Minimal repro: CreateJob with Input.FileInput s3://<bucket>/<name>.mp4 and an output group Destination s3://<bucket>/output/ with NameModifier '-out'; poll GetJob to COMPLETE; then ListObjectsV2 with Prefix 'output/'. The listed key omits <name>, and a GetObject on 'output/<name>-out.mp4' answers 404 NoSuchKey; the listed ChecksumAlgorithm is CRC64NVME rather than SHA256.

Issue-Source: trace-sweep-divergence
Issue-Trace: entityresolution-rule-based-matching-job-writes-matches
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: entityresolution-rule-based-matching-job-writes-matches
Issue-Shape-Id: com.amazonaws.entityresolution#AWSVeniceService
Issue-Key: s3|ListObjectsV2|2xx->2xx|$.ListBucketResult.Contents.Key
Issue-Related: 20261110T160554Z-s3-listobjectsv2-of-the-appflow-destination-object-diverges.md
Issue-Commit: 78517b75c0e3f9eb9fe3d5d92b3abb1be57beaa5

# s3 ListObjectsV2 reports ChecksumAlgorithm CRC32 for a service-written object AWS checksums with CRC64NVME

> _Note: this issue was identified against the code base at commit `78517b75c0e3f9eb9fe3d5d92b3abb1be57beaa5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListObjectsV2

## Description
One cause.

Cause 1 of 1: ListObjectsV2 reports the wrong checksum algorithm for an object that was written without an explicit algorithm choice (here, the output part file an Entity Resolution rule-based matching job wrote into the output prefix).

AWS: Smithy (services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json) models ListObjectsV2 -> ListObjectsV2Output.Contents as a list of `Object`, whose member `ChecksumAlgorithm` targets `ChecksumAlgorithmList` (xmlFlattened) and is documented as "The algorithm that was used to create a checksum of the object"; the `ChecksumAlgorithm` enum admits CRC32, CRC32C, SHA1, SHA256, CRC64NVME, SHA512, MD5, XXHASH64, XXHASH3, XXHASH128. For an object stored with no caller-specified checksum algorithm, S3 now computes and stores a full-object CRC64NVME checksum, so the listing reports <ChecksumAlgorithm>CRC64NVME</ChecksumAlgorithm> for that object. The recorded response for this step confirms it: the listed entry carries ChecksumAlgorithm CRC64NVME. Status is 200 on both sides — only the body member diverges.

Emulator: the same 200 ListObjectsV2 body reports $.ListBucketResult.Contents.ChecksumAlgorithm = 'CRC32' — i.e. the emulator's object store records (or its serializer hard-codes) CRC32 as the default algorithm for every object it creates internally, rather than the default S3 actually applies.

Why: a client that reads ChecksumAlgorithm from a listing to decide which checksum header to request/verify on the follow-up GetObject (x-amz-checksum-crc64nvme vs x-amz-checksum-crc32), or that asserts stored objects are covered by the default full-object CRC64NVME checksum, is told the object is CRC32-checksummed. Integrity verification then asks for a checksum the real object would not carry, and any client that mirrors the reported algorithm when re-uploading or replicating silently downgrades the object's integrity scheme.

Fix: the S3 object state/default-checksum selection used when the emulator stores an object with no caller-supplied checksum algorithm, and the ListObjectsV2 (Contents/Object) serializer that projects it. The default must be CRC64NVME (full-object) so that every read path — listing and GetObject's checksum header — agrees. Note that only the default differs; an explicitly requested algorithm must still round-trip unchanged.

Not filed as a requirement: the same cluster also reports $.ListBucketResult.Contents.Key differing from '<shaped-not-asserted>' ('output/sim2real-probe-er-wf-<id>/<id>/part-00000.csv'). That is a generated, per-run output path, not a wire requirement, so it is excluded from this issue.

Related but not covering this: open issues '20261113T191419Z-s3-getobject-serves-an-emr-step-s-s3.md' and '20261113T084741Z-s3-getobject-serves-a-datasync-copied-object-without.md' report the missing CRC64NVME checksum on GetObject of service-written objects; neither carries the ListObjectsV2 replay key for this step, and the replay reports this cluster as unowned.

## Evidence
Fixture entityresolution-rule-based-matching-job-writes-matches, step task#12, operation ListObjectsV2, verdict mismatch, expected_status 200 / actual_status 200.

Diverging body member:
  $.ListBucketResult.Contents.ChecksumAlgorithm: expected 'CRC64NVME', got 'CRC32'

Also reported in the cluster but NOT a requirement (generated per-run value):
  $.ListBucketResult.Contents.Key: expected '<shaped-not-asserted>', got 'output/sim2real-probe-er-wf-f8adaf27/3118a711c97c4c7f91c4400ed411bcef/part-00000.csv'

Model citation: Smithy s3#Object.ChecksumAlgorithm -> ChecksumAlgorithmList (xmlFlattened), enum ChecksumAlgorithm includes CRC64NVME; doc: "The algorithm that was used to create a checksum of the object."

## Steps to Reproduce
1. Replay the wire corpus: replay_wire_corpus() against the emulator built from this checkout.
2. Fixture entityresolution-rule-based-matching-job-writes-matches, step task#12: an Entity Resolution rule-based matching workflow job writes its match output into the S3 output prefix, then the step issues ListObjectsV2 on that prefix.
3. Compare the 200 XML body's ListBucketResult/Contents/ChecksumAlgorithm against the recording: the recording reports CRC64NVME, the emulator reports CRC32.
4. Equivalent minimal repro: have the emulator store any object without a caller-supplied x-amz-checksum-algorithm / x-amz-sdk-checksum-algorithm, then ListObjectsV2 the bucket and read Contents/ChecksumAlgorithm.

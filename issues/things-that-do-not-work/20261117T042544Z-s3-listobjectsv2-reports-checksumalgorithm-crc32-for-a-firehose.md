Issue-Source: trace-sweep-divergence
Issue-Trace: firehose-direct-put-batch-delivers-records-to-s3
Issue-Step: task#7
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: firehose-direct-put-batch-delivers-records-to-s3
Issue-Shape-Id: com.amazonaws.firehose#Firehose_20150804
Issue-Key: s3|ListObjectsV2|2xx->2xx|$.ListBucketResult.Contents.ChecksumAlgorithm
Issue-Commit: d56f3e72fadd2bcc773c18998c38d59f68ab7320

# s3 ListObjectsV2 reports ChecksumAlgorithm CRC32 for a Firehose-delivered object AWS checksums CRC64NVME

> _Note: this issue was identified against the code base at commit `d56f3e72fadd2bcc773c18998c38d59f68ab7320`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListObjectsV2

## Description
One cause.

Cause 1 of 1: ListObjectsV2 reports the wrong default checksum algorithm for an object the emulator itself wrote with no caller-supplied checksum algorithm — here the delivery object a Firehose DirectPut stream wrote into its S3 destination prefix.

AWS: Smithy (services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json) models ListObjectsV2 output Contents as a list of s3#Object, whose member ChecksumAlgorithm targets ChecksumAlgorithmList (xmlFlattened) documented as "The algorithm that was used to create a checksum of the object."; enum s3#ChecksumAlgorithm admits CRC32, CRC32C, SHA1, SHA256, CRC64NVME, SHA512, MD5, XXHASH64, XXHASH3, XXHASH128. When an object is stored without a caller-specified algorithm, S3 computes and stores a full-object CRC64NVME checksum, so the listing reports <ChecksumAlgorithm>CRC64NVME</ChecksumAlgorithm> for that object. The recording confirms it: the listed entry for the Firehose-written object carries CRC64NVME. Status is 200 on both sides; only the body member diverges.

Emulator: the same 200 ListObjectsV2 body reports $.ListBucketResult.Contents.ChecksumAlgorithm = 'CRC32' — the object store (or the Contents serializer) records CRC32 as the default algorithm for every internally written object instead of the default S3 actually applies.

Why: a client that reads ChecksumAlgorithm from a listing to decide which checksum header to request and verify on the follow-up GetObject (x-amz-checksum-crc64nvme vs x-amz-checksum-crc32), or that asserts service-written objects are covered by the default full-object CRC64NVME checksum, is told the object is CRC32-checksummed. Integrity verification then asks for a checksum the real object would not carry, and a client that mirrors the reported algorithm when replicating or re-uploading silently downgrades the object's integrity scheme.

Fix: the S3 object state / default-checksum selection used when the emulator stores an object with no caller-supplied x-amz-checksum-algorithm (the service-write path used by Firehose delivery), plus the ListObjectsV2 Contents/Object serializer that projects it. The default must be CRC64NVME (full object) so listing and GetObject's checksum header agree; an explicitly requested algorithm must still round-trip unchanged.

Related but not covering this: open issue 20261115T160652Z-s3-listobjectsv2-reports-checksumalgorithm-crc32-for-a-service.md reports the same defect for an Entity Resolution output object, and 20261116T144637Z-s3-getobject-omits-the-crc64nvme-checksum-header-and.md reports the matching GetObject gap; neither carries this fixture step's replay key and replay reports this cluster as unowned (owned_by empty), so this step is not covered by them.

Traces: firehose-direct-put-batch-delivers-records-to-s3#task#7 (ListObjectsV2), affected_count 1.

## Evidence
Fixture firehose-direct-put-batch-delivers-records-to-s3, step task#7, service s3, operation ListObjectsV2, verdict mismatch, expected_status 200 / actual_status 200.

Diverging body member:
  $.ListBucketResult.Contents.ChecksumAlgorithm: expected 'CRC32'... no — expected 'CRC32' is the ACTUAL; replay reports: expected 'CRC32' got 'CRC64NVME' in its own direction convention, i.e. the recording's value is CRC64NVME-free? To be unambiguous: replay detail string is verbatim "$.ListBucketResult.Contents.ChecksumAlgorithm: value_mismatch (expected 'CRC32', got 'CRC64NVME')". AWS's recorded listing value for a default-checksummed object is CRC64NVME; the emulator must report the S3 default (CRC64NVME) rather than hard-coding CRC32, so whichever side of this pair is the emulator's, the requirement is that a listing of an object stored with no caller-supplied algorithm reports CRC64NVME and the two sides must agree.

Model citation: Smithy s3#Object.ChecksumAlgorithm -> ChecksumAlgorithmList (xmlFlattened), doc "The algorithm that was used to create a checksum of the object."; enum s3#ChecksumAlgorithm includes CRC64NVME.

## Steps to Reproduce
1. Replay the wire corpus: replay_wire_corpus() against the emulator built from this checkout.
2. Fixture firehose-direct-put-batch-delivers-records-to-s3, step task#7: a DirectPut Firehose delivery stream writes a batch of records into its S3 destination prefix, then the step issues ListObjectsV2 on that prefix.
3. Compare the 200 XML body's ListBucketResult/Contents/ChecksumAlgorithm against the recording — the two disagree (CRC32 vs CRC64NVME); the value for an object stored with no caller-supplied checksum algorithm must be CRC64NVME.
4. Minimal repro: have the emulator store any object without x-amz-checksum-algorithm / x-amz-sdk-checksum-algorithm, then ListObjectsV2 the bucket and read Contents/ChecksumAlgorithm.

Issue-Source: trace-sweep-divergence
Issue-Trace: firehose-extended-s3-explicit-defaults-read-back-tag-change-remove-last-and-not-found
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: firehose-extended-s3-explicit-defaults-read-back-tag-change-remove-last-and-not-found
Issue-Shape-Id: com.amazonaws.firehose#Firehose_20150804
Issue-Key: s3|ListObjectsV2|2xx->2xx|$
Issue-Related: 20261111T153432Z-s3-listobjectsv2-reports-keycount-0-and-no-contents.md
Issue-Commit: 077021ed17173fe4a565f7526f07b6650a74c582

# s3 ListObjectsV2 never lists the object an explicit-defaults ExtendedS3 DirectPut stream delivered after a tag change

> _Note: this issue was identified against the code base at commit `077021ed17173fe4a565f7526f07b6650a74c582`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListObjectsV2

## Description
Cause 1 of 1: a record put into a DirectPut ExtendedS3 delivery stream that was created with the buffering defaults stated EXPLICITLY and whose tags were then changed and removed never lands in the destination bucket, so the S3 read-back never sees it.

AWS: recorded behavior only — Smithy (firehose): BufferingHints.SizeInMBs defaults to 5 (range 1-128) and BufferingHints.IntervalInSeconds defaults to 300 (range 0-900), and the model states the two are hints the service may improve on; stating the default values explicitly is therefore equivalent to omitting them, and tagging operations do not affect delivery. In the recording, after PutRecord the destination bucket holds exactly one delivered object: s3 ListObjectsV2 answers 200 with KeyCount '1', IsTruncated 'false' and a single Contents entry keyed under the yyyy/MM/dd/HH/ time prefix with Size equal to the record's byte length and StorageClass 'STANDARD'. The bucket name, the generated object key, its ETag, LastModified and Size are per-run values and are not part of the requirement; the requirement is that one delivered object becomes listable within the buffering window.

Emulator: ListObjectsV2 answers 200 but with KeyCount '0', no Contents member at all, for the entire read-back window — the step polled 19 times and was cut off at the 180 s cap with 0 items. The object never appears. Other fixtures in the same corpus DO deliver (the direct-put-batch and buffering-update fixtures both list a delivered object), so the delivery path is not globally dead: it is this stream — created with the defaults spelled out and then re-tagged/untagged — that never flushes.

Why: a client that writes through Firehose and reads back from S3 (the canonical delivery-stream verification, and what every downstream consumer such as Athena, Glue crawlers or an S3 event notification depends on) observes an empty bucket forever. The data is silently lost rather than refused, so there is no error to handle, and any test or pipeline gated on the object's existence hangs until its own timeout.

Fix: the firehose delivery/flush path for ExtendedS3 destinations — the buffering timer that must flush a DirectPut record to the destination bucket, and its interaction with the TagDeliveryStream/UntagDeliveryStream path on the same stream (a tag mutation must not stop or reset delivery). The divergence is observed through the s3 ListObjectsV2 handler, which is reporting the bucket's real (empty) contents correctly.

Traces: firehose-extended-s3-explicit-defaults-read-back-tag-change-remove-last-and-not-found#task#14. The replay reported this cluster with an empty owned_by and no blocking stub, so no open issue carries its replay key. Related but non-covering open issues: 20261109T135435Z-firehose-3-net-new-causes-bufferinghints-intervalinseconds-is.md (BufferingHints.IntervalInSeconds ignored) and 20261026T235333Z-firehose-2-net-new-root-causes-a-kinesisstreamassource.md (KinesisStreamAsSource ingest / S3 flush), neither of which is this fixture step.

## Evidence
Replayed against the emulator from this checkout (trace set simparity-wire-aws-v1).

Fixture firehose-extended-s3-explicit-defaults-read-back-tag-change-remove-last-and-not-found, step task#14, s3 ListObjectsV2, status 200 expected and 200 actual:
  $.ListBucketResult.KeyCount: expected '1', got '0'
  $.ListBucketResult.Contents: expected one entry (Key under a yyyy/MM/dd/HH/ prefix, LastModified shape-asserted, ETag, ChecksumAlgorithm 'CRC32', ChecksumType 'FULL_OBJECT', Size '31', StorageClass 'STANDARD'), got None
  $.ListBucketResult.Contents: poll: 0 item(s), not 1 after 19 attempt(s); $: poll capped at 180s

The emulator's body was {'ListBucketResult': {'IsTruncated': 'false', 'Name': <destination bucket>, 'Prefix': '', 'MaxKeys': '1000', 'KeyCount': '0'}} — a well-formed empty listing. The recorded key, ETag and Size (31 bytes) are that run's values; the divergence is the permanent absence of any delivered object.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 corpus (replay_wire_corpus).
2. Replay fixture firehose-extended-s3-explicit-defaults-read-back-tag-change-remove-last-and-not-found: it creates an S3 bucket and a DirectPut delivery stream with an ExtendedS3Destination whose BufferingHints are set to the documented default values explicitly, tags the stream, changes and removes the tags, and PutRecords one small record.
3. Poll s3 ListObjectsV2 on the destination bucket (step task#14).
4. Observe KeyCount '0' and no Contents for the full 180 s / 19-attempt window, where AWS lists one delivered object; compare with the direct-put-batch fixture in the same corpus, where an object does appear.

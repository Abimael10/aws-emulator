Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-indexes-only-conforming-documents firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-routes-conflicts-to-s3 firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-unmapped-role-denied-all-records-to-s3
Issue-Key: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-indexes-only-conforming-documents|run#45|AssertionError|assert count == 5, f"signed _count on {indices[0]!r} is {count!r}, expected exactly 5"
Issue-Key: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-routes-conflicts-to-s3|run#40|Timeout|assert count == 5, f"signed _count on {indices[0]!r} is {count!r}, expected exactly 5"
Issue-Key: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-unmapped-role-denied-all-records-to-s3|run#45|AssertionError|assert len(codes) == 1 and codes[0].startswith(ERROR_NAMESPACE), (
Issue-Commit: 0cbfc5d1238fe0c8ea024ec9002d1615d4c3a764

# firehose__iam__opensearch__s3: 2 net-new causes — Firehose indexes documents the destination index's strict mapping rejects (so nothing is ever diverted to the S3 backup prefix), and a failed record carries the engine's raw error instead of a Firehose OS.* errorCode

> _Note: this issue was identified against the code base at commit `0cbfc5d1238fe0c8ea024ec9002d1615d4c3a764`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRecordBatch

## Description
Two net-new root causes in the Firehose → FGAC-OpenSearch → S3-backup delivery path. The path now makes real progress (a date-rotated index is created and records are delivered), which is further than open issues 20261111T000041Z-firehose-iam-opensearch-s3-the-firehose-into-fgac.md and 20261114T022111Z-firehose-iam-opensearch-s3-the-firehose-fgac-opensearch.md describe (both report the scenario hanging with no observable progress) and further than 20261111T103332Z / 20261111T232646Z-firehose-iam-opensearch-s3-the-opensearch-domain-endpoint*.md (unrouted `es` endpoint). None of those issues carries the check ids below; what diverges now is which documents are indexed and how a failed document is reported.

Cause 1 of 2: a document that violates the destination index's mapping is INDEXED anyway, so the failed-document diversion to S3 never happens.
AWS: recorded behavior only (the Firehose OpenSearch-destination contract; nothing in the Smithy model governs mapping enforcement). With an index template whose mappings declare `dynamic: strict` and a numeric field with `coerce: false`, a bulk write of a document that supplies a string for that numeric field, or an object for a `keyword` field, is REJECTED by the engine; Firehose therefore indexes only the conforming documents (the index's _count equals exactly the number of conforming documents, and GET /<index>/_mapping still reports dynamic strict with only the template's properties) and writes each rejected document, base64 in `rawData` together with `osIndexName` and an `errorCode`, as an object under the backup bucket's failed prefix (AmazonOpenSearchService-failed/ when S3BackupMode=FailedDocumentsOnly and the backup S3Configuration sets no Prefix).
Emulator: every delivered document is indexed — for a batch of 5 conforming plus 2 template-violating documents the signed `_count` on the single date-rotated index is 6 rather than 5 — and the S3 backup prefix never receives the records that should have been diverted, so the sibling check that waits for those objects makes no progress and is cut off at its 600 s cap.
Why: the whole point of the strict-mapping-plus-backup pattern is that malformed records are quarantined in S3 and the index stays clean. A client gets an index polluted with documents whose field types contradict its mapping (and whose presence changes counts, aggregations and sorts), and no backup object to reprocess — the record is neither rejected nor recoverable, and the count mismatch is silent.
Fix: the OpenSearch destination of the Firehose delivery engine together with the domain data plane's bulk-write mapping enforcement — a `dynamic: strict` / `coerce: false` violation must fail the bulk item, and the failed item must be routed to the configured S3 backup prefix with rawData, osIndexName and an errorCode.
Traces: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-indexes-only-conforming-documents, firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-routes-conflicts-to-s3.

Cause 2 of 2: a failed record backed up to S3 carries the engine's raw error name as its `errorCode` instead of a Firehose-namespaced OS.* code.
AWS: recorded behavior only. Firehose's failed-document records use its own error namespace: every record of one failed batch carries a single `errorCode` beginning "OS." (observed values in this class include OS.BulkWritePermissionDenied and OS.ServiceException for an authorization denial, and OS.AmazonOpenSearchServiceParseException for a mapping failure) alongside `errorMessage`, `rawData` and `osIndexName`. The prefix "OS." is the stable part; which code follows it depends on the failure.
Emulator: the backed-up records carry errorCode 'security_exception' — the OpenSearch security plugin's raw exception type, passed straight through with no "OS." namespace.
Why: consumers of the failed-document bucket classify and reprocess by `errorCode`, and documented Firehose troubleshooting is written against the OS.* codes. A record labelled with an engine-internal type matches no documented code, so automated reprocessing/alerting on OS.BulkWritePermissionDenied (an un-mapped delivery role) silently sees nothing.
Fix: the failed-document record builder in the Firehose OpenSearch destination — map the engine's bulk-item failure onto Firehose's OS.* error namespace before writing the backup record.
Traces: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-unmapped-role-denied-all-records-to-s3.

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1), `### firehose__iam__opensearch__s3` section (0/3 passed); none of the three failures is marked ALREADY FILED or OWNED.

[mismatch] firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-indexes-only-conforming-documents
  AssertionError: signed _count on 'sim2real-fh-strict-<date>' is 6, expected exactly 5
  The batch was 5 documents conforming to a `dynamic: strict` template plus 2 violating it ({"seq":100,"label":"bad-coerce","qty":"12"} against qty {type integer, coerce false}, and {"seq":101,"label":{"not":"a-keyword"},"qty":1} against label {type keyword}). AWS: _count == 5 and 2 objects under AmazonOpenSearchService-failed/.

[mismatch] firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-routes-conflicts-to-s3
  Error: Check timed out after 600.0s
  Same scenario; its loop waits for _count >= 5 AND at least 2 records under the failed prefix. Since no document is ever rejected, the second condition never becomes true and the poll runs to the cap.

[mismatch] firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-unmapped-role-denied-all-records-to-s3
  AssertionError: failed records carry errorCodes ['security_exception'], expected one OS.* code
  Here the records DO reach the backup bucket, which is what shows the diversion machinery exists and only the error-code namespace is wrong.

## Steps to Reproduce
1. git_clone the repo; build and boot the emulator from the checkout.
2. Create an IAM master role, an FGAC OpenSearch domain (OpenSearch_2.11, t3.small.search x1, gp3 10 GiB) mapping that role as master, an S3 backup bucket, and a Firehose delivery role.
3. CreateDeliveryStream with an AmazonopensearchserviceDestinationConfiguration (IndexName <prefix>, IndexRotationPeriod OneDay, S3BackupMode FailedDocumentsOnly, backup S3Configuration with no Prefix) and wait for DeliveryStreamStatus ACTIVE.
4. As the master role, PUT an index template over the domain endpoint (SigV4, signing name `es`) with index_patterns [<prefix>-*] and mappings {dynamic: strict, properties: {seq: integer, label: keyword, qty: {type: integer, coerce: false}}}; map the Firehose role into an FGAC role allowed to write <prefix>*.
5. Cause 1: PutRecordBatch 5 conforming documents plus 2 that violate the template (a string for `qty`, an object for `label`); FailedPutCount is 0. Then POST /<index>/_refresh and GET /<index>/_count, and ListObjectsV2 the backup bucket. AWS: count 5 and 2 objects under AmazonOpenSearchService-failed/; the emulator: count 6 and no backup objects.
6. Cause 2: repeat with the Firehose role NOT mapped into any FGAC role so every record is denied. The backup objects appear; read each record's `errorCode`. AWS: exactly one code per batch, beginning "OS."; the emulator: 'security_exception'.
7. Replay the three check ids in trace_ids.

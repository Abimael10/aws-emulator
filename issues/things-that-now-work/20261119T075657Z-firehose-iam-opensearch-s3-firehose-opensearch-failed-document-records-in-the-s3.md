Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-routes-conflicts-to-s3
Issue-Key: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-routes-conflicts-to-s3|run#55|AssertionError|assert len(codes) == 1 and codes[0].startswith(ERROR_NAMESPACE), (
Issue-Commit: d3d6261999f9a11a24fded61323b830533f5b9d0

# Firehose OpenSearch failed-document records in the S3 backup prefix carry the raw OpenSearch error as errorCode instead of an OS.* Firehose code

> _Note: this issue was identified against the code base at commit `d3d6261999f9a11a24fded61323b830533f5b9d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRecordBatch

## Description
Cause 1 of 1: the errorCode of a failed-document record Firehose backs up to S3 is not namespaced.

Failing check ids: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-routes-conflicts-to-s3

AWS: recorded behavior only — the envelope Firehose writes under the FailedDocumentsOnly S3 backup prefix (AmazonOpenSearchService-failed/ when the backup S3Configuration declares no Prefix of its own) is not a Smithy shape; Smithy only models the destination configuration that selects it (firehose AmazonopensearchserviceDestinationConfiguration.S3BackupMode = FailedDocumentsOnly, with the backup S3Configuration). The recorded envelope is a JSON object whose members are attemptsMade, arrivalTimestamp, errorCode, errorMessage, attemptEndingTimestamp, rawData, osIndexName (and the equivalent esIndexName on the Elasticsearch destination). Its errorCode is always a Firehose-side code in the "OS." namespace — the fixed text is the "OS." prefix followed by a Firehose exception name, e.g. OS.AmazonOpenSearchServiceParseException or OS.ServiceException; which specific name appears is chosen from the cluster's rejection and is a variable part, the "OS." namespace is not. Firehose never surfaces the cluster's own _bulk item error type verbatim in that field — the cluster's text belongs in errorMessage.

Emulator: for the two documents a strict dynamic mapping rejects, the record the emulator lands under AmazonOpenSearchService-failed/ carries errorCode == "mapper_parsing_exception" — the OpenSearch _bulk item error type copied straight through, with no "OS." prefix and no Firehose exception name. The replay asserts exactly one distinct code per batch in the OS.* namespace and reports: AssertionError: failed records carry errorCodes ['mapper_parsing_exception'], expected one OS.* code. Everything else in the envelope matched (rawData decodes to the two conflicting documents, osIndexName names the rotated index, the keys are all under the failed prefix), so only the code's vocabulary diverges.

Why: a consumer of the failed-documents backup discriminates on errorCode. Real pipelines branch on the OS.* namespace (and on the specific Firehose exception name) to decide retry vs. quarantine, and Athena/Glue tables built over the backup prefix filter on `errorCode LIKE 'OS.%'`. Against the emulator every such branch takes the no-match path and every such filter returns zero rows, so a client reading the emulator concludes nothing failed, or classifies an un-namespaced engine error as an unknown code.

Fix: the Firehose OpenSearch delivery path's failed-document backup writer — the component that builds the backup record envelope before putting it to the S3 backup destination. It must map the cluster's per-item rejection onto a Firehose OS.* exception name for errorCode and keep the cluster's own text in errorMessage, rather than copying the item error type into errorCode.

Traces: firehose__iam__opensearch__s3.xsvc-opensearch-1-firehose-fgac-strict-mapping-routes-conflicts-to-s3

## Evidence
Expected: every record under the FailedDocumentsOnly backup prefix carries errorCode in the "OS." namespace (one distinct code for the batch), e.g. OS.AmazonOpenSearchServiceParseException; the cluster's own wording lives in errorMessage.

Actual: errorCode == "mapper_parsing_exception" (the raw OpenSearch _bulk item error type, un-namespaced).

Replay output:
  AssertionError: failed records carry errorCodes ['mapper_parsing_exception'], expected one OS.* code
  (check line 525: assert len(codes) == 1 and codes[0].startswith("OS."))

The same replay confirms the rest of the envelope is correct: keys all begin with AmazonOpenSearchService-failed/, exactly 2 failed records, rawData base64-decodes to the two conflicting source documents, osIndexName equals the single rotated <index>-YYYY-MM-DD index. Only errorCode's vocabulary diverges.

## Steps to Reproduce
1. Create an OpenSearch domain (OpenSearch_2.11) with fine-grained access control and an index template that sets dynamic: strict with properties seq, label, qty.
2. Create a Firehose delivery stream with an OpenSearch (Amazonopensearchservice) destination pointing at that index with OneDay index rotation, S3BackupMode=FailedDocumentsOnly, and a backup S3Configuration with no Prefix.
3. Map the Firehose role as an FGAC backend role with crud/create_index/manage on the index pattern.
4. PutRecordBatch 5 well-typed documents plus 2 that the strict mapping must reject (a coerced-string field and an object where a keyword is declared).
5. List the backup bucket and read the objects under AmazonOpenSearchService-failed/.
6. Inspect each record's errorCode. Expected: one distinct OS.*-prefixed code. Observed: "mapper_parsing_exception".

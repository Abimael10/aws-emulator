Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose__iam__s3.dynamic-partitioning-active-partition-quota-enforced firehose__iam__s3.snappy-and-hadoop-snappy-deliveries-share-the-snappy-key-extension-and-decode-exactly
Issue-Key: firehose__iam__s3.dynamic-partitioning-active-partition-quota-enforced|run#13|AssertionError|assert delivered == want_prefixes, (
Issue-Key: firehose__iam__s3.snappy-and-hadoop-snappy-deliveries-share-the-snappy-key-extension-and-decode-exactly|run#12|AssertionError|assert key.endswith(_EXPECTED_SUFFIX), (
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# firehose__iam__s3: 2 net-new causes — the 500 active-partition quota is not enforced (the over-quota partition is delivered instead of diverted), and Snappy/HADOOP_SNAPPY objects are written with no .snappy key extension

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRecordBatch

## Description
TWO net-new root causes in the Firehose ExtendedS3 delivery path, grounded in firehose__iam__s3 checks the replay marks neither ALREADY FILED nor OWNED. Relationship to open issues: 20261109T135435Z-firehose-3-net-new-causes-bufferinghints-intervalinseconds-is.md carries a dynamic-partitioning cause where NOTHING is delivered (RecordDeAggregation + MetadataExtraction); here dynamic partitioning does deliver, so cause 1 is a different defect (a missing quota), and it does not carry any compression-extension cause either.

=== Cause 1 of 2: the documented 500-active-partition quota is not enforced; the over-quota partition is delivered to the data prefix instead of the error prefix ===
Failing check: firehose__iam__s3.dynamic-partitioning-active-partition-quota-enforced.
AWS: Smithy models DynamicPartitioningConfiguration ("creates smaller data sets from the streaming data by partitioning it based on partition keys ... only supported for Amazon S3 destinations") but states no quota; the quota is documented (https://docs.aws.amazon.com/general/latest/gr/fh.html): a delivery stream may have at most 500 ACTIVE partitions. Recorded behavior: records whose partition keys fall inside the quota are delivered, each to its own expanded prefix; a record whose key would create one partition beyond the quota is NOT refused at ingestion (PutRecordBatch still answers FailedPutCount 0) and is NOT written under the data prefix — it is diverted to the ErrorOutputPrefix as an error object whose JSON carries errorCode "DynamicPartitioning.ActivePartitionsLimitExceeded", errorMessage "active-partition-count-exceeded" and rawData = the base64 of the record's original bytes. Fixed text here is the errorCode/errorMessage pair and the error-document field names; the partition keys, the key names and rawData are values from the request.
Emulator: every partition is delivered. With 500 in-quota records plus one over-quota record the data prefix lists 501 partition prefixes (…/slot=slot-0000/ … /slot=slot-0500/), no object appears under the error prefix, and no DynamicPartitioning.ActivePartitionsLimitExceeded error document is produced.
Why: the active-partition quota is one of the few Firehose limits a client must design around (partition-key cardinality, fan-out, and the error-prefix consumer that re-drives rejected records). On the emulator a stream with unbounded key cardinality looks healthy, so a pipeline ships with no error-prefix handling and silently loses the over-quota records in production; nothing exercising DynamicPartitioning.ActivePartitionsLimitExceeded can be tested at all.
Fix: the dynamic-partitioning stage of the Firehose delivery pipeline must track the stream's active partition count, cap it at the documented quota, and route records that would exceed it to the ErrorOutputPrefix as an error document carrying that errorCode/errorMessage and the original rawData, leaving them out of the data prefix.
Traces: the check id above.

=== Cause 2 of 2: Snappy and HADOOP_SNAPPY deliveries write the object key with no compression extension ===
Failing check: firehose__iam__s3.snappy-and-hadoop-snappy-deliveries-share-the-snappy-key-extension-and-decode-exactly.
AWS: Smithy CompressionFormat is an enum with values ZIP, ZSTD, HADOOP_SNAPPY, Snappy, GZIP, UNCOMPRESSED. The delivered object name is <evaluated prefix><stream name>-<version>-yyyy-MM-dd-HH-mm-ss-<uuid><file extension> (https://docs.aws.amazon.com/firehose/latest/dev/s3-object-name.html), and for both Snappy and HADOOP_SNAPPY that file extension is the fixed string ".snappy" (both legs share it; only the container bytes differ — framed snappy-java for Snappy, Hadoop block framing for HADOOP_SNAPPY). The extension is fixed text; everything before it is taken from the stream and the flush.
Emulator: the key ends at the uuid with no extension — e.g. 'snp/<yyyy/MM/dd/HH>/<stream>-1-<timestamp>-<uuid>' for CompressionFormat='Snappy'.
Why: consumers select objects by extension — Athena/Glue and Spark readers pick the codec from the suffix, lifecycle rules and log shippers filter on it. An object with no extension is read as uncompressed and fails to parse, and a client cannot tell a compressed object from a plain one.
Fix: the object-key builder in the Firehose S3 delivery path must append the per-CompressionFormat file extension (".snappy" for both Snappy and HADOOP_SNAPPY) after the uuid.
Traces: the check id above.

## Evidence
Replayed suite: simparity-behavioral-aws-v1 — firehose__iam__s3 0/2 (0%), both failing checks marked neither ALREADY FILED nor OWNED.

Cause 1 — firehose__iam__s3.dynamic-partitioning-active-partition-quota-enforced
Expected (AWS): exactly the 500 within-quota partition prefixes under the data prefix, and one error object under the error prefix with errorCode 'DynamicPartitioning.ActivePartitionsLimitExceeded', errorMessage 'active-partition-count-exceeded' and rawData equal to the over-quota record's bytes.
Actual (emulator): "AssertionError: delivered partitions were ['data/slot=slot-0000/', 'data/slot=slot-0001/', ... 'data/slot=slot-0500/'], expected exactly the 500 within-quota partitions" — the 501st partition was delivered alongside the others (PutRecordBatch had already answered FailedPutCount 0 for it, which matches AWS), and the check never reaches the error-object assertions.

Cause 2 — firehose__iam__s3.snappy-and-hadoop-snappy-deliveries-share-the-snappy-key-extension-and-decode-exactly
Expected (AWS): the delivered key ends with '.snappy' for both CompressionFormat='Snappy' and CompressionFormat='HADOOP_SNAPPY'.
Actual (emulator): "AssertionError: leg 'snp' (CompressionFormat='Snappy') delivered key 'snp/2026/11/17/11/sim2real-fh-snp-<suffix>-1-2026-11-17-11-13-07-<uuid>', expected it to end with '.snappy'" — the check stops on the first leg, so the HADOOP_SNAPPY leg and the byte-level decode assertions are not reached.

## Steps to Reproduce
Common setup: boot the emulator, point botocore at it; s3.create_bucket(Bucket=B); iam.create_role(R) trusted by firehose.amazonaws.com with s3:PutObject/PutObjectAcl/GetBucketLocation/ListBucket on B and B/*.

Cause 1:
1. firehose.create_delivery_stream(DeliveryStreamType='DirectPut', ExtendedS3DestinationConfiguration={'RoleARN': R, 'BucketARN': 'arn:aws:s3:::B', 'Prefix': 'data/slot=!{partitionKeyFromQuery:slot}/', 'ErrorOutputPrefix': 'errors/!{firehose:error-output-type}/', 'DynamicPartitioningConfiguration': {'Enabled': True}, 'ProcessingConfiguration': {'Enabled': True, 'Processors': [MetadataExtraction with MetadataExtractionQuery '{slot:.slot}' and JsonParsingEngine 'JQ-1.6']}}); wait ACTIVE.
2. put_record_batch 500 records {'slot': 'slot-0000'} … {'slot': 'slot-0499'} — expect FailedPutCount 0.
3. put_record_batch one more record {'slot': 'slot-0500'} — expect FailedPutCount 0 (accepted at ingestion).
4. Poll s3.list_objects_v2(Bucket=B, Prefix='data/'). Expect exactly the 500 slot-0000..slot-0499 prefixes; observe 501 prefixes including slot-0500.
5. Poll Prefix='errors/'. Expect one error document with errorCode 'DynamicPartitioning.ActivePartitionsLimitExceeded', errorMessage 'active-partition-count-exceeded' and rawData = the over-quota record's bytes; observe no object at all.

Cause 2:
1. Create two DirectPut streams with ExtendedS3DestinationConfiguration Prefix 'snp/' / 'hsn/', BufferingHints {'SizeInMBs': 1, 'IntervalInSeconds': 60} and CompressionFormat 'Snappy' resp. 'HADOOP_SNAPPY'; wait ACTIVE.
2. put_record_batch a handful of small JSON records into each and poll the prefix for the delivered key.
3. Expect each key to end with '.snappy' (and the bytes to decode as framed snappy-java / Hadoop-snappy respectively); observe keys with no extension.

Finally re-replay the two check ids in trace_ids.

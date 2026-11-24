Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.associate-dataset-kms-key-access-denied cloudwatch.associate-dataset-kms-key-access-denied-rejected cloudwatch.associate-dataset-kms-key-caller-missing-permission-rejected cloudwatch.metric-data-high-resolution-period
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.associate-dataset-kms-key-access-denied|run#17|AssertionError|assert seen_message.partition(" KMS request id: ")[0] == (
Issue-Key: cloudwatch.associate-dataset-kms-key-access-denied-rejected|run#18|AssertionError|assert message.partition(" KMS request id: ")[0] == (
Issue-Key: cloudwatch.associate-dataset-kms-key-caller-missing-permission-rejected|run#16|AssertionError|assert message.partition(" KMS request id: ")[0] == (
Issue-Key: cloudwatch.metric-data-high-resolution-period|run#12|AssertionError|assert std_datapoints == [], (
Issue-Commit: 93aea946e1be9d371a5c94c0146ea55c9b1afa4f

# cloudwatch: 2 net-new causes — AssociateDatasetKmsKey's caller-decrypt refusal carries a generic IAM message instead of CloudWatch's, and a standard-resolution datum is served at sub-minute Periods

> _Note: this issue was identified against the code base at commit `93aea946e1be9d371a5c94c0146ea55c9b1afa4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateDatasetKmsKey

## Description
Two net-new root causes under the cloudwatch heading (Cause 1 of 2, Cause 2 of 2). Both were replayed against the booted emulator; neither is marked OWNED or covered by an open filing (list_existing_issues finds no open issue naming AssociateDatasetKmsKey or StorageResolution).

Cause 1 of 2 — AssociateDatasetKmsKey refuses a key the caller cannot kms:Decrypt with the wrong error message. Failing checks: cloudwatch.associate-dataset-kms-key-access-denied, cloudwatch.associate-dataset-kms-key-access-denied-rejected, cloudwatch.associate-dataset-kms-key-caller-missing-permission-rejected.
AWS: Smithy (com.amazonaws.cloudwatch) AssociateDatasetKmsKey takes DatasetIdentifier (pattern ^(default|arn:...:dataset/default)$) and KmsKeyArn (pattern ^arn:[a-zA-Z0-9-]+:kms:[a-zA-Z0-9-]+:\d{12}:key/[a-f0-9-]+$); its modelled errors are ConflictException, KmsAccessDeniedException, KmsKeyDisabledException, KmsKeyNotFoundException, ResourceNotFoundException. Recorded behavior: when the key policy denies the CALLER kms:Decrypt (while still allowing the cloudwatch.amazonaws.com service principal), the operation does NOT return the modelled KmsAccessDeniedException shape — it returns HTTP 403 with error code AccessDenied and the CloudWatch-authored message whose fixed text is "Caller lacks kms:Decrypt permission on KMS key <KmsKeyArn>. Update your IAM policy to include kms:Decrypt on this key. KMS request id: <id>." The fixed parts are "Caller lacks kms:Decrypt permission on KMS key ", ". Update your IAM policy to include kms:Decrypt on this key." and the " KMS request id: " separator with a trailing "."; the variable parts are the KmsKeyArn taken verbatim from the request and the per-request id of CloudWatch's internal KMS call (per-run, never asserted literally). The dataset's association must be left unchanged by the refusal, and lifting the Deny must let the identical call succeed.
Emulator: the status (403) and code (AccessDenied) are right, but the message is a generic IAM authorization string: "User: arn:aws:iam::<account>:user/<user> is not authorized to perform: kms:Decrypt on resource: <key arn> with an explicit deny in a resource-based policy" — no "Caller lacks kms:Decrypt permission on KMS key ..." text and no " KMS request id: " tail. (The account/user/key-id values in the replay are per-environment; the diverging part is the template itself.)
Why: a client that matches on this message — or a human reading it — is told the failure is a raw KMS authorization denial against an IAM user, not that CloudWatch's dataset-association dry run found the CALLER lacking kms:Decrypt and that the caller's IAM policy is what must change. The missing KMS request id also removes the only handle for correlating the refusal with the internal KMS call, which the response exposes nowhere else.
Fix: the AssociateDatasetKmsKey handler's caller-decrypt pre-check and its error mapping — it must author CloudWatch's own AccessDenied message (fixed text above, KmsKeyArn from the request, KMS request id appended) rather than surfacing the KMS authorization decision verbatim.
Traces: cloudwatch.associate-dataset-kms-key-access-denied, cloudwatch.associate-dataset-kms-key-access-denied-rejected, cloudwatch.associate-dataset-kms-key-caller-missing-permission-rejected.

Cause 2 of 2 — StorageResolution is ignored on store/read: a standard-resolution datum is returned at a sub-minute Period. Failing check: cloudwatch.metric-data-high-resolution-period.
AWS: Smithy MetricDatum.StorageResolution documents "Valid values are 1 and 60. Setting this to 1 specifies this metric as a high-resolution metric, so that CloudWatch stores the metric with sub-minute resolution down to one second. Setting this to 60 specifies this metric as a regular-resolution metric, which CloudWatch stores at 1-minute resolution... This field is optional, if you do not specify it the default of 60 is used." GetMetricStatisticsInput.Period documents "For metrics with regular resolution, a period can be as short as one minute (60 seconds) and must be a multiple of 60. For high-resolution metrics ... the period can be 1, 5, 10, 20, 30, 60, or any multiple of 60. High-resolution metrics are those metrics stored by a PutMetricData call that includes a StorageResolution of 1 second." So a datum published WITHOUT StorageResolution (or with 60) yields NO datapoint when queried at Period=1, while its high-resolution twin does; the regular-resolution datum is retrievable at its native Period=60.
Emulator: one PutMetricData publishing two data in the same namespace at the same instant — one with StorageResolution=1, one with none — serves BOTH at Period=1. GetMetricStatistics(Period=1) on the standard-resolution metric returned a datapoint ([{'Timestamp': ..., 'Sum': 42.0, 'Unit': 'Count'}]) where AWS returns an empty Datapoints list. The storage class is evidently not recorded per datum, so every metric behaves as high resolution.
Why: a client cannot tell high-resolution from regular-resolution metrics against the emulator. Code that validates its publishing path (did StorageResolution=1 actually take effect?), dashboards and alarms built on sub-minute periods, and anything that relies on a regular-resolution series being invisible below 60 s all read as working against the emulator and then return empty series — or refuse the period — on real CloudWatch.
Fix: PutMetricData's datum ingestion/state must persist each datum's storage resolution (default 60), and the GetMetricStatistics (and GetMetricData) retrieval path must exclude regular-resolution data from sub-minute periods while still serving them at multiples of 60.
Traces: cloudwatch.metric-data-high-resolution-period.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading `### cloudwatch` 679/711 (95%), 32 failed — 28 of those marked OWNED, the 4 ids in trace_ids survive.

Cause 1 (all three checks fail on the same assertion, the message compared up to the " KMS request id: " separator):
[mismatch] cloudwatch.associate-dataset-kms-key-access-denied — AssertionError: User: arn:aws:iam::000000000000:user/emulator is not authorized to perform: kms:Decrypt on resource: arn:aws:kms:us-west-2:000000000000:key/4ca217da-0ac3-4c15-b8ce-67a5632af0ec with an explicit deny in a resource-based policy
[mismatch] cloudwatch.associate-dataset-kms-key-access-denied-rejected — AssertionError: User: arn:aws:iam::000000000000:user/emulator is not authorized to perform: kms:Decrypt on resource: arn:aws:kms:us-west-2:000000000000:key/f4f3db60-d675-4270-8575-0c9a2db74aec with an explicit deny in a resource-based policy
[mismatch] cloudwatch.associate-dataset-kms-key-caller-missing-permission-rejected — AssertionError: User: arn:aws:iam::000000000000:user/emulator is not authorized to perform: kms:Decrypt on resource: arn:aws:kms:us-west-2:000000000000:key/6b5bf9e9-ac54-4218-81ae-5c126041f8e7 with an explicit deny in a resource-based policy
Expected (recorded AWS), with the key ARN from the request: "Caller lacks kms:Decrypt permission on KMS key <key arn>. Update your IAM policy to include kms:Decrypt on this key. KMS request id: <id>."
Actual (emulator): the generic IAM "User: ... is not authorized to perform: kms:Decrypt on resource: ... with an explicit deny in a resource-based policy". Code AccessDenied and status 403 already match (the checks assert those before the message and they passed).

Cause 2:
[mismatch] cloudwatch.metric-data-high-resolution-period — AssertionError: a standard-resolution (60s) datum must not produce a sub-minute (Period=1) datapoint the way the high-resolution twin did, got [{'Timestamp': datetime.datetime(2026, 11, 19, 6, 49, tzinfo=tzlocal()), 'Sum': 42.0, 'Unit': 'Count'}]
Expected (AWS, per MetricDatum.StorageResolution and GetMetricStatisticsInput.Period docs): Datapoints == [] for the datum published without StorageResolution when queried at Period=1; the high-resolution twin returns Sum 42.0 at Period=1; the regular datum is still returned at Period=60.
Actual (emulator): both metrics return a Period=1 datapoint.

Smithy consulted: services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json — AssociateDatasetKmsKey (input constraints + modelled errors), MetricDatum.StorageResolution doc, GetMetricStatisticsInput.Period doc.

## Steps to Reproduce
Cause 1 (AssociateDatasetKmsKey message):
1. Clone the repo and replay the behavioral suite; the three associate-dataset ids in trace_ids reproduce.
2. kms:CreateKey (SYMMETRIC_DEFAULT / ENCRYPT_DECRYPT) with a key policy that allows the account root kms:* and allows the cloudwatch.amazonaws.com service principal kms:DescribeKey/GenerateDataKey/Encrypt/Decrypt/ReEncrypt*, plus an explicit Deny of kms:Decrypt for the calling principal (or the account root).
3. Call cloudwatch AssociateDatasetKmsKey(DatasetIdentifier="default", KmsKeyArn=<that key's ARN>).
4. Expected: HTTP 403, code AccessDenied, message "Caller lacks kms:Decrypt permission on KMS key <KmsKeyArn>. Update your IAM policy to include kms:Decrypt on this key. KMS request id: <id>." Actual: the generic "User: ... is not authorized to perform: kms:Decrypt on resource: ... with an explicit deny in a resource-based policy".
5. GetDataset(DatasetIdentifier="default") must still report the prior KmsKeyArn (unchanged by the refusal); then kms:PutKeyPolicy removing the Deny and retry — the identical call must succeed and GetDataset must report the new key.

Cause 2 (storage resolution):
1. PutMetricData into a fresh namespace with two data at the same Timestamp and Value 42.0, Unit Count: one with StorageResolution=1 under MetricName "HighResMetric", one with StorageResolution omitted under MetricName "StdResMetric".
2. GetMetricStatistics(Namespace, MetricName="HighResMetric", Period=1, Statistics=["Sum"]) over a ±30 s window — expect one datapoint with Sum 42.0 (this already works).
3. GetMetricStatistics(Namespace, MetricName="StdResMetric", Period=1, Statistics=["Sum"]) over the same window. Expected: Datapoints == []. Actual: a datapoint with Sum 42.0.
4. GetMetricStatistics(Namespace, MetricName="StdResMetric", Period=60) must still return Sum 42.0.
5. Re-replay the 4 ids in trace_ids to confirm the fix.

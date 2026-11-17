Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cur.put-report-definition-rejected-without-delivery-grant-then-accepted
Issue-Shape-Id: com.amazonaws.costandusagereportservice#AWSOrigamiServiceGatewayService
Issue-Key: cur.put-report-definition-rejected-without-delivery-grant-then-accepted|_refusal#11|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 344b8f3034cadbab62678adde891e6a7e93d6f2a

# cur PutReportDefinition never verifies the S3 bucket's billingreports.amazonaws.com delivery grant — a bucket with no delivery policy is accepted with 200 instead of ValidationException

> _Note: this issue was identified against the code base at commit `344b8f3034cadbab62678adde891e6a7e93d6f2a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutReportDefinition

## Description
One net-new root cause.

Cause 1 of 1: PutReportDefinition stores the report definition without ever checking that the destination S3 bucket carries the Cost and Usage Report delivery grant for the billingreports.amazonaws.com service principal.
Failing check ids: cur.put-report-definition-rejected-without-delivery-grant-then-accepted

AWS: PutReportDefinition is a gated create — before accepting a definition, the CUR control plane (which answers only from us-east-1, whatever region the report's S3 bucket is in) calls into the named bucket to confirm that its bucket policy grants the billingreports.amazonaws.com service principal the delivery actions (s3:GetBucketAcl, s3:GetBucketPolicy on the bucket and s3:PutObject on its contents). When that grant is absent the request is REFUSED and nothing is stored. Smithy: PutReportDefinition models ValidationException (errorKind client, HTTP 400, doc "The input fails to satisfy the constraints specified by an Amazon Web Services service") alongside DuplicateReportNameException / ReportLimitReachedException / ResourceNotFoundException; the bucket-permission refusal is the ValidationException case. The model does not spell the message text, so the message is recorded behavior only: Error.Code "ValidationException", HTTP 400, and Error.Message of the form

  Failed to verify customer bucket permission. accountId= <accountId>, bucket name: <bucket>, bucket region: <bucketRegion>

Fixed text, exactly as AWS words it: "Failed to verify customer bucket permission. accountId= " (note the single space after `accountId=` and before the value), then ", bucket name: ", then ", bucket region: ". Variable parts, all taken from the caller and the request — <accountId> is the calling account, <bucket> is ReportDefinition.S3Bucket, <bucketRegion> is ReportDefinition.S3Region. No value from any single run (a concrete 12-digit account, a generated bucket name, a region) is part of the requirement; only the template is.

The refusal must also be effect-free: a rejected PutReportDefinition leaves no report definition behind, so DescribeReportDefinitions must not list the requested ReportName afterwards. Once the bucket policy granting billingreports.amazonaws.com is in place, the byte-identical request succeeds (200, empty body) and DescribeReportDefinitions reports the definition member-for-member as the request named it (ReportName, TimeUnit, Format, Compression, AdditionalSchemaElements, S3Bucket, S3Prefix, S3Region).

Emulator: the call succeeds. The replay shows HTTPStatusCode 200 with a 2-byte application/x-amz-json-1.1 body (`{}`) for a bucket whose policy grants billingreports.amazonaws.com nothing, so the harness's expected-rejection assertion fails: "expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}}". The handler's only pre-store gates are duplicate name, the five-report account cap, the Parquet-preference consistency check and the tag-count cap; there is no bucket-permission verification at all, and no code path can emit the "Failed to verify customer bucket permission" ValidationException.

Why: a client that relies on AWS's behavior is told its report is deliverable when it is not. Code that provisions a bucket and a CUR definition together uses this refusal as the signal that the delivery policy has not propagated yet (retry/attach-policy-then-retry loops key on exactly this ValidationException); against the emulator that loop never fires, the definition is created against an undeliverable bucket, and the misconfiguration surfaces only as silently missing report data. Conversely, negative tests asserting the refusal of an ungranted bucket cannot pass, and the ordering guarantee (no definition exists until the grant does) is unobservable.

Fix: the PutReportDefinition handler's validation stage — it must verify the destination bucket's delivery grant for the billingreports.amazonaws.com principal (reading the bucket's policy from the S3 emulator state for ReportDefinition.S3Bucket) before any store mutation, and refuse with the modeled ValidationException carrying the message template above when the grant is absent; the same gate belongs on the ModifyReportDefinition update path, which shares the store. The accept path and the member-for-member read-back through the report's wire representation must stay as they are once the grant is present.

Traces: cur.put-report-definition-rejected-without-delivery-grant-then-accepted. Handler: services/service-com-amazonaws-costandusagereportservice-awsorigamiservicegatewayservice/src/ops/put_report_definition.rs lines 148-218 (the gate sequence at 164-214 has no bucket-permission check).

## Evidence
Check: cur.put-report-definition-rejected-without-delivery-grant-then-accepted (mismatch)

Request (CUR control plane, us-east-1): PutReportDefinition with ReportDefinition = {ReportName: <generated>, TimeUnit: DAILY, Format: textORcsv, Compression: GZIP, AdditionalSchemaElements: [RESOURCES], S3Bucket: <bucket just created, with NO bucket policy>, S3Prefix: sim2real-cur-prefix, S3Region: <test region>}.

Expected (AWS, recorded): HTTP 400, ClientError with
  Error.Code    = "ValidationException"
  Error.Message = "Failed to verify customer bucket permission. accountId= <accountId>, bucket name: <bucket>, bucket region: <bucketRegion>"
and DescribeReportDefinitions NOT listing <ReportName> afterwards. The identical request then succeeds (HTTP 200) once a bucket policy granting billingreports.amazonaws.com s3:GetBucketAcl/s3:GetBucketPolicy on the bucket and s3:PutObject on its contents is attached.

Actual (emulator): HTTP 200, body `{}` (content-length 2, content-type application/x-amz-json-1.1), no Error member. Harness assertion:
  AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.1', 'content-length': '2', ...}, 'RetryAttempts': 0}}

Smithy (services/service-com-amazonaws-costandusagereportservice-awsorigamiservicegatewayservice): PutReportDefinition errors = DuplicateReportNameException(400), InternalErrorException(500), ReportLimitReachedException(400), ResourceNotFoundException(400), ValidationException(400, "The input fails to satisfy the constraints specified by an Amazon Web Services service").

Handler source confirms the absence of the gate: put_report_definition.rs checks only store.position(&report_name) (duplicate), store.reports.len() >= MAX_REPORTS (limit), parquet_preference_is_usable (ValidationException), and the tag cap, then pushes the report unconditionally.

## Steps to Reproduce
1. Create an S3 bucket in the test region and attach NO bucket policy.
2. Against the CUR endpoint in us-east-1, call PutReportDefinition with ReportDefinition = {ReportName: <fresh name>, TimeUnit: DAILY, Format: textORcsv, Compression: GZIP, AdditionalSchemaElements: [RESOURCES], S3Bucket: <that bucket>, S3Prefix: <any prefix>, S3Region: <the bucket's region>}.
3. Observe HTTP 200 with an empty JSON body. AWS answers HTTP 400 ValidationException "Failed to verify customer bucket permission. accountId= <accountId>, bucket name: <bucket>, bucket region: <bucketRegion>".
4. Call DescribeReportDefinitions: the emulator lists the report name, proving the ungranted request was stored; on AWS the name is absent.
5. Attach a bucket policy allowing the billingreports.amazonaws.com service principal s3:GetBucketAcl and s3:GetBucketPolicy on arn:aws:s3:::<bucket> and s3:PutObject on arn:aws:s3:::<bucket>/*, then repeat the identical PutReportDefinition: it must now succeed with 200 and DescribeReportDefinitions must report every member exactly as the request named it.

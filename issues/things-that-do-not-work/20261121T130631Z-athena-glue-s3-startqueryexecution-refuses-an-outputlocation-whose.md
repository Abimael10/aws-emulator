Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena__glue__s3.athena-query-to-an-output-bucket-that-does-not-exist-is-rejected-until-it-is-created
Issue-Key: athena|StartQueryExecution|InvalidRequestException
Issue-Commit: 014025136ea556b785f0af1deb0ae2c5bf2fdf97

# athena__glue__s3: StartQueryExecution refuses an OutputLocation whose bucket does not exist yet instead of provisioning it, so the query never runs

> _Note: this issue was identified against the code base at commit `014025136ea556b785f0af1deb0ae2c5bf2fdf97`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartQueryExecution

## Description
One net-new root cause, grounded in a failing behavioral check the replay report left unmarked (no ALREADY-FILED and no OWNED-by marker).

Cause 1 of 1: StartQueryExecution's output-bucket verification collapses two distinct outcomes into one refusal — it refuses ANY OutputLocation bucket that is not already present in state, including an unclaimed name that Athena is required to create on the caller's behalf.
Failing check id: athena__glue__s3.athena-query-to-an-output-bucket-that-does-not-exist-is-rejected-until-it-is-created

  AWS:      Smithy (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): StartQueryExecution input member ResultConfiguration -> ResultConfiguration$OutputLocation ("The location in Amazon S3 where your query and calculation results are stored, such as s3://path/to/query/bucket/"); QueryString required @length 1-262144; modeled errors InternalServerException (httpError 500), InvalidRequestException (httpError 400), TooManyRequestsException (httpError 400). The model says nothing about bucket provisioning, so the submit-time outcome is recorded behavior only, and the recording shows TWO different outcomes that depend on whether the account can claim the bucket:
              (a) a bucket name that already belongs to another AWS account (so the caller can neither adopt nor create it) is refused synchronously, before any execution is recorded, with HTTP 400, Error Code InvalidRequestException and Message "Unable to verify/create output bucket <bucket>" — fixed text "Unable to verify/create output bucket " plus one variable part, the bucket name taken from the request's ResultConfiguration.OutputLocation s3:// URI. The workgroup's ListQueryExecutions is unchanged by the refused request.
              (b) a bucket name nobody has claimed — it simply does not exist — is ACCEPTED: Athena creates exactly that bucket in the caller's account, the execution settles State SUCCEEDED, and the result object lands at <prefix>/<QueryExecutionId>.csv inside the newly created bucket.
  Emulator: outcome (b) is served as outcome (a). StartQueryExecution on an OutputLocation naming an unclaimed, non-existent bucket is refused with HTTP 400 InvalidRequestException and Message "Unable to verify/create output bucket <bucket>" (the replay shows botocore.errorfactory.InvalidRequestException: An error occurred (InvalidRequestException) when calling the StartQueryExecution operation: Unable to verify/create output bucket <bucket>). No bucket is created, no execution id is minted, and no result object is ever written. The refusal wording itself matches AWS's foreign-bucket text, so the verification step exists — what is missing is its create half: "cannot be verified" and "does not exist yet" are answered identically.
  Why:      Letting Athena provision its own results bucket is a documented, widely used submission style (a workgroup or request OutputLocation pointed at a results bucket that the pipeline has not created itself). Against the emulator every such query is refused 400 at submit, so the client's whole query path dead-ends on what it reads as an unusable location, and it cannot distinguish the genuinely unusable foreign bucket (a request it must correct) from the absent bucket (a request AWS fulfils). Any workflow that then reads <prefix>/<QueryExecutionId>.csv sees nothing at all.
  Fix:      StartQueryExecution's handler/validation for ResultConfiguration.OutputLocation (services/service-com-amazonaws-athena-amazonathena/src/ops/start_query_execution.rs) and the S3 state it consults: the output-bucket verification must create an absent, unclaimed bucket in the caller's account and continue with the execution, reserving the InvalidRequestException refusal for a bucket that exists outside the account. Not covered by any filed issue: open wire issue 20261111T064644Z-athena-query-submission-and-ddl-execution-ignore-s3.md asks (cause 1) for a NON-S3 OutputLocation path to be refused, and fixed issue 20261109T192206Z-athena-2-net-new-causes-startqueryexecution-accepts-malformed.md added submit-time statement validation; neither describes the auto-provisioning half of the bucket check, which is this cause.
  Traces:   athena__glue__s3.athena-query-to-an-output-bucket-that-does-not-exist-is-rejected-until-it-is-created

## Evidence
Failing check athena__glue__s3.athena-query-to-an-output-bucket-that-does-not-exist-is-rejected-until-it-is-created [verdict: error].

The check's positive control (an existing, owned bucket) and its foreign-bucket negative (bucket name owned by another account -> InvalidRequestException "Unable to verify/create output bucket <foreign-bucket>") both pass; the failure is on the third, auto-provisioning leg.

Request: athena:StartQueryExecution(QueryString="SELECT 1", WorkGroup=<a workgroup created by the check>, ResultConfiguration={"OutputLocation": "s3://<unclaimed-bucket>/athena-results/"}) where <unclaimed-bucket> is a freshly generated name that exists nowhere.

Expected (AWS): HTTP 200 with a QueryExecutionId; the bucket is created by Athena; GetQueryExecution settles Status.State == "SUCCEEDED"; the object athena-results/<QueryExecutionId>.csv is listable in that bucket.

Actual (emulator): HTTP 400, Error.Code "InvalidRequestException", Error.Message "Unable to verify/create output bucket <unclaimed-bucket>" (the same text AWS reserves for a bucket the account cannot claim). Replay failure: botocore.errorfactory.InvalidRequestException: An error occurred (InvalidRequestException) when calling the StartQueryExecution operation: Unable to verify/create output bucket <unclaimed-bucket>. No execution is created and no result object is written.

## Steps to Reproduce
1. git_clone() the project and boot the emulator built from the checkout.
2. s3:CreateBucket <good-bucket>; athena:CreateWorkGroup(Name=<wg>) with no enforced result configuration.
3. Positive control: athena:StartQueryExecution(QueryString="SELECT 1", WorkGroup=<wg>, ResultConfiguration={OutputLocation: s3://<good-bucket>/athena-results/}) and poll GetQueryExecution to SUCCEEDED.
4. Negative control (passes today): athena:StartQueryExecution with OutputLocation s3://<a bucket owned by another account>/athena-results/ — observe 400 InvalidRequestException "Unable to verify/create output bucket <bucket>" and that ListQueryExecutions(WorkGroup=<wg>) is unchanged.
5. The divergence: generate a bucket name that does not exist anywhere and call athena:StartQueryExecution(QueryString="SELECT 1", WorkGroup=<wg>, ResultConfiguration={OutputLocation: s3://<absent-bucket>/athena-results/}).
6. Observe HTTP 400 InvalidRequestException "Unable to verify/create output bucket <absent-bucket>" instead of a 200 with a QueryExecutionId.
7. Observe that <absent-bucket> was never created and that no athena-results/<QueryExecutionId>.csv object exists, where AWS would have created the bucket and written the result there.
8. Re-replay with: athena__glue__s3.athena-query-to-an-output-bucket-that-does-not-exist-is-rejected-until-it-is-created

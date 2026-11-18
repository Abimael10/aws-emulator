Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__lambda__s3.obs-lambda-3-data-events-s3
Issue-Key: cloudtrail__lambda__s3.obs-lambda-3-data-events-s3|run#51|TimeoutError|raise TimeoutError(
Issue-Commit: f5587c3bdd7bfd24e9b7be5f07fe9dfba86ab287

# cloudtrail__lambda__s3: a successful Lambda GetFunction is never recorded as a CloudTrail management event, so LookupEvents never indexes it and the data-event/management-event separation barrier never clears

> _Note: this issue was identified against the code base at commit `f5587c3bdd7bfd24e9b7be5f07fe9dfba86ab287`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
LookupEvents

## Description
One net-new root cause, grounded in the only failing behavioral check under this heading that is neither ALREADY FILED nor OWNED by an open issue.

Cause 1 of 1: a successful Lambda GetFunction produces no CloudTrail management-event record, so CloudTrail LookupEvents answers HTTP 200 with an empty Events list for it forever and any poll that waits for that record to be indexed can never terminate. Failing check id: cloudtrail__lambda__s3.obs-lambda-3-data-events-s3.

AWS: CloudTrail LookupEvents (cloudtrail 2013-11-01, awsQuery, operation `LookupEvents`, @idempotent, paginated on NextToken/Events) "looks up management events ... captured by CloudTrail" for the Region the request is made in, filtered by one LookupAttribute — here AttributeKey=EventSource, AttributeValue=lambda.amazonaws.com. Every management call on a service is captured in event history whether or not a trail exists. The Lambda Smithy model (service AWSGirApiService, version 2015-03-31) declares awsService.cloudTrailEventSource = "lambda.amazonaws.com", and `GetFunction` (GET /2015-03-31/functions/{FunctionName}, success 200) carries the @readonly trait, so it is a read-only MANAGEMENT event. The requirement is therefore: after a successful GetFunction, CloudTrail event history holds a record with eventSource="lambda.amazonaws.com", eventCategory="Management", eventName="GetFunction20150331v2" (classic Lambda's eventName carries its per-operation REST URI date version suffix, which is what CloudTrail publishes for this operation), readOnly=true, no errorCode, awsRegion = the request's Region, recipientAccountId = the caller's account, a unique eventID, and requestParameters.functionName naming the function the request addressed. Recorded behavior for the surrounding contract: LookupEvents returns management events ONLY, so the Invoke DATA events the same check produced must never appear there — that half of the contract can only be checked once the GetFunction barrier record exists. Nothing about the function's generated name, the account id, eventID values or timestamps is part of the requirement; those are per-run values. The model states no error for this case: the diverging answer is a 200 with an empty result, not a refusal.

Emulator: CloudTrail's LookupEvents is implemented and reads a shared management-event store, but Lambda's GetFunction handler never writes into it — services/service-com-amazonaws-lambda-awsgirapiservice/src/ops/get_function.rs resolves the function out of `crate::state`, builds GetFunctionOutput and returns, with no management-event recorder referenced anywhere in the handler. So LookupEvents(LookupAttributes=[{EventSource: lambda.amazonaws.com}], StartTime=<window>) answers HTTP 200 with no record whose eventName is "GetFunction20150331v2" and whose requestParameters.functionName is the function just read, however long the caller pages and polls. The replayed check reaches its third leg (data events never enter management-event history), polls LookupEvents for that barrier record, and ends with `TimeoutError: the GetFunction barrier for '<function>' to be indexed: the run budget ended first; not graded` — the barrier predicate is never satisfiable, so the poll consumes whatever time is left and the Invoke-must-not-appear-in-event-history assertions that follow it are never evaluated. The check's two earlier legs passed in this replay, so the Invoke data-event delivery to S3 did occur; the only thing missing is the management-event record for the read.

Why: every audit, compliance and "who read this function" workflow reads a management call back out of CloudTrail event history, and every workflow that must distinguish a DATA event from a MANAGEMENT one uses a management-event record as its barrier before asserting the data events are absent. Against the emulator such a workflow blocks until its own deadline and then reports that the call was never audited — indistinguishable from a real missing-trail defect — and the guarantee this check exists to pin (LookupEvents serves management events only, never Invoke data events) becomes unverifiable, because the barrier that gates it can never clear.

Fix: the write path on the producing operation, not CloudTrail's reader. Lambda's GetFunction handler must record a management event into the shared management-event store CloudTrail's LookupEvents already reads, using the service's modeled cloudTrailEventSource ("lambda.amazonaws.com", taken from the Smithy trait rather than from the crate or service name), CloudTrail's eventName for the operation ("GetFunction20150331v2", i.e. the operation name plus its REST URI date version suffix), eventCategory=Management, readOnly=true because the operation is @readonly, the request's Region and the caller's account, a freshly minted unique eventID per call, and requestParameters holding only that call's own input members (functionName, and Qualifier when the request named one). The same treatment is owed to Lambda's other read-only management operations. CloudTrail's LookupEvents handler itself needs no change.

Related open issues that do NOT cover this cause (named so a fixer does not close them by mistake): 20261104T040654Z-cloudtrail-lambda-core-and-lambda-write-operations-are.md covers lambda-core CreateNetworkConnector and Lambda PublishLayerVersion, i.e. WRITE operations recorded with readOnly=false, and names neither GetFunction nor any read-only Lambda operation; 20261024T114720Z-cloudtrail-event-history-is-empty-for-everything-but.md predates and does not carry this check's replay key; 20261107T051039Z-cloudtrail-lambda-s3-a-trail-whose-advanced-event.md and 20261105T112044Z-cloudtrail-lambda-s3-the-lambda-invoke-response-carries.md concern Invoke DATA-event delivery and the Invoke request-id header, both of which this replay got past.

Traces: cloudtrail__lambda__s3.obs-lambda-3-data-events-s3. Code citation for the fixer: services/service-com-amazonaws-lambda-awsgirapiservice/src/ops/get_function.rs lines 154-261 (the whole handle body, no management-event recorder).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 1939/2549 passed (76.1%), 517 mismatched, 93 errored, bootstrap_signal false (the emulator answered calls throughout this check — IAM, Lambda, S3, CloudTrail and CloudWatch Logs all responded, and the check's first two legs passed).

### cloudtrail__lambda__s3
[error] cloudtrail__lambda__s3.obs-lambda-3-data-events-s3
  Error: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
  File "check_cloudtrail__lambda__s3_obs_lambda_3_data_events_s3.py", line 425, in run
    raise TimeoutError(
  TimeoutError: the GetFunction barrier for 'sim2real-xsvc-ctd-a-<generated>' to be indexed: the run budget ended first; not graded

The check (GROUP=eventing, SERVICES=[cloudtrail, lambda, s3]) creates two functions fnA/fnB with a no-permission execution role, a trail with an advanced event selector naming only fnA's ARN, invokes both, then verifies three legs in order:
  Leg 1 — fnA's Invoke data events are delivered to the trail's S3 bucket with the AWS record shape (eventSource lambda.amazonaws.com, eventCategory "Data", managementEvent false, readOnly false, requestParameters.functionName = fnA's unqualified ARN, additionalEventData.functionVersion = "<arn>:$LATEST"). PASSED in this replay.
  Leg 2 — no record names fnB. PASSED in this replay.
  Leg 3 — data events never enter management-event history. Its barrier is the GetFunction issued after the last invoke:
      lam.get_function(FunctionName=fn_a)   # the management-event barrier for the LookupEvents leg
      ... poll ct.lookup_events(LookupAttributes=[{"AttributeKey": "EventSource", "AttributeValue": "lambda.amazonaws.com"}], StartTime=start) every 2 s
          until any record has eventName == "GetFunction20150331v2" and requestParameters.functionName == fn_a
  and only once that record is indexed does it assert `leaked == []` (no Invoke record in LookupEvents) and that LookupEvents(EventName=Invoke) returns none of this check's invokes.

Expected (AWS): the first poll iteration (or one shortly after) returns an Event whose CloudTrailEvent parses to eventSource='lambda.amazonaws.com', eventCategory='Management', eventName='GetFunction20150331v2', readOnly=true, errorCode absent, awsRegion = the request Region, recipientAccountId = the caller's account, requestParameters.functionName = the function read.
Actual (emulator): LookupEvents answers HTTP 200 and never yields any record for that eventName/functionName pair, because Lambda's GetFunction handler writes nothing into the management-event store CloudTrail's LookupEvents reads. The barrier predicate is unsatisfiable, the poll runs until no budget remains, and the harness raises TimeoutError labelled "not graded" — so none of the management-events-only assertions in leg 3 is ever evaluated.
(The generated function names, account id, eventID values and timestamps in the recording are per-run values and are not part of the requirement; the diverging fact is that zero management-event records exist for a successful GetFunction.)

## Steps to Reproduce
1. git_clone, then run_behavioral_suite() and look at check cloudtrail__lambda__s3.obs-lambda-3-data-events-s3 under the `cloudtrail__lambda__s3` heading.
2. Or by hand against the booted emulator:
   a. Create an IAM role trusting lambda.amazonaws.com with no permission policy, and lambda CreateFunction a small python3.12 function; wait for it to be Active.
   b. Call lambda GetFunction (GET /2015-03-31/functions/<name>) and observe HTTP 200 — the read succeeds.
3. Call CloudTrail LookupEvents with LookupAttributes=[{"AttributeKey":"EventSource","AttributeValue":"lambda.amazonaws.com"}] and StartTime a few minutes before step 2b, paging NextToken to exhaustion. Expect at least one Event whose CloudTrailEvent has eventName='GetFunction20150331v2', eventCategory='Management', readOnly=true, no errorCode, awsRegion = the request Region, recipientAccountId = the caller's account, and requestParameters.functionName = the function from 2a. Observe that no such record is returned, however long you poll.
4. Repeat step 3 with LookupAttributes=[{"AttributeKey":"EventName","AttributeValue":"GetFunction20150331v2"}] and observe HTTP 200 with Events=[].
5. Because the GetFunction management record is never indexed, the contract this check exists to pin — that LookupEvents serves management events ONLY and never returns the Invoke data events produced in the same window — can never be reached: the barrier that gates those assertions does not clear, and the check ends in TimeoutError instead of evaluating them.

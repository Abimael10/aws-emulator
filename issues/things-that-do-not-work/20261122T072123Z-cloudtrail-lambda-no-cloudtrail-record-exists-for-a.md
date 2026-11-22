Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__lambda.obs-lambda-3-management-events
Issue-Key: cloudtrail__lambda.obs-lambda-3-management-events|run#32|Timeout|time.sleep(_POLL_INTERVAL_S)
Issue-Commit: 634fb37e35f51bdb636290ffeec5191f736cf22e

# cloudtrail__lambda: no CloudTrail record exists for a Lambda CreateFunction / AddPermission / refused GetFunction, so the URI-versioned eventNames, the package+environment redaction and the rejection's error fields are all unobservable (1 cause)

> _Note: this issue was identified against the code base at commit `634fb37e35f51bdb636290ffeec5191f736cf22e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
LookupEvents

## Description
Cause 1 of 1: lambda.amazonaws.com management events are never recorded, so CloudTrail returns nothing for a successful CreateFunction, a successful AddPermission or a refused GetFunction, and every per-record contract that rides on those records (versioned eventName, Resources summary, responseElements, deployment-package/environment redaction, errorCode/errorMessage/readOnly of the rejection) can never be observed.

AWS: recorded behavior only for the audit records (the CloudTrail record-contents reference and Lambda's "Logging Lambda API calls with CloudTrail" page document the members); the Smithy model fixes the API side. Lambda control-plane calls are recorded as management events with eventSource "lambda.amazonaws.com", eventCategory "Management", awsRegion equal to the calling region and recipientAccountId equal to the caller's account. The eventName is the URI-versioned form of the operation, not the bare operation name: CreateFunction is recorded as "CreateFunction20150331", AddPermission as "AddPermission20150331v2" and GetFunction as "GetFunction20150331v2" (the "20150331" stem is the operation's HTTP URI version — Smithy: CreateFunction http POST /2015-03-31/functions, AddPermission POST /2015-03-31/functions/{FunctionName}/policy, GetFunction GET /2015-03-31/functions/{FunctionName}). The successful CreateFunction record carries errorCode absent, readOnly false, a Resources entry {"ResourceType": "AWS::Lambda::Function", "ResourceName": <the function name from the request>} in the LookupEvents summary, EventSource "lambda.amazonaws.com" on the summary, and responseElements.functionArn / responseElements.codeSha256 equal to the FunctionArn and CodeSha256 the API returned (Smithy: CreateFunction output members FunctionArn, CodeSha256). The same record is REDACTED: requestParameters.code is recorded as an empty object (the Code.ZipFile bytes are stripped) and the environment variables appear nowhere in the record — neither a variable's key nor its value — even though the CreateFunction response itself echoes Environment.Variables to the caller (Smithy: CreateFunction output Environment -> EnvironmentResponse). The AddPermission record carries requestParameters.statementId and requestParameters.principal copied from the request, errorCode absent, readOnly false, and its own eventID distinct from the CreateFunction record's. A GetFunction on a name that does not exist is refused ResourceNotFoundException with HTTP 404 (Smithy: GetFunction errors include ResourceNotFoundException, httpError 404) and message of the fixed form "Function not found: " followed by the fully qualified function ARN the service resolved from the request (arn:aws:lambda:<region>:<account-id>:function:<name>; region, account and name are request/state values, the prefix text is fixed); its audit record carries readOnly true, errorCode "ResourceNotFoundException", errorMessage byte-identical to the message the caller received, responseElements absent, an empty Resources list (so only an EventSource lookup finds it) and the LookupEvents summary EventName "GetFunction20150331v2".

Emulator: the Lambda calls themselves are accepted (the role is created, the function reaches Active, AddPermission succeeds, the missing-name GetFunction is refused), but LookupEvents then returns NO lambda.amazonaws.com record at all: neither a ResourceName lookup on the function name nor an EventSource lookup on "lambda.amazonaws.com" ever yields CreateFunction20150331, AddPermission20150331v2 or GetFunction20150331v2, and the check is killed polling that loop ("Check timed out after 60.0s", the poll would otherwise run to its 420 s index budget and raise "within 420s CloudTrail must index 'CreateFunction20150331' and 'AddPermission20150331v2' ... actual eventNames for this resource []"). Because no record is produced, every downstream requirement — the versioned eventNames, eventCategory/awsRegion/recipientAccountId, the AWS::Lambda::Function resource summary, responseElements.functionArn and .codeSha256, the stripped requestParameters.code, the absence of the environment key and value and of the zip bytes, the AddPermission parameters, the distinct eventIDs, and the refused call's readOnly/errorCode/errorMessage/absent responseElements — is simultaneously unmet.

Why: a client that audits Lambda control-plane activity (drift detection on function configuration or resource policy, change review, incident response) sees an account in which no function was ever created and no permission was ever granted, and cannot distinguish "nothing happened" from "recording is dead". The redaction half is worse than absent: a consumer cannot verify that the deployment package and environment secrets are kept out of the audit trail, so a later implementation that leaks them would look the same as today.

Fix: the CloudTrail event-recording path for lambda.amazonaws.com management calls — the recorder that stamps the URI-versioned eventName and classification, the userIdentity/region/account attribution, the requestParameters serializer with its code/environment redaction rules, the responseElements serializer for CreateFunction, the error-field population for a refused call, and the resource-summary/LookupEvents index that must return these records under both ResourceName and EventSource lookup attributes.

Related but not covering this check: open issue 20261029T173328Z-cloudtrail-records-and-delivers-no-audit-record-for.md carries cloudtrail__lambda.obs-lambda-3-duplicate-permission-rejected-and-audited (Cause 7, the refused duplicate AddPermission) but does not carry this check's replay key, and it does not state the CreateFunction redaction/responseElements or GetFunction-rejection contracts reported here.

Traces: cloudtrail__lambda.obs-lambda-3-management-events

## Evidence
Replay report, suite simparity-behavioral-aws-v1, service heading `### cloudtrail__lambda`:

  [mismatch] cloudtrail__lambda.obs-lambda-3-management-events — Error: Check timed out after 60.0s

Not marked ALREADY FILED and not marked OWNED by any open issue.

Expected (AWS): within the check's index budget, LookupEvents returns three lambda.amazonaws.com management records —
  - eventName "CreateFunction20150331": eventCategory "Management", awsRegion = calling region, recipientAccountId = caller account, errorCode absent, readOnly false, summary EventSource "lambda.amazonaws.com" and Resources containing {"ResourceType": "AWS::Lambda::Function", "ResourceName": <function name>}, responseElements.functionArn = the API's FunctionArn, responseElements.codeSha256 = the API's CodeSha256, requestParameters.code = {} and no occurrence anywhere in the record of the environment variable key, the environment variable value, the zip bytes or their base64 form;
  - eventName "AddPermission20150331v2": requestParameters.statementId and requestParameters.principal as sent, errorCode absent, readOnly false, eventID different from the CreateFunction record's;
  - eventName "GetFunction20150331v2" for the refused call: readOnly true, errorCode "ResourceNotFoundException", errorMessage identical to the caller's message ("Function not found: arn:aws:lambda:<region>:<account>:function:<name>" — fixed prefix "Function not found: " plus the ARN built from request/state), responseElements absent, and summary EventName "GetFunction20150331v2".

Actual (emulator): all the exercised Lambda calls are served (CreateFunction echoes Environment.Variables, AddPermission succeeds, GetFunction on an absent name is refused), but LookupEvents yields zero lambda.amazonaws.com records for the whole polling window, under both LookupAttributes ResourceName=<function name> and EventSource=lambda.amazonaws.com. The check never leaves the indexing poll and is terminated at the 60 s harness cap; the assertion the budget would otherwise raise names the observed set of eventNames for the resource as empty and the failed-GetFunction record as not found.

## Steps to Reproduce
Against the booted emulator, in one region:
1. iam:CreateRole with a lambda.amazonaws.com assume-role policy and no permissions policy.
2. lambda:CreateFunction with Runtime python3.12, that role, Code.ZipFile = a small zip containing a unique marker string, and Environment.Variables = {<unique key>: <unique value>}; wait for State == Active. Confirm the response echoes the environment variable (positive control for the redaction legs).
3. lambda:AddPermission on that function with a unique StatementId, Action lambda:InvokeFunction, Principal s3.amazonaws.com, SourceAccount = caller account.
4. lambda:GetFunction on a name that does not exist; confirm ResourceNotFoundException / HTTP 404 and record the message.
5. Poll cloudtrail:LookupEvents with LookupAttributes [{ResourceName: <function name>}] and, separately, [{EventSource: lambda.amazonaws.com}], StartTime two minutes before step 2, paging NextToken to exhaustion.

Observed: step 5 returns no event whose requestParameters.functionName is either the created function or the missing name, for the whole budget — no CreateFunction20150331, AddPermission20150331v2 or GetFunction20150331v2 record exists, so none of the record-content, redaction or error-field assertions can be evaluated.

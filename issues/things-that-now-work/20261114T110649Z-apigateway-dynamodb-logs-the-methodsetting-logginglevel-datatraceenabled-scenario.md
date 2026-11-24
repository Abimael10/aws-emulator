Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__logs.deep-apigateway-1-apigw-methodsetting-logging-level-and-data-trace-gate-execution-logs
Issue-Key: apigateway__dynamodb__logs.deep-apigateway-1-apigw-methodsetting-logging-level-and-data-trace-gate-execution-logs|_until_logged#31|Timeout|fired.append(fire())
Issue-Commit: 47219b124a45386662a63382125b929260efea78

# apigateway__dynamodb__logs: the MethodSetting loggingLevel/dataTraceEnabled scenario still makes no progress — no execution-log event is delivered for OFF/ERROR/INFO/data-trace and the check is killed at the cap

> _Note: this issue was identified against the code base at commit `47219b124a45386662a63382125b929260efea78`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStage

## Description
Cause 1 of 1: a REST stage's per-method execution-logging controls produce no observable behavior, so the OFF/ERROR/INFO/INFO+dataTrace scenario never reaches a graded assertion and is terminated at the runner cap. Failing check: apigateway__dynamodb__logs.deep-apigateway-1-apigw-methodsetting-logging-level-and-data-trace-gate-execution-logs.

AWS: Smithy (apigateway): MethodSetting carries loggingLevel ('Specifies the logging level for this method ... Valid values are OFF, ERROR, and INFO') and dataTraceEnabled (@default false), both set with UpdateStage patchOperations on /*/*/logging/loglevel and /*/*/logging/dataTrace and echoed by GetStage under methodSettings['*/*']. Recorded behavior: once the account's cloudwatchRoleArn names a role holding AmazonAPIGatewayPushToCloudWatchLogs, execution logs are delivered to the group API-Gateway-Execution-Logs_<rest-api-id>/<stage-name>, every line prefixed '(<requestId>)' with the response's x-amzn-RequestId. At OFF nothing is delivered for either a successful or a failing request; at ERROR only the failing request is logged and only at error verbosity ('Execution failed due to configuration error' appears, 'Starting execution for request' does not); at INFO the successful request logs 'Starting execution for request'; with dataTraceEnabled=true the same request additionally logs 'Endpoint request body after transformations: ', 'Endpoint response body before transformations: ' and 'Method response body after transformations: '. Those prefixes are AWS's fixed text; the request id, marker and bodies are request/state values.

Emulator: the scenario makes no progress at all — 'Error: Check timed out after 60.0s'. Not one phase settles: no barrier for the deployed endpoints answering (200 on the DynamoDB AWS-integration resource, 500 'Execution failed due to configuration error' on the resource whose credentials role does not exist) nor any of the three MethodSetting flips landing events in API-Gateway-Execution-Logs_<api-id>/<stage> is ever satisfied, so no execution-log event is observed for any logging level.

Why: loggingLevel and dataTraceEnabled are the only switches a client has over REST execution logging, and that log group is where an operator diagnoses a 500 from a misconfigured integration. Accepted-but-inert switches mean OFF cannot be distinguished from INFO, a configuration-error failure leaves no trace, and dataTraceEnabled reveals no request/response bodies.

Fix: the API Gateway REST data plane's execution-logging path — the per-request logger that must honour the stage's effective MethodSetting (OFF/ERROR/INFO and dataTraceEnabled) and deliver '(<requestId>) ...' events to API-Gateway-Execution-Logs_<api-id>/<stage> once the account cloudwatchRoleArn is set — plus the UpdateStage/GetStage methodSettings state it reads. NOTE: the open issues 20261113T163938Z-apigateway-dynamodb-logs-a-rest-stage-s-methodsetting.md, 20261107T173512Z-apigateway-dynamodb-logs-method-setting-logginglevel-datatrace-never.md and 20261029T180026Z-apigateway-dynamodb-logs-api-gateway-method-setting-logginglevel-datatrace-does-not.md describe this same scenario, but the replay did not report this check as owned by any of them (their recorded keys were taken at an _until_logged timeout, while the check now dies at the whole-check cap), so it is filed here to keep the failing check attached to an open work item — fold it into whichever of those causes is still open.

Traces: the failing check id above.

## Evidence
Expected (AWS): with the account cloudwatchRoleArn set to a role holding arn:aws:iam::aws:policy/service-role/AmazonAPIGatewayPushToCloudWatchLogs, API-Gateway-Execution-Logs_<rest-api-id>/<stage> receives, per request id: nothing at OFF; only 'Execution failed due to configuration error' for the failing request at ERROR (and nothing for a successful one); 'Starting execution for request' at INFO with no data-trace lines; and with dataTraceEnabled=true additionally one 'Endpoint request body after transformations: ' line carrying the transformed integration request, one 'Endpoint response body before transformations: ' line carrying the backend response, and one 'Method response body after transformations: ' line equal to the body the client received.

Actual (emulator): 'Error: Check timed out after 60.0s' — no phase barrier is satisfied, i.e. no execution-log event is observed in API-Gateway-Execution-Logs_<api-id>/<stage> for any of OFF, ERROR, INFO or INFO+dataTrace, and the first endpoint-readiness assertions are never passed.

## Steps to Reproduce
1. CreateTable (hash key) and a role API Gateway may assume allowing dynamodb:UpdateItem on it.
2. CreateRestApi (REGIONAL); GET /count as an AWS integration to arn:aws:apigateway:<region>:dynamodb:action/UpdateItem with that role and a request template embedding $input.params('m'); GET /broken with the same integration but credentials naming a role that does not exist; add 200 method/integration responses; CreateDeployment to stage 'logs'.
3. Create a role holding AmazonAPIGatewayPushToCloudWatchLogs and UpdateAccount patch /cloudwatchRoleArn to it.
4. For each setting, UpdateStage patchOperations replace /*/*/logging/loglevel = OFF | ERROR | INFO and /*/*/logging/dataTrace = false | true; confirm GetStage echoes methodSettings['*/*']; GET both endpoints and FilterLogEvents on API-Gateway-Execution-Logs_<api-id>/logs, indexing by the '(<requestId>)' prefix.
Expected: the per-level delivery above. Actual: no events at all; the scenario never progresses and is killed at the runner cap.

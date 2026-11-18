Issue-Source: trace-sweep-divergence
Issue-Trace: acc-gateway-rate-limit-batch-and-update
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: acc-gateway-rate-limit-batch-and-update bedrock-agentcore-control-not-found-error-paths bedrock-agentcore-control-online-evaluation-config-not-found-and-double-delete bedrock-agentcore-control-online-evaluation-config-create-validation-errors
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|UpdateGatewayRateLimit|2xx->2xx|$.createdAt
Issue-Key: bedrock-agentcore-control|UpdateGatewayRateLimit|2xx->2xx|$.updatedAt
Issue-Key: bedrock-agentcore-control|UpdateGatewayRateLimit|4xx->4xx|$.message
Issue-Key: bedrock-agentcore-control|GetOnlineEvaluationConfig|4xx->4xx|$.message
Issue-Key: bedrock-agentcore-control|CreateOnlineEvaluationConfig|4xx->2xx|$.message
Issue-Key: bedrock-agentcore-control|CreateOnlineEvaluationConfig|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: bedrock-agentcore-control|CreateOnlineEvaluationConfig|4xx->2xx|status
Issue-Key: bedrock-agentcore-control|CreateOnlineEvaluationConfig|4xx->4xx|$.message
Issue-Key: bedrock-agentcore-control|CreateOnlineEvaluationConfig|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: bedrock-agentcore-control|CreateOnlineEvaluationConfig|4xx->4xx|status
Issue-Commit: e9549b1dd16f60dcc5781cad715703321b701533

# bedrock-agentcore-control: 5 net-new causes in gateway rate-limit and online-evaluation-config surfaces

> _Note: this issue was identified against the code base at commit `e9549b1dd16f60dcc5781cad715703321b701533`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateGatewayRateLimit

## Description
Five net-new wire divergences on the bedrock-agentcore-control gateway rate-limit and online-evaluation-config surfaces. They share one shape of fault: the emulator serializes modeled date-time members as raw epoch numbers, and its refusal paths either skip a validation AWS performs or answer with emulator-authored prose instead of the AWS message for that error class. There are FIVE distinct causes and all five must be closed.

Cause 1 of 5: UpdateGatewayRateLimit serializes createdAt/updatedAt as epoch numbers (fixture acc-gateway-rate-limit-batch-and-update, step task#7).
AWS: Smithy: UpdateGatewayRateLimitResponse.createdAt and .updatedAt both target DateTimestamp, required, with @timestampFormat "date-time" — so the JSON body carries an RFC3339/ISO-8601 date-time STRING (e.g. of the form '<YYYY-MM-DD>T<hh:mm:ss>.<frac>Z'), never a number. The same two members on GetGatewayRateLimitResponse carry the identical date-time trait.
Emulator: emits JSON numbers for both members (replay reports createdAt/updatedAt as a float value where a date-time string was expected).
Why: an AWS SDK deserializing a date-time-formatted member rejects or mis-parses a bare number, so every client that reads a rate limit's createdAt/updatedAt either errors or gets a wrong instant; code that formats or compares the timestamp breaks.
Fix: the gateway rate-limit response serializer for UpdateGatewayRateLimit (and the shared rate-limit representation it builds), which must render DateTimestamp members in the modeled date-time form rather than passing the stored epoch value through.
Traces: acc-gateway-rate-limit-batch-and-update#task#7.
Note: an open issue, 20261108T204036Z-bedrock-agentcore-control-policy-policy-engine-and-gateway-rate-limit-timestamps.md, and 20261110T043203Z-bedrock-agentcore-control-gateway-rate-limit-timestamps-are-epoch-numbers-updates.md, describe the epoch-number defect on Get/BatchPut/Create rate-limit paths; neither carries this UpdateGatewayRateLimit step's replay key, so it is filed here.

Cause 2 of 5: UpdateGatewayRateLimit's 404 ResourceNotFoundException message is emulator prose (fixture acc-gateway-rate-limit-batch-and-update, step task#11).
AWS: Smithy: UpdateGatewayRateLimit models ResourceNotFoundException with httpError 404. Recorded behavior for an unknown rateLimitId: status 404 with message 'Rate limit not found: <rateLimitId>'. Fixed text: 'Rate limit not found: '; variable part: the rateLimitId taken verbatim from the request path (the recording's '00000000-0000-0000-0000-000000000000' is only one run's value).
Emulator: status 404 but message "Failed to retrieve rate limit because it doesn't exist. Retry the request with a different resource identifier." — generic prose that names neither the member nor the requested id.
Why: the status matches, so a client reaches its error branch, but any handler that parses or logs the identifier out of the message (or asserts on AWS's wording) gets nothing usable; the refusal is also unactionable to a human reading a log.
Fix: the UpdateGatewayRateLimit handler's not-found path, which must raise ResourceNotFoundException with the AWS message template above interpolating the requested rate-limit id.
Traces: acc-gateway-rate-limit-batch-and-update#task#11.
Note: 20261118T170231Z-bedrock-agentcore-control-batchputgatewayratelimits-destroys-rate-limits-it-does-not-name.md covers the same wording defect on GetGatewayRateLimit; it does not carry this UpdateGatewayRateLimit step.

Cause 3 of 5: GetOnlineEvaluationConfig's 404 message leaks the requested id instead of AWS's fixed text (fixture bedrock-agentcore-control-not-found-error-paths, step task#3).
AWS: Smithy: GetOnlineEvaluationConfig models ResourceNotFoundException with httpError 404 and takes onlineEvaluationConfigId as an @httpLabel. Recorded behavior for an unknown id: status 404 with message 'Online evaluation configuration not found' — the whole message is fixed text with NO variable part; AWS does not echo the requested identifier for this error class.
Emulator: status 404 with message 'Online evaluation configuration with ID <id> does not exist', emulator-authored wording that additionally interpolates the caller's identifier.
Why: clients and test suites that match AWS's not-found wording for this operation fail to classify the refusal, and the emulator teaches a message contract AWS does not honor.
Fix: the GetOnlineEvaluationConfig handler's not-found path, which must raise ResourceNotFoundException with exactly the fixed AWS text and no interpolated id.
Traces: bedrock-agentcore-control-not-found-error-paths#task#3; also bedrock-agentcore-control-online-evaluation-config-not-found-and-double-delete#task#1.

Cause 4 of 5: CreateOnlineEvaluationConfig accepts a request that names neither evaluators nor insights (fixture bedrock-agentcore-control-online-evaluation-config-create-validation-errors, step task#1).
AWS: Smithy: CreateOnlineEvaluationConfigRequest has both evaluators (EvaluatorList, 0..25) and insights (InsightList, 0..10) as optional members, and models ValidationException at httpError 400; the mutual-exclusivity rule is recorded behavior only. Recorded behavior: status 400, x-amzn-errortype 'ValidationException', message 'Exactly one of evaluators or insights must be provided' — this message is entirely fixed text with no variable part.
Emulator: answers 202 with a created configuration (no message, no x-amzn-errortype header) and persists the resource.
Why: a client building an evaluation config with neither list — or with both — is told the configuration is valid and gets back an id for a config AWS would never have created, so the error surfaces only later against real AWS; worse, the emulator's state now holds a config that cannot exist, which skews every subsequent list/get in the same session.
Fix: CreateOnlineEvaluationConfig's input validation, which must refuse with ValidationException (400) and the fixed message above unless exactly one of evaluators / insights is supplied.
Traces: bedrock-agentcore-control-online-evaluation-config-create-validation-errors#task#1.

Cause 5 of 5: CreateOnlineEvaluationConfig never validates evaluationExecutionRoleArn, so an unassumable role is not refused (fixture bedrock-agentcore-control-online-evaluation-config-create-validation-errors, step task#2).
AWS: Smithy: CreateOnlineEvaluationConfigRequest.evaluationExecutionRoleArn is required, targets RoleArn with pattern '^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$', and the operation models ValidationException at httpError 400 (ConflictException 409 also exists but is a different class). Recorded behavior: a syntactically valid role the service cannot assume is refused with status 400, x-amzn-errortype 'ValidationException', message 'The provided execution role cannot be assumed. Please verify the role exists and its trust policy allows this service to assume it.' — all fixed text, no variable part.
Emulator: answers 409 with x-amzn-errortype 'ConflictException' and message 'Online evaluation configuration with same name already exists'. Hypothesis: because Cause 4 let the preceding create succeed, the emulator reaches its duplicate-name check and never performs role validation at all; the role is accepted whenever the name is free.
Why: the client receives the wrong HTTP status and the wrong error code, so retry/branching logic keyed on ConflictException vs ValidationException takes the wrong path, and a misconfigured execution role is silently accepted — the evaluation config is created and only fails invisibly at execution time.
Fix: CreateOnlineEvaluationConfig's input validation must assume-check evaluationExecutionRoleArn and refuse with ValidationException (400) and the fixed message above, ordered ahead of the duplicate-name conflict check.
Traces: bedrock-agentcore-control-online-evaluation-config-create-validation-errors#task#2; also bedrock-agentcore-control-online-evaluation-config-create-validation-errors#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 (58 fixtures, 298 steps, 215 passed, score 72.1%). The five clusters below carried no stub_path and no owning issue.

Cause 1 — UpdateGatewayRateLimit, acc-gateway-rate-limit-batch-and-update#task#7, verdict mismatch, status 200 expected / 200 actual:
  $.createdAt: expected a date-time string ('2026-11-03T09:45:45.701543537Z' in this run), got 1795029064.8641832
  $.updatedAt: expected a date-time string ('2026-11-03T09:45:45.882547498Z' in this run), got 1795029064.8791876

Cause 2 — UpdateGatewayRateLimit, acc-gateway-rate-limit-batch-and-update#task#11, status 404 expected / 404 actual:
  $.message: expected 'Rate limit not found: <rateLimitId>' (recorded id 00000000-0000-0000-0000-000000000000), got "Failed to retrieve rate limit because it doesn't exist. Retry the request with a different resource identifier."

Cause 3 — GetOnlineEvaluationConfig, bedrock-agentcore-control-not-found-error-paths#task#3, status 404 expected / 404 actual (affects 2 steps):
  $.message: expected 'Online evaluation configuration not found', got 'Online evaluation configuration with ID <id> does not exist' (recorded id probenofakeeval-abcdefghij)

Cause 4 — CreateOnlineEvaluationConfig, bedrock-agentcore-control-online-evaluation-config-create-validation-errors#task#1, status expected 400, got 202:
  $.message: expected 'Exactly one of evaluators or insights must be provided', got None
  $headers.x-amzn-errortype: expected 'ValidationException', got None

Cause 5 — CreateOnlineEvaluationConfig, bedrock-agentcore-control-online-evaluation-config-create-validation-errors#task#2, status expected 400, got 409 (affects 2 steps):
  $.message: expected 'The provided execution role cannot be assumed. Please verify the role exists and its trust policy allows this service to assume it.', got 'Online evaluation configuration with same name already exists'
  $headers.x-amzn-errortype: expected 'ValidationException', got 'ConflictException'

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the simparity-wire-aws-v1 wire corpus.
3. Cause 1: replay fixture acc-gateway-rate-limit-batch-and-update to step task#7 — create a gateway, create a rate limit on it, then PATCH /gateways/{gatewayIdentifier}/rate-limits/{rateLimitId} (UpdateGatewayRateLimit) and inspect createdAt/updatedAt in the 200 body: they are JSON numbers, not modeled date-time strings.
4. Cause 2: same fixture, step task#11 — call UpdateGatewayRateLimit on the same gateway with a rateLimitId that does not exist; the 404 body's message is the emulator's generic prose instead of 'Rate limit not found: <rateLimitId>'.
5. Cause 3: replay fixture bedrock-agentcore-control-not-found-error-paths to step task#3 — GET /online-evaluation-configs/{onlineEvaluationConfigId} for an id that was never created; the 404 message interpolates the id instead of being AWS's fixed 'Online evaluation configuration not found'.
6. Cause 4: replay fixture bedrock-agentcore-control-online-evaluation-config-create-validation-errors step task#1 — POST /online-evaluation-configs/create with required name, rule, dataSourceConfig, evaluationExecutionRoleArn and enableOnCreate but with NEITHER evaluators nor insights; the emulator answers 202 and creates the config instead of 400 ValidationException.
7. Cause 5: same fixture, step task#2 — POST /online-evaluation-configs/create with a syntactically valid evaluationExecutionRoleArn that cannot be assumed; the emulator answers 409 ConflictException (duplicate name) instead of 400 ValidationException about the unassumable execution role. Step task#3 repeats it.

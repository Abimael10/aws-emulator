Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-control-agentcore-traj-runtime-lookup-from-log-group-name
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-control-agentcore-traj-runtime-lookup-from-log-group-name
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|GetAgentRuntime|4xx->4xx|$.message
Issue-Key: bedrock-agentcore-control|GetAgentRuntime|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: bedrock-agentcore-control|GetAgentRuntime|4xx->4xx|status
Issue-Commit: 70ff1500c355ed7fc4788da90f349959a442a9b5

# bedrock-agentcore-control GetAgentRuntime answers ValidationException where AWS refuses the unauthorized caller with AccessDeniedException

> _Note: this issue was identified against the code base at commit `70ff1500c355ed7fc4788da90f349959a442a9b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAgentRuntime

## Description
Cause 1 of 1: GetAgentRuntime validates the agentRuntimeId path label before it authorizes the caller, so a call that AWS refuses as unauthorized comes back as a malformed-input error.

AWS: Smithy GetAgentRuntime (GET /runtimes/{agentRuntimeId}/) binds agentRuntimeId as a required @httpLabel with pattern ^[a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}$ and models both AccessDeniedException (httpError 403) and ValidationException (httpError 400). The recorded response for a caller that holds no bedrock-agentcore permission is HTTP 403 with x-amzn-errortype: AccessDeniedException and message "User: <caller principal ARN> is not authorized to perform: bedrock-agentcore:GetAgentRuntime" — authorization is evaluated before the path label is pattern-checked, so a non-conforming agentRuntimeId (here one derived from a vended log-group name, which contains no '-<10 chars>' suffix) still produces the access-denied refusal, not a validation error. In that message the fixed text is "User: " ... " is not authorized to perform: bedrock-agentcore:GetAgentRuntime"; the only variable part is the principal ARN of the signing identity taken from the request's credentials (recorded behavior only for the ordering of the two checks — the model lists both errors without stating precedence).

Emulator: the same request is answered HTTP 400 with x-amzn-errortype: ValidationException and message "1 validation error detected: Value '<id>' at 'agentRuntimeId' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}" — the pattern check runs first and no authorization decision is ever reported.

Why: a client that reads the error code to tell "my credentials lack this action" from "my input is malformed" is told the wrong thing: it sees a ValidationException and retries with a reshaped identifier forever instead of attaching the bedrock-agentcore:GetAgentRuntime permission. Tooling that walks from a CloudWatch log-group name back to a runtime (the recorded flow) cannot distinguish an id it must normalize from a permission it must be granted, and any test asserting the 403 refusal passes against AWS and fails here.

Fix: the GetAgentRuntime handler in the bedrock-agentcore-control service — run the request's authorization decision before the input/path-label validation stage, and emit AccessDeniedException (403) with AWS's wording when the caller is not authorized. Related but not covering: the open issues "bedrock-agentcore-control ListAgentRuntimes is not authorized" (20261104T032753Z-...) and "ListAgentRuntimes succeeds for an assumed-role session the identity policy denies" (20261109T143515Z-...) are behavioral findings about ListAgentRuntimes performing no authorization at all; neither carries this GetAgentRuntime wire step or its check-ordering defect.

Traces: bedrock-agentcore-control-agentcore-traj-runtime-lookup-from-log-group-name#task#4 (representative) and #task#5 in the same cluster.

## Evidence
Replay cluster service=bedrock-agentcore-control|GetAgentRuntime|status=403->400.
Expected (recorded AWS): status 403, header x-amzn-errortype: AccessDeniedException, body message "User: arn:aws:iam::000000000000:user/emulator is not authorized to perform: bedrock-agentcore:GetAgentRuntime".
Actual (emulator): status 400, header x-amzn-errortype: ValidationException, body message "1 validation error detected: Value 'sim2real_probe_agmcp_ed5cafbc' at 'agentRuntimeId' failed to satisfy constraint: Member must satisfy regular expression pattern: [a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}".
Diverging keys: status, $headers.x-amzn-errortype, $.message. Affected steps: task#4 and task#5 of fixture bedrock-agentcore-control-agentcore-traj-runtime-lookup-from-log-group-name. The principal ARN in the expected message is the caller identity from the request credentials; the id in the actual message is the request's agentRuntimeId.

## Steps to Reproduce
Replay fixture bedrock-agentcore-control-agentcore-traj-runtime-lookup-from-log-group-name, step task#4 (and task#5): using credentials that are not authorized for bedrock-agentcore:GetAgentRuntime, call GetAgentRuntime with an agentRuntimeId recovered from a vended log-group name (a value that does not match ^[a-zA-Z][a-zA-Z0-9_]{0,99}-[a-zA-Z0-9]{10}$). AWS answers 403 AccessDeniedException naming the principal and bedrock-agentcore:GetAgentRuntime; the emulator answers 400 ValidationException about the agentRuntimeId pattern.

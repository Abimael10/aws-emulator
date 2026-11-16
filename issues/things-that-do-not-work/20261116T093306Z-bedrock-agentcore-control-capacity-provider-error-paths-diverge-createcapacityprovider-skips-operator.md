Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-control-capacity-provider-errors
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-control-capacity-provider-errors
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|CreateCapacityProvider|4xx->2xx|$.fieldList
Issue-Key: bedrock-agentcore-control|CreateCapacityProvider|4xx->2xx|$.message
Issue-Key: bedrock-agentcore-control|CreateCapacityProvider|4xx->2xx|$.reason
Issue-Key: bedrock-agentcore-control|CreateCapacityProvider|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: bedrock-agentcore-control|CreateCapacityProvider|4xx->2xx|status
Issue-Key: bedrock-agentcore-control|GetCapacityProvider|4xx->4xx|$.message
Issue-Commit: 47cc4ef4df4e13f0c580047fd2f3a66085f63081

# Capacity-provider error paths diverge: CreateCapacityProvider skips operator-role validation and GetCapacityProvider's not-found message is emulator-worded

> _Note: this issue was identified against the code base at commit `47cc4ef4df4e13f0c580047fd2f3a66085f63081`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCapacityProvider

## Description
Two net-new root causes on the bedrock-agentcore-control capacity-provider error surface. Both are reached through the same fixture (bedrock-agentcore-control-capacity-provider-errors) and both concern a refusal the emulator either never raises or words itself.

Cause 1 of 2: CreateCapacityProvider never validates the operator role named in permissionsConfiguration, so a request AWS refuses is accepted.

AWS: Smithy (com.amazonaws.bedrockagentcorecontrol#CreateCapacityProvider, PUT /capacity-providers, success 202) models permissionsConfiguration as REQUIRED, targeting PermissionsConfiguration whose single required member capacityProviderOperatorRoleArn is a RoleArn (@length 1..2048, @pattern ^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$) and is documented as "The Amazon Resource Name (ARN) of the IAM role that AgentCore assumes to manage the capacity provider, including launching, tagging, and terminating instances and their network interfaces." ValidationException (HTTP 400) is modeled for the operation, with required members message and reason (ValidationExceptionReason enum, value FieldValidationFailed) plus the optional fieldList of ValidationExceptionField {name, message}. Because the service must ASSUME that role to manage EC2 capacity on the caller's behalf, AWS performs the assume-role check synchronously at create time: the recorded response refuses the create before any provider is minted, with HTTP 400, error code ValidationException, reason "FieldValidationFailed", message "Role validation failed for the operator role. Please verify that the role exists and its trust policy allows assumption by this service", and a fieldList whose single entry repeats that message under a name pointing at the offending compute/permissions path. The message is entirely FIXED text for this error class — it names no role, account or request value — and the only variable part of the response is the fieldList entry's `name`, which is the request-relative path of the field AWS attributes the failure to (in the recording "/computeConfiguration/ec2Configuration/vpcConfiguration/subnets").

Emulator: the create is accepted. Status 202 with a normal CreateCapacityProvider body; no `message`, no `reason`, no `fieldList` and no x-amzn-errortype header are emitted at all (the replay reports each as "missing"). The operator role is never resolved and its trust policy is never consulted, so an ARN that no assumable role backs provisions a capacity provider that reaches a healthy status.

Why: a client relying on AWS's behavior uses this 400 as the signal that its operator role or trust policy is wrong, and fixes the role before anything is provisioned. Against the emulator the misconfigured create SUCCEEDS, so the error is deferred or lost entirely: automation that asserts on the ValidationException (IaC pre-flight checks, role-bootstrap retries, setup wizards that re-create the trust policy on a FieldValidationFailed) takes the success branch and proceeds to use a provider whose role can never launch an instance. Clients also lose the fieldList, which is how they point a user at the specific field to correct.

Fix: the CreateCapacityProvider handler's request validation — add the operator-role precondition check (resolve permissionsConfiguration.capacityProviderOperatorRoleArn and verify it exists and that its trust policy permits assumption by this service) BEFORE the provider is minted, and refuse with a modeled ValidationException carrying reason FieldValidationFailed, the fixed message text above, and a fieldList entry for the attributed field. The ValidationException serializer must emit message, reason and fieldList together with the x-amzn-errortype header.

Cause 2 of 2: GetCapacityProvider's not-found refusal message is emulator-authored prose instead of AWS's wording.

AWS: Smithy models GetCapacityProvider (GET /capacity-providers/{capacityProviderId}, success 200) with ResourceNotFoundException (HTTP 404, member `message`, documented "This exception is thrown when a resource referenced by the operation does not exist"). The status and error code are correct in the emulator; only the message text diverges. Recorded behavior only for the exact text: AWS answers 404 ResourceNotFoundException with message "No capacity provider found with id <capacityProviderId>". The fixed text is "No capacity provider found with id " and the single variable part is the capacityProviderId taken verbatim from the request path label.

Emulator: status 404 and error code ResourceNotFoundException are right, but the message reads "Capacity provider with ID <capacityProviderId> does not exist" — a different sentence, differently capitalized ("ID" vs "id"), with the id in a different position and the lead-in and trailing clause both rewritten.

Why: the message is part of the contract for this error class. A client relying on AWS's behavior that matches, logs or surfaces the refusal text — error-message assertions in tests, support tooling that greps the message, or UI that renders it to a user — sees text real AWS never returns, so the mismatch is only ever discovered against the real service. The existing open issues 20261109T033342Z-bedrock-agentcore-control-getpaymentmanager-not-found-message-uses.md and 20261029T212239Z-bedrock-agentcore-control-refusal-messages-diverge-from-aws.md record the same class of defect on other resources, but neither covers GetCapacityProvider, so this is a distinct cause.

Fix: the GetCapacityProvider handler's not-found path — emit the ResourceNotFoundException message as "No capacity provider found with id {capacityProviderId}", interpolating the requested id and leaving the surrounding text fixed. Status 404 and the ResourceNotFoundException code already match and must not change.

Traces: Cause 1 — bedrock-agentcore-control-capacity-provider-errors#task#1 (CreateCapacityProvider, expected 400, got 202). Cause 2 — bedrock-agentcore-control-capacity-provider-errors#task#2 (GetCapacityProvider, 404 both sides, $.message value_mismatch).

## Evidence
Cause 1 — fixture bedrock-agentcore-control-capacity-provider-errors, step task#1, operation CreateCapacityProvider, verdict mismatch:
  status: expected 400, got 202
  $.message: missing (expected 'Role validation failed for the operator role. Please verify that the role exists and its trust policy allows assumption by this service', got None)
  $.reason: missing (expected 'FieldValidationFailed', got None)
  $.fieldList: missing (expected [{'name': '/computeConfiguration/ec2Configuration/vpcConfiguration/subnets', 'message': 'Role validation failed for the operator role. Please verify that the role exists and its trust policy allows assumption by this service'}], got None)
  $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)
Expected (AWS): 400 ValidationException, reason=FieldValidationFailed, the fixed message above, and a fieldList entry naming the attributed field. Actual (emulator): 202 with a successful create body and no error members whatsoever.
Model evidence (smithy_query): CreateCapacityProvider errors include ValidationException (httpError 400); permissionsConfiguration is required and PermissionsConfiguration.capacityProviderOperatorRoleArn is a required RoleArn (@pattern ^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$) that "AgentCore assumes to manage the capacity provider"; ValidationException requires {message, reason} and may carry fieldList of ValidationExceptionField{name,message}; ValidationExceptionReason includes FieldValidationFailed.

Cause 2 — fixture bedrock-agentcore-control-capacity-provider-errors, step task#2, operation GetCapacityProvider, verdict mismatch (status 404 expected and returned):
  $.message: value_mismatch (expected 'No capacity provider found with id sim2realnonexistentcp-0000000000', got 'Capacity provider with ID sim2realnonexistentcp-0000000000 does not exist')
Expected (AWS) message template: fixed "No capacity provider found with id " + the capacityProviderId from the request path. Actual (emulator) template: "Capacity provider with ID " + id + " does not exist". The id shown is the fixture's probe value, not part of the requirement.
Model evidence (smithy_query): GetCapacityProvider errors include ResourceNotFoundException (httpError 404) with a single `message` member; capacityProviderId is an @httpLabel required CapacityProviderId (@pattern ^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$), which the probe id satisfies, so this is a genuine not-found and not a constraint refusal.

## Steps to Reproduce
Cause 1 (fixture bedrock-agentcore-control-capacity-provider-errors, step task#1):
1. Call CreateCapacityProvider (PUT /capacity-providers) with a pattern-valid `name`, a required `computeConfiguration` (ec2Configuration with a vpcConfiguration), and a permissionsConfiguration.capacityProviderOperatorRoleArn that is ARN-shaped but names a role that does not exist / whose trust policy does not allow this service to assume it.
2. AWS: the call is refused with HTTP 400, x-amzn-errortype ValidationException, reason "FieldValidationFailed", message "Role validation failed for the operator role. Please verify that the role exists and its trust policy allows assumption by this service", and a fieldList entry repeating that message against the attributed field path. No capacity provider is created.
3. Emulator: the call returns HTTP 202 with a normal create body (capacityProviderId/capacityProviderArn/name/status) and no error members at all; the provider exists afterwards.

Cause 2 (same fixture, step task#2):
1. Call GetCapacityProvider (GET /capacity-providers/{capacityProviderId}) with a well-formed capacityProviderId that matches ^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$ but names no existing provider.
2. AWS: HTTP 404 ResourceNotFoundException with message "No capacity provider found with id <that id>".
3. Emulator: HTTP 404 ResourceNotFoundException (correct) but message "Capacity provider with ID <that id> does not exist".

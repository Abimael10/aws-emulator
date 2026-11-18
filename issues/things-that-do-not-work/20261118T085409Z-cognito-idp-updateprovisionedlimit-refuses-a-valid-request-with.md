Issue-Source: trace-sweep-divergence
Issue-Trace: cognito-idp-provisioned-limit-round-trip
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cognito-idp-provisioned-limit-round-trip
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|UpdateProvisionedLimit|2xx->4xx|$.Limit
Issue-Key: cognito-idp|UpdateProvisionedLimit|2xx->4xx|status
Issue-Commit: a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50

# cognito-idp UpdateProvisionedLimit refuses a valid request with HTTP 400 and returns no Limit

> _Note: this issue was identified against the code base at commit `a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateProvisionedLimit

## Description
1 cause.

Cause 1 of 1: UpdateProvisionedLimit answers HTTP 400 for a request AWS accepts, so the operation's required output never arrives.
AWS: Smithy: UpdateProvisionedLimit takes LimitDefinition (LimitDefinitionType, required: LimitClass enum + Attributes string map, both required) and RequestedLimitValue (IntegerType, required) and returns Limit (LimitType, REQUIRED: LimitDefinition, ProvisionedLimitValue and FreeLimitValue all required); its modeled errors are InvalidParameterException (400), NotAuthorizedException (403), ResourceNotFoundException (404), ServiceQuotaExceededException (400), TooManyRequestsException (429) and InternalErrorException (500). Recorded behavior: provisioning an in-range value for LimitClass API_CATEGORY with Attributes {Category: UserRead} SUCCEEDS with HTTP 200 and Limit {LimitDefinition {LimitClass API_CATEGORY, Attributes {Category: UserRead}}, ProvisionedLimitValue = the requested value, FreeLimitValue = the category's free rate} — the recorded step shows requested, provisioned and free all equal to 120, i.e. writing back the value the service itself reports is an ordinary accepted write, not a rejected no-op. The only refusal AWS makes for an in-range/over-range value on this operation is ServiceQuotaExceededException for a value beyond the account ceiling.
Emulator: HTTP 400 and no Limit member in the body, so the round trip cannot proceed past this call. The sibling open issue below records the emulator's refusal for this condition as InvalidParameterException with the message 'Requested value is the same as the current provisioned limit' (a whole-sentence emulator-authored text that names no request value); AWS produces no such refusal.
Why: a client that reads the current limit and applies it — the idempotent write that configuration tooling, retries and reconciliation loops all perform — gets a hard 400 instead of the Limit structure, so the provisioned limit can never be written from the service's own reported state and an over-quota request can no longer be distinguished from a benign one.
Fix: the same-value / no-op gate in the UpdateProvisionedLimit handler in the cognito-idp crate (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice) — an in-range RequestedLimitValue must be stored and echoed back in the required Limit output; only the over-ceiling case belongs to ServiceQuotaExceededException.
Traces: cognito-idp-provisioned-limit-round-trip#task#2.

Related open issue 20261118T042515Z-cognito-idp-updateprovisionedlimit-now-refuses-re-applying-the.md reports this refusal from the behavioral corpus (check cognito-idp.update-provisioned-limit-out-of-range-value-rejected) and carries no wire replay key, so this fixture step is not covered by it and must be re-replayed separately.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture cognito-idp-provisioned-limit-round-trip:

task#2 UpdateProvisionedLimit, expected 200 got 400:
  status: expected 200, got 400
  $.Limit: missing (expected {'FreeLimitValue': 120, 'LimitDefinition': {'Attributes': {'Category': 'UserRead'}, 'LimitClass': 'API_CATEGORY'}, 'ProvisionedLimitValue': 120}, got None)

Smithy (com.amazonaws.cognitoidentityprovider#UpdateProvisionedLimit): input LimitDefinition (required) + RequestedLimitValue (required integer); output Limit (required), LimitType with LimitDefinition / ProvisionedLimitValue / FreeLimitValue all required; errors InvalidParameterException 400, NotAuthorizedException 403, ResourceNotFoundException 404, ServiceQuotaExceededException 400, TooManyRequestsException 429, InternalErrorException 500.

## Steps to Reproduce
1. Replay fixture cognito-idp-provisioned-limit-round-trip.
2. Step task#2: UpdateProvisionedLimit with LimitDefinition={"LimitClass": "API_CATEGORY", "Attributes": {"Category": "UserRead"}} and an in-range RequestedLimitValue (the value the preceding read reports) -> expect HTTP 200 with Limit carrying that LimitDefinition, ProvisionedLimitValue equal to the requested value and the category's FreeLimitValue; the emulator answers HTTP 400 with no Limit.

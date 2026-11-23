Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.get-effective-policies-reflects-principal-attachment
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.get-effective-policies-reflects-principal-attachment|_error#41|AssertionError|raise AssertionError(f"expected a ClientError, but the call succeeded: {result!r}")
Issue-Commit: b399d56cced4737504d09473e3522631a8c4fa9f

# iot GetEffectivePolicies answers 200 with an empty list for a cognitoIdentityPoolId that does not exist, instead of 404 ResourceNotFoundException "IdentityPool '<pool-id>' not found."

> _Note: this issue was identified against the code base at commit `b399d56cced4737504d09473e3522631a8c4fa9f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetEffectivePolicies

## Description
One net-new root cause in the iot service. The failing behavioral check iot.get-effective-policies-reflects-principal-attachment is marked neither ALREADY FILED nor OWNED by the replay report: its earlier causes (the thingName-only / no-argument identity refusal) now pass, and the check advances to — and fails on — the Cognito identity-pool identity, which no filed issue carries. ONE cause must be closed before submitting.

Cause 1 of 1: GetEffectivePolicies accepts a cognitoIdentityPoolId that names no identity pool and answers a successful, empty authorization result instead of refusing the read.

AWS: Smithy — GetEffectivePolicies (POST /effective-policies, success 200) takes principal, cognitoIdentityPoolId and thingName as three optional members and models ResourceNotFoundException @httpError 404 (alongside InvalidRequestException @httpError 400). Recorded behavior: cognitoIdentityPoolId is one of the two acceptable IDENTITIES for this operation and it is REALLY RESOLVED, not merely parsed — a syntactically valid pool id in the <region>:<uuid> form that names no identity pool is refused with HTTP 404, error code ResourceNotFoundException and the message "IdentityPool '<region>:<pool-id>' not found." The fixed text is "IdentityPool '" ... "' not found." (with the trailing period); the single variable part is the pool id echoed verbatim from the request's cognitoIdentityPoolId. No concrete region or uuid is part of the requirement — one run's value only fills the template.

Emulator: the request is answered HTTP 200 with body {"effectivePolicies": []} — no refusal at all. services/service-com-amazonaws-iot-awsiotservice/src/ops/get_effective_policies.rs resolves the pool id no further than its FORM: lines 233-248 match crate::state::parse_policy_target(pool_id), push the Cognito-identity key into the attachment scope when the form parses, and refuse (400 InvalidRequestException) only when the value is not even shaped like a pool id. Existence is never consulted, so any well-formed pool id falls through to the empty-scope projection and serves a success.

Why: this is an authorization answer that is wrong in the direction that hides the error. A caller asking "which policies govern this Cognito identity pool?" with a wrong, stale or typo'd pool id is told, with HTTP 200, that the pool is governed by NOTHING — indistinguishable from a real pool with no attachments. An audit concludes the identities are ungoverned (or that a policy attachment did not take) and nothing signals the pool does not exist; a client that branches on ResourceNotFoundException to retry or re-resolve the pool never takes that branch. It also makes the two identity members behave inconsistently: a principal naming a resource this account does not hold IS refused 404 by the same handler (lines 212-231, via resolve_policy_target's NotFound), while the pool id is not.

Fix: the GetEffectivePolicies handler's cognitoIdentityPoolId resolution — the pool-id branch must resolve the named identity pool against state and refuse the modeled ResourceNotFoundException (HTTP 404) with that fixed text and the request's pool id, instead of treating a parsed form as a resolved identity and projecting an empty effectivePolicies list. The form-only 400 refusal for a value that is not a pool id at all stays as it is.

Related issues that do NOT cover this cause: the fixed issue 20261113T063920Z-iot-geteffectivepolicies-diverges-on-5-causes-identity-validation.md covers this handler's identity-member validation, non-ARN principal classification, thing-miss wording and default-document projection, and carries no key for the pool-existence read; the open issue 20261119T032013Z-iot-7-net-new-causes-the-published-iot.md (Cause 2) covers the thingName-only identity refusal of this same check, which now passes. This finding is not a reopening of either: the check fails on an unexpected SUCCESS, not on a refusal left in place by a fix.

Traces: iot.get-effective-policies-reflects-principal-attachment.

## Evidence
Replay of suite simparity-behavioral-aws-v1 (232/351 passed). Failing check iot.get-effective-policies-reflects-principal-attachment, verdict mismatch, in its _error helper (the helper that turns an expected refusal into (Error.Code, Error.Message) and raises when the call instead succeeds):

  File "check_iot_get_effective_policies_reflects_principal_attachment.py", line 218, in run
    cognito = _error(
  File "check_iot_get_effective_policies_reflects_principal_attachment.py", line 48, in _error
    raise AssertionError(f"expected a ClientError, but the call succeeded: {result!r}")
  AssertionError: expected a ClientError, but the call succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/json', 'content-length': '24', ...}, 'RetryAttempts': 0}, 'effectivePolicies': []}

Call under test: iot.get_effective_policies(cognitoIdentityPoolId="<region>:00000000-0000-0000-0000-000000000000") — no principal, no thingName.
  Expected: HTTP 404, Error.Code "ResourceNotFoundException", Error.Message "IdentityPool '<region>:00000000-0000-0000-0000-000000000000' not found." (the check asserts both the code and the exact message, the pool id being the request's own value).
  Actual:   HTTP 200, body {"effectivePolicies": []}, no error envelope.

Everything the check asserts before this point passed against this build, which narrows the cause to the pool-id branch alone: a principal with no attachment resolves to [], an attached policy resolves to exactly itself with its own ARN and default-version document, a second certificate resolves to exactly its own policy, and get_effective_policies(thingName=<an existing attached thing>) and get_effective_policies() with no arguments are both already refused 400 InvalidRequestException "Both principal and CognitoIdentityPoolId can not be null."

Emulator code backing the diagnosis: services/service-com-amazonaws-iot-awsiotservice/src/ops/get_effective_policies.rs lines 233-248 — the cognitoIdentityPoolId branch matches crate::state::parse_policy_target(pool_id), pushes PolicyTarget::CognitoIdentity(..).key() into `scope` on a successful parse and otherwise returns invalid_request(invalid_cognito_identity_pool_id_message(pool_id)); there is no existence lookup and no not_found(..) path, unlike the principal branch at lines 212-231. The region and all-zero uuid above are one run's values filling the message template, not part of the requirement.

## Steps to Reproduce
1. git_clone(); 2. run_behavioral_suite(); 3. re-replay iot.get-effective-policies-reflects-principal-attachment.

Direct repro against the booted emulator with a botocore `iot` client (any region, any account):
1. iot.get_effective_policies(cognitoIdentityPoolId="<region>:00000000-0000-0000-0000-000000000000") — a well-formed <region>:<uuid> pool id that no identity pool in the account uses, with no `principal` and no `thingName`.
   Expect: HTTP 404, ResourceNotFoundException, message "IdentityPool '<region>:00000000-0000-0000-0000-000000000000' not found."
   Observe: HTTP 200 with {"effectivePolicies": []}.
2. Contrast, to show the inconsistency inside the one handler: iot.get_effective_policies(principal="arn:aws:iot:<region>:<account>:cert/<64-hex id no certificate was ever created with>") is refused 404 ResourceNotFoundException by the same handler, while step 1 is not.
3. Control, which must keep working: iot.get_effective_policies(cognitoIdentityPoolId="not-a-pool-id") stays HTTP 400 InvalidRequestException (form refusal), and get_effective_policies() with no arguments / with only thingName=<an existing thing> stay HTTP 400 InvalidRequestException "Both principal and CognitoIdentityPoolId can not be null."

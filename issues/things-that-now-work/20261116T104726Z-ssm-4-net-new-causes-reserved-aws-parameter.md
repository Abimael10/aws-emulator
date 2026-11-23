Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.parameter-hierarchy-depth-and-prefix-conflict-rejected ssm.parameter-policies-survive-a-value-update-and-only-an-empty-array-clears-them ssm.expiration-notification-requires-expiration-policy ssm.list-nodes-summary-aggregates-by-filter
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.parameter-hierarchy-depth-and-prefix-conflict-rejected|run#52|AssertionError|assert reserved_read == "AccessDeniedException", (
Issue-Key: ssm.parameter-policies-survive-a-value-update-and-only-an-empty-array-clears-them|run#47|AssertionError|assert quota_message == (
Issue-Key: ssm.expiration-notification-requires-expiration-policy|run#10|AssertionError|assert alone_message == _MISSING_EXPIRATION_MESSAGE, alone_message
Issue-Key: ssm.list-nodes-summary-aggregates-by-filter|run#8|AssertionError|assert summary_code == "UnsupportedOperationException", (
Issue-Commit: 749d15b6fbd9d45e9f4cb9d1e27df4cf54db433b

# ssm: 4 net-new causes — reserved /aws/ parameter reads answer ParameterNotFound instead of AccessDeniedException, two Parameter Store policy refusal messages are emulator-authored, and ListNodesSummary answers NotImplemented

> _Note: this issue was identified against the code base at commit `749d15b6fbd9d45e9f4cb9d1e27df4cf54db433b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutParameter

## Description
Four net-new root causes in SSM, all grounded in failing behavioral checks. Each must be closed before submitting.

Cause 1 of 4: GetParameter on a name in the reserved /aws/ namespace is not gated.
AWS: PutParameter and GetParameter both refuse a name under the reserved /aws/ hierarchy with error code AccessDeniedException and the message "No access to reserved parameter name: <name-without-leading-slash>." — fixed text "No access to reserved parameter name: " plus the requested name with its leading '/' stripped, then a period. The read is gated exactly like the write, so the reserved namespace leaks no existence information. Smithy: GetParameter's modeled errors are InternalServerError, InvalidKeyId, ParameterNotFound, ParameterVersionNotFound — AccessDeniedException is an authorization refusal outside that list, so this is recorded behavior; the recording shows the write refused with AccessDeniedException and the same message, and the read refused identically.
Emulator: PutParameter on /aws/<name> is refused correctly, but GetParameter on the same name answers error code ParameterNotFound with an empty message (HTTP 400 per the model's ParameterNotFound).
Why: a client cannot distinguish "you may not touch this namespace" from "nothing is stored there": retry/creation logic that treats ParameterNotFound as "safe to create" will loop against a namespace it can never write, and the emulator discloses non-existence where AWS discloses nothing.
Fix: GetParameter's handler/validation in the SSM parameter store — apply the same reserved-prefix gate the PutParameter path already applies, before the store lookup.
Traces: ssm.parameter-hierarchy-depth-and-prefix-conflict-rejected (assertion at check line 230).

Cause 2 of 4: PutParameter's PoliciesLimitExceededException message is the model's doc sentence, not AWS's runtime message.
AWS: Smithy: PutParameter error PoliciesLimitExceededException (client, HTTP 400, awsQueryError code PoliciesLimitExceededException); its documentation states the maximum is 10 policies per parameter. The runtime message is "Number of policies attached to a parameter cannot exceed 10" — fixed text, with 10 being the fixed API maximum (not a per-request value).
Emulator: the code is right (PoliciesLimitExceededException) but the message is "You specified more than the maximum number of allowed policies for the parameter. The maximum is 10." — verbatim the exception shape's documentation string rather than the service's message. The refused write correctly created nothing.
Why: clients and operators that match or display the refusal text (quota dashboards, retry classifiers keyed on message, test fixtures) see wording AWS never emits.
Fix: PutParameter's policy validation message construction in the SSM ops handler (services/service-com-amazonaws-ssm-amazonssm/src/ops/put_parameter.rs) — stop using the shape's doc text as the error message.
Traces: ssm.parameter-policies-survive-a-value-update-and-only-an-empty-array-clears-them (assertion at check line 323).

Cause 3 of 4: PutParameter's IncompatiblePolicyException message for an ExpirationNotification policy with no Expiration policy is emulator-authored.
AWS: Smithy: PutParameter error IncompatiblePolicyException (client, HTTP 400, awsQueryError code IncompatiblePolicyException), documented as a conflict in the policies specified for the parameter. An Advanced-tier PutParameter whose Policies contain an ExpirationNotification policy but no Expiration policy is refused with that code and the message "Expiration notification policies are missing expiration policy" — fixed text, no request-derived parts.
Emulator: the code is right (IncompatiblePolicyException) but the message is "Expiration notification policy requires an expiration policy" — different wording. The refused write correctly created nothing.
Why: a client that reports or asserts on the refusal text gets wording AWS never emits, and message-keyed handling of policy-conflict classes diverges.
Fix: PutParameter's parameter-policy compatibility validation in the SSM ops handler (services/service-com-amazonaws-ssm-amazonssm/src/ops/put_parameter.rs) — emit AWS's wording for the missing-Expiration conflict.
Traces: ssm.expiration-notification-requires-expiration-policy (assertion at check line 136).

Cause 4 of 4: ListNodesSummary answers NotImplemented where AWS refuses with UnsupportedOperationException.
AWS: Smithy: ListNodesSummary binds UnsupportedOperationException (client, HTTP 400, awsQueryError code UnsupportedOperation), documented "This operation is not supported for the current account. You must first enable the Systems Manager integrated experience in your account." For an account that has not enabled the integrated node experience the call is refused with that code and the message "This is an unsupported operation for this account. You must first enable the Systems Manager integrated experience in your account." — fixed text, no request-derived parts. The sibling ListNodes already answers exactly this in the emulator, so the two operations disagree.
Emulator: ListNodesSummary answers error code 'NotImplemented' (the unimplemented-scaffold refusal), while ListNodes in the same check answers the correct UnsupportedOperationException.
Why: a client probing node-experience enrollment gets a non-AWS error code it cannot classify; SDK retry/error handling for UnsupportedOperationException never triggers, and the operation looks broken rather than not-enabled.
Fix: the ListNodesSummary handler (services/service-com-amazonaws-ssm-amazonssm/src/ops/list_nodes_summary.rs) — refuse like ListNodes with UnsupportedOperationException and AWS's message.
Note: open issue 20261103T215115Z-ssm-listnodessummary-stub.md ("Implement ssm ListNodesSummary: unimplemented scaffold stub") covers the stub for a DIFFERENT check (ssm.list-nodes-summary-invalid-aggregator-rejected); it does not carry this check's replay key, which is why this cause is filed here. Closing that stub with the UnsupportedOperationException refusal above closes this cause too.

## Evidence
Cause 1 — ssm.parameter-hierarchy-depth-and-prefix-conflict-rejected (mismatch):
  Expected: GetParameter(Name='/aws/<name>') -> AccessDeniedException, message "No access to reserved parameter name: aws/<name>." (same diagnostic as the PutParameter refusal on the same name).
  Actual: AssertionError: reading the reserved name /aws/<name> must be refused with AccessDeniedException (not ParameterNotFound -- the namespace is gated, not empty), got 'ParameterNotFound' (message empty).

Cause 2 — ssm.parameter-policies-survive-a-value-update-and-only-an-empty-array-clears-them (mismatch):
  Request: PutParameter(Tier='Advanced', Policies=[11 x ExpirationNotification]).
  Expected: PoliciesLimitExceededException, message "Number of policies attached to a parameter cannot exceed 10".
  Actual: PoliciesLimitExceededException with message "You specified more than the maximum number of allowed policies for the parameter. The maximum is 10." (the Smithy doc sentence).

Cause 3 — ssm.expiration-notification-requires-expiration-policy (mismatch):
  Request: PutParameter(Tier='Advanced', Policies=[{Type: ExpirationNotification, Version: '1.0', Attributes: {Before: '7', Unit: 'Days'}}]).
  Expected: IncompatiblePolicyException, message "Expiration notification policies are missing expiration policy".
  Actual: IncompatiblePolicyException with message "Expiration notification policy requires an expiration policy".

Cause 4 — ssm.list-nodes-summary-aggregates-by-filter (mismatch):
  Request: ListNodesSummary(Aggregators=[{AggregatorType: 'Count', TypeName: 'Instance', AttributeName: 'Region'}], MaxResults=50) on an account without the integrated node experience.
  Expected: UnsupportedOperationException (HTTP 400), message "This is an unsupported operation for this account. You must first enable the Systems Manager integrated experience in your account."
  Actual: AssertionError: ListNodesSummary: expected UnsupportedOperationException, got 'NotImplemented'. ListNodes in the same check answered the expected code and message.

## Steps to Reproduce
Cause 1: PutParameter(Name='/aws/<unique>', Value='x', Type='String') — observe the correct AccessDeniedException and its message — then GetParameter(Name='/aws/<unique>'); it answers ParameterNotFound with an empty message instead of the same AccessDeniedException diagnostic.

Cause 2: PutParameter(Name='/<unique>', Value='never', Type='String', Tier='Advanced', Policies=json of 11 ExpirationNotification policies); read Error.Message of the PoliciesLimitExceededException.

Cause 3: PutParameter(Name='/<unique>', Value='never', Type='String', Tier='Advanced', Policies=json of a single ExpirationNotification policy with no Expiration policy); read Error.Message of the IncompatiblePolicyException.

Cause 4: Call DescribeInstanceInformation(MaxResults=10) as a positive control, then ListNodes(MaxResults=50) (correct refusal), then ListNodesSummary(Aggregators=[{AggregatorType:'Count',TypeName:'Instance',AttributeName:'Region'}], MaxResults=50) and compare the error code/message.

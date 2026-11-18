Issue-Source: trace-sweep-divergence
Issue-Trace: cognito-idp-email-alias-pool-refuses-email-format-usernames
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cognito-idp-email-alias-pool-refuses-email-format-usernames cognito-idp-standard-attribute-value-validation
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp|AdminUpdateUserAttributes|4xx->2xx|$
Issue-Key: cognito-idp|AdminUpdateUserAttributes|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: cognito-idp|AdminUpdateUserAttributes|4xx->2xx|status
Issue-Key: cognito-idp|AdminUpdateUserAttributes|4xx->4xx|$.message
Issue-Key: cognito-idp|UpdateUserAttributes|4xx->2xx|$
Issue-Key: cognito-idp|UpdateUserAttributes|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: cognito-idp|UpdateUserAttributes|4xx->2xx|status
Issue-Related: 20261118T085409Z-cognito-idp-date-members-are-serialized-with-sub.md
Issue-Related: 20261118T085409Z-cognito-idp-email-alias-pools-alias-migration-and.md
Issue-Commit: a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50

# cognito-idp user-attribute update paths perform no validation

> _Note: this issue was identified against the code base at commit `a3bd23e2d05a6ea1b387d01db1dc229f0bef2a50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AdminUpdateUserAttributes

## Description
The attribute-write operations accept values AWS refuses, and the one refusal they do produce is worded by the framework rather than by Cognito. 4 causes; each must be closed before submitting.

Cause 1 of 4: AdminUpdateUserAttributes answers an empty HTTP 200 where AWS refuses the request.
AWS: Smithy: AdminUpdateUserAttributes input UserPoolId, Username (UsernameType), UserAttributes (required AttributeListType); output is empty; modeled errors include InvalidParameterException (400) and AliasExistsException (400). Recorded refusals on this operation are HTTP 400 with an x-amzn-errortype header, __type 'InvalidParameterException' and a message naming the rule that was broken — e.g. 'Username cannot be of email format, since user pool is configured for email alias.' for an email-format Username in an email-alias pool (fixed text, no variable part), and a value-level refusal for a standard attribute whose value does not satisfy the pool's schema. The emulator's 200 carries an empty body, so no refusal information reaches the caller at all.
Emulator: HTTP 200 with body {} for five such steps across two fixtures, and the write is applied.
Why: every server-side guard a client depends on for attribute writes is absent — invalid values become stored user state and the client believes the validation passed.
Fix: input and schema validation in the AdminUpdateUserAttributes handler in the cognito-idp crate (services/service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice), with the refusal serialized as a 400 InvalidParameterException carrying __type, x-amzn-errortype and the message.
Traces: cognito-idp-email-alias-pool-refuses-email-format-usernames#task#7 (cluster also covers cognito-idp-standard-attribute-value-validation#task#6, #task#7, #task#9, #task#14).

Cause 2 of 4: the attribute-length refusal is reported as the generic constraint report instead of Cognito's message.
AWS: recorded refusal: HTTP 400 InvalidParameterException with the message 'The value of the attribute gender must have a length less than or equal to 2048 characters' — fixed text whose variable parts are the attribute name (from the request's UserAttributes entry) and the limit (the pool schema's maximum length for that attribute). AWS does NOT echo the offending value here.
Emulator: answers the framework's constraint report: "1 validation error detected: Value '<the whole oversized value>' at '...' failed to satisfy constraint: ...", which both uses the wrong wording and echoes the entire rejected value.
Why: a client branching on AWS's message (or logging it) sees text AWS never produces for this error class, and the response is inflated with the rejected payload.
Fix: the attribute-value length check in the AdminUpdateUserAttributes handler in the cognito-idp crate — it must emit Cognito's per-attribute message rather than falling through to the generic input-constraint reporter.
Traces: cognito-idp-standard-attribute-value-validation#task#11 (cluster also covers #task#12).

Cause 3 of 4: UpdateUserAttributes accepts a non-mutable attribute.
AWS: Smithy: UpdateUserAttributes models InvalidParameterException (400). Recorded refusal: HTTP 400, __type 'InvalidParameterException', x-amzn-errortype present, message 'Cannot modify the non-mutable attribute email_verified' — fixed text plus the attribute name taken from the request.
Emulator: HTTP 200 with an empty body; the write is applied.
Why: a user can change a verification-grade attribute on themselves, so email_verified stops being a trustworthy claim for any relying party.
Fix: mutability enforcement (the pool schema's Mutable flag, including the implicitly non-mutable *_verified attributes) in the UpdateUserAttributes handler in the cognito-idp crate.
Traces: cognito-idp-standard-attribute-value-validation#task#16.

Cause 4 of 4: the unvalidated write is observable in stored state — AdminGetUser reports the invalid attribute value.
AWS: recorded behavior: because the invalid write is refused, AdminGetUser still reports the user's previous, valid email value.
Emulator: AdminGetUser reports the invalid value that the unrefused update stored ('not-an-email' in the email attribute).
Why: the divergence is not confined to one response — it is persisted, so every later read of that user is wrong too, and state cannot be repaired by retrying the read.
Fix: closing Causes 1-3 (no separate read-path fix); the AdminGetUser step is listed so the state assertion is re-replayed.
Traces: cognito-idp-standard-attribute-value-validation#task#18. That step additionally carries the sub-millisecond date divergence filed as "cognito-idp date members are serialized with sub-millisecond precision".

Related open issue 20261117T112621Z-cognito-idp-9-net-new-causes-forcealiascreation-refused.md (Cause 2) reports unenforced CUSTOM attribute constraints from the behavioral corpus; it carries no wire replay key for these fixture steps and does not cover the standard-attribute, mutability or message-wording legs above.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up):

cognito-idp-email-alias-pool-refuses-email-format-usernames#task#7 AdminUpdateUserAttributes, expected 400 got 200 (cluster of 5: + cognito-idp-standard-attribute-value-validation#task#6, #task#7, #task#9, #task#14):
  status: expected 400, got 200
  $: empty_body (expected {'__type': 'InvalidParameterException', 'message': 'Username cannot be of email format, since user pool is configured for email alias.'}, got {})
  $headers.x-amzn-errortype: missing
cognito-idp-standard-attribute-value-validation#task#11 AdminUpdateUserAttributes, 400 -> 400 (cluster also #task#12):
  $.message: expected 'The value of the attribute gender must have a length less than or equal to 2048 characters', got "1 validation error detected: Value 'gggg...<the full oversized value>' at '...' failed to satisfy constraint: ..."
cognito-idp-standard-attribute-value-validation#task#16 UpdateUserAttributes, expected 400 got 200:
  $: empty_body (expected {'__type': 'InvalidParameterException', 'message': 'Cannot modify the non-mutable attribute email_verified'}, got {})
cognito-idp-standard-attribute-value-validation#task#18 AdminGetUser, 200 -> 200:
  $.UserAttributes[0].Value: expected 'sim2real-probe-cip-user-005e2dd4@example.com', got 'not-an-email'
  $.UserCreateDate / $.UserLastModifiedDate: value_mismatch (expected '<shaped-not-asserted>', got 1794991573.6076317 / 1794991573.7265584)

Smithy: AdminUpdateUserAttributes (UserAttributes required; errors AliasExistsException 400, InvalidParameterException 400, UserNotFoundException, ...; empty output) and UpdateUserAttributes (InvalidParameterException 400).

## Steps to Reproduce
1. Replay fixture cognito-idp-email-alias-pool-refuses-email-format-usernames, step task#7: AdminUpdateUserAttributes naming an email-format Username in an email-alias pool -> expect 400 InvalidParameterException with __type, x-amzn-errortype and 'Username cannot be of email format, since user pool is configured for email alias.'; the emulator answers 200 {}.
2. Replay fixture cognito-idp-standard-attribute-value-validation, steps task#6 / task#7 / task#9 / task#14 (same cluster): invalid standard-attribute writes -> all answered 200 {}.
3. Step task#11 (and task#12): AdminUpdateUserAttributes with a gender value longer than the schema maximum -> expect 'The value of the attribute gender must have a length less than or equal to 2048 characters'; the emulator answers the generic "1 validation error detected: Value '...'" report.
4. Step task#16: UpdateUserAttributes setting email_verified -> expect 400 InvalidParameterException 'Cannot modify the non-mutable attribute email_verified'; the emulator answers 200 {}.
5. Step task#18: AdminGetUser for that user -> the stored email is the value the refused update should never have written.

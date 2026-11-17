Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sesv2.lambda-sesv2-configuration-set-missing-permission-negative lambda__sesv2.lambda-sesv2-create-contact-list-missing-permission-negative lambda__sesv2.lambda-sesv2-create-email-identity-missing-permission-negative
Issue-Key: lambda__sesv2.lambda-sesv2-configuration-set-missing-permission-negative|run#11|AssertionError|assert body["error_message"] == expected_denial, f"{body!r} != {expected_denial!r}"
Issue-Key: lambda__sesv2.lambda-sesv2-create-contact-list-missing-permission-negative|run#11|AssertionError|assert body["error_message"] == expected_denial, f"{body!r} != {expected_denial!r}"
Issue-Key: lambda__sesv2.lambda-sesv2-create-email-identity-missing-permission-negative|run#12|AssertionError|assert bad_body["error_message"] == expected_denial, f"{bad_body!r} != {expected_denial!r}"
Issue-Commit: fe65e03d933b6ac0ef81655d3240f2b613766017

# An in-function SES v2 AccessDeniedException names "resource: *" instead of the specific resource ARN the call targeted

> _Note: this issue was identified against the code base at commit `fe65e03d933b6ac0ef81655d3240f2b613766017`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateConfigurationSet

## Description
Cause 1 of 1: when a Lambda execution role is not granted an `ses:*` action, the AccessDeniedException the SES v2 call raises reports the resource as the literal `*` rather than the ARN of the resource the request actually named. One root cause, observed on three different SES v2 operations.

AWS: recorded behavior only for the message (IAM denial text is not in the Smithy model). An implicit-deny AccessDeniedException is worded with this template: fixed text `User: ` + <caller ARN> + ` is not authorized to perform: ` + <action> + ` on resource: ` + <the ARN of the resource the request named> + ` because no identity-based policy allows the ` + <action> + ` action`. The variable parts come from the request and the caller's identity: the assumed-role ARN of the function's execution role, the `ses:<Operation>` action, and the resource ARN the call targets — `arn:aws:ses:<region>:<account>:configuration-set/<name>` for CreateConfigurationSet, `arn:aws:ses:<region>:<account>:contact-list/<name>` for CreateContactList, `arn:aws:ses:<region>:<account>:identity/<identity>` for CreateEmailIdentity. The literal `*` appears only where the request genuinely names no resource.

Emulator: the caller ARN and the action are correct, but the resource segment is always `*`. The error code `AccessDeniedException` is right; only the resource segment of the message is wrong.

Why: the resource ARN in a denial is what tells an operator which policy statement to widen. Collapsing it to `*` makes every denial look identical and un-actionable, and tooling that parses the ARN out of a denial (to build a least-privilege policy, or to confirm the denial was scoped to the intended resource) gets a wildcard it cannot use.

Fix: the IAM authorization denial path shared by the SES v2 operation handlers — the resource ARN the authorization decision is made against must be the ARN constructed from the request's own input members, and it must be interpolated into the denial message instead of `*`.

Note: this is distinct from the open issue 20261108T003043Z-lambda-sesv2-7-net-new-causes-sesv2-enforces.md, which covers SESv2 enforcing no IAM authorization at all; here the denial IS produced and only its resource segment diverges.

Traces: lambda__sesv2.lambda-sesv2-configuration-set-missing-permission-negative, lambda__sesv2.lambda-sesv2-create-contact-list-missing-permission-negative, lambda__sesv2.lambda-sesv2-create-email-identity-missing-permission-negative

## Evidence
Actual, verbatim (three checks, same root cause — error_code is AccessDeniedException in all three):
  AssertionError: {'success': False, 'error_code': 'AccessDeniedException', 'error_message': 'User: arn:aws:sts::000000000000:assumed-role/<role>/<fn> is not authorized to perform: ses:CreateConfigurationSet on resource: * because no identity-based policy allows the ses:CreateConfigurationSet action'} != 'User: arn:aws:sts::000000000000:assumed-role/<role>/<fn> is not authorized to perform: ses:CreateConfigurationSet on resource: arn:aws:ses:us-west-2:000000000000:configuration-set/<name> because no identity-based policy allows the ses:CreateConfigurationSet action'
  ... ses:CreateContactList on resource: * ...  (expected: arn:aws:ses:<region>:<account>:contact-list/<name>)
  ... ses:CreateEmailIdentity on resource: * ...  (expected: arn:aws:ses:<region>:<account>:identity/<identity>)
The role/function names and resource names are per-run values; the divergence is the `on resource: *` segment where the request's own resource ARN belongs.

## Steps to Reproduce
1. Create a Lambda execution role with no `ses:*` grants.
2. CreateFunction with a handler that calls sesv2 CreateConfigurationSet (then, separately, CreateContactList and CreateEmailIdentity) and returns the error code and message.
3. Invoke it.
4. Observe the message `User: arn:aws:sts::<account>:assumed-role/<role>/<function> is not authorized to perform: ses:CreateConfigurationSet on resource: * because no identity-based policy allows the ses:CreateConfigurationSet action`. Expected `on resource: arn:aws:ses:<region>:<account>:configuration-set/<the name the request supplied>` (and the contact-list / identity ARNs for the other two operations).

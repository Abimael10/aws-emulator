Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iam.get-role-template-version iam.get-role-template-version-contract
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam.get-role-template-version|run#11|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: iam.get-role-template-version-contract|run#11|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: 19f0e5a096ed637da91415d21b3a547e7492acec

# IAM GetRoleTemplateVersion answers every malformed TemplateArn with one generic ValidationError sentence instead of the two class-specific AWS messages

> _Note: this issue was identified against the code base at commit `19f0e5a096ed637da91415d21b3a547e7492acec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRoleTemplateVersion

## Description
GetRoleTemplateVersion refuses a malformed `TemplateArn` with the right status and the right error code (400 / `ValidationError`), but it collapses every form failure onto ONE fixed sentence. AWS words two DIFFERENT messages for two different classes of malformed value, and the emulator serves its single sentence for both. 2 net-new causes, both in this one operation's reading of `TemplateArn`.

Cause 1 of 2: a TemplateArn that is not ARN-shaped at all.
AWS: Smithy — `GetRoleTemplateVersion.TemplateArn` (arnType) is `@required` and bounds only `@length min 20 max 2048`, so the FORM of the value is not modelled and the refusal is recorded behavior only. The recording shows 400 with error code `ValidationError` (the model's own declared `InvalidInputException` / `InvalidInput` is NOT what this member's form failure answers) and the message `Invalid ARN:  Could not be parsed!`. That message is entirely FIXED text with no variable part — note it contains TWO spaces between `Invalid ARN:` and `Could`, and it does not echo the offending value.
Emulator: for the same input class it answers 400 `ValidationError` with `The specified role template ARN is not valid. Expected format is: arn:{partition}:iam::aws:role-template/{servicePrincipal}/{templateName}:{majorVersion} (majorVersion must be a positive integer with no leading zeros).`
Why: a client that branches on, logs, or asserts the refusal text for an unparseable ARN never sees AWS's wording, so an input-shape error is reported as a role-template-format error and the two classes become indistinguishable to the caller.
Fix: the `TemplateArn` form validation this operation's handler calls before it reads the store — `crate::state::role_template_arn_refusal`, invoked from the GetRoleTemplateVersion handler. It must classify the value rather than return one sentence.
Traces: check `iam.get-role-template-version`.

Cause 2 of 2: a well-formed, role-template-shaped ARN whose account field is the caller's own account id instead of the reserved `aws` pseudo-account.
AWS: recorded behavior only (the model constrains only length, as above). Role templates live under the `aws` pseudo-account, like AWS managed policies. The recording shows 400 with error code `ValidationError` and the message `The specified role template ARN is not valid. The ARN must use 'aws' as the account field (e.g., arn:aws:iam::aws:role-template/...).` — the whole sentence is FIXED text, including the literal example; it does not echo the request's account id or the supplied ARN.
Emulator: answers the same single generic sentence as in Cause 1 (`The specified role template ARN is not valid. Expected format is: arn:{partition}:iam::aws:role-template/{servicePrincipal}/{templateName}:{majorVersion} (majorVersion must be a positive integer with no leading zeros).`), so an ARN that is correctly shaped but in the wrong namespace is reported as a shape error.
Why: a caller that built the ARN from its own account id gets no indication that the account field is the problem, and tooling that matches AWS's wording to tell "wrong namespace" from "unparseable ARN" misroutes both.
Fix: same component as Cause 1 — the role-template ARN reading used by this handler must emit the namespace-specific sentence when the ARN parses but its account field is not `aws`.
Traces: check `iam.get-role-template-version-contract`.

Not a regression of an earlier fix: the positive control in both checks (`arn:aws:iam::aws:role-template/iam.amazonaws.com/PowerUserRoleTemplate:1` resolving to TemplateName `PowerUserRoleTemplate` with `arn:aws:iam::aws:policy/PowerUserAccess`) succeeded, so the already-fixed issues `20261029T184359Z-iam-role-templates-are-not-provisioned-so-dependent-role.md` and `20261103T132715Z-iam-acquirerole-answers-404-so-no-role-template.md` hold; only the refusal wording diverges. The NoSuchEntity branch (`Role Template {arn} cannot be found.`) is not reached by either check, so nothing is claimed about it here.

## Evidence
Cause 1 — check iam.get-role-template-version, request `GetRoleTemplateVersion(TemplateArn="not-a-valid-arn-shape-at-all")` (clears the 20-char modelled minimum, nowhere near ARN-shaped):
  expected: 400, Error.Code `ValidationError`, Error.Message `Invalid ARN:  Could not be parsed!`
  actual:   400, Error.Code `ValidationError`, Error.Message `The specified role template ARN is not valid. Expected format is: arn:{partition}:iam::aws:role-template/{servicePrincipal}/{templateName}:{majorVersion} (majorVersion must be a positive integer with no leading zeros).`
  assertion: `AssertionError: expected message 'Invalid ARN:  Could not be parsed!', got 'The specified role template ARN is not valid. Expected format is: ...'`

Cause 2 — check iam.get-role-template-version-contract, request `GetRoleTemplateVersion(TemplateArn="arn:aws:iam::<caller-account-id>:role-template/does-not-exist/<generated-name>:1")` (the account field is the caller's account, not `aws`; the account id and the name are per-run values, not part of the requirement):
  expected: 400, Error.Code `ValidationError`, Error.Message `The specified role template ARN is not valid. The ARN must use 'aws' as the account field (e.g., arn:aws:iam::aws:role-template/...).`
  actual:   400, Error.Code `ValidationError`, Error.Message `The specified role template ARN is not valid. Expected format is: arn:{partition}:iam::aws:role-template/{servicePrincipal}/{templateName}:{majorVersion} (majorVersion must be a positive integer with no leading zeros).`
  assertion: `AssertionError: expected message "The specified role template ARN is not valid. The ARN must use 'aws' as the account field (e.g., arn:aws:iam::aws:role-template/...).", got 'The specified role template ARN is not valid. Expected format is: ...'`

In both cases the status (400) and the error code (`ValidationError`) match; the single diverging field is Error.Message, and the emulator returns the SAME message for both input classes — which is the root cause.

Handler path: services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/ops/get_role_template_version.rs:203-205 returns `crate::scaffolded::Scaffolded::validation_error(refusal)` from the single `crate::state::role_template_arn_refusal(&input.template_arn)` reading, before the store is consulted; that one reading produces the one sentence.

## Steps to Reproduce
1. Boot the emulator and point an IAM client at it.
2. Cause 1: call `GetRoleTemplateVersion` with `TemplateArn="not-a-valid-arn-shape-at-all"` (>=20 chars so botocore sends it). Observe 400 `ValidationError`; compare Error.Message against AWS's `Invalid ARN:  Could not be parsed!` (two spaces after the colon).
3. Cause 2: call `sts:GetCallerIdentity` for the account id, then call `GetRoleTemplateVersion` with `TemplateArn="arn:aws:iam::<that-account-id>:role-template/does-not-exist/anything:1"`. Observe 400 `ValidationError`; compare Error.Message against AWS's `The specified role template ARN is not valid. The ARN must use 'aws' as the account field (e.g., arn:aws:iam::aws:role-template/...).`
4. Note that steps 2 and 3 return the identical message from the emulator, where AWS returns two different ones.
5. Re-replay checks `iam.get-role-template-version` and `iam.get-role-template-version-contract`; each also exercises the positive control `arn:aws:iam::aws:role-template/iam.amazonaws.com/PowerUserRoleTemplate:1`, which already succeeds and must keep succeeding.

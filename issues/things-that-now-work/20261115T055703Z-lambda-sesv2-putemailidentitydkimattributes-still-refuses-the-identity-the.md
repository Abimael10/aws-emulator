Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__sesv2.cfn-deploy-lambda-sesv2-wrong-arn-negative
Issue-Key: lambda__sesv2.cfn-deploy-lambda-sesv2-wrong-arn-negative|run#16|AssertionError|assert valid_body["ok"] is True, f"expected success against the real identity, got {valid_body}"
Issue-Commit: 3e93d103c367b3f539a01bb50ca5553fc9a2ef76

# lambda__sesv2: PutEmailIdentityDkimAttributes still refuses the identity the stack really created — AWS::SES::EmailIdentity's Ref resolves to a generated physical name, not the EmailIdentity value

> _Note: this issue was identified against the code base at commit `3e93d103c367b3f539a01bb50ca5553fc9a2ef76`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEmailIdentityDkimAttributes

## Description
REOPENS 20261109T210043Z-lambda-sesv2-putemailidentitydkimattributes-answers-notfoundexception-for-an-unverifiable-identity-instead.md: its fix left the call refused.

Cause 1 of 1 — a DKIM write against the email identity a CloudFormation stack actually created is refused BadRequestException, because the identity CloudFormation reports creating is not addressable under the name the template gave it: `Ref` on an `AWS::SES::EmailIdentity` resolves to a CloudFormation-generated physical name instead of the resource's `EmailIdentity` property value. Failing check: lambda__sesv2.cfn-deploy-lambda-sesv2-wrong-arn-negative.

AWS: Smithy (com.amazonaws.sesv2) PutEmailIdentityDkimAttributes — PUT /v2/email/identities/{EmailIdentity}/dkim, input {EmailIdentity (@httpLabel, @required, @length min 1), SigningEnabled (Enabled, default false)}, output empty, http code 200; modeled errors BadRequestException (400), NotFoundException (404), TooManyRequestsException (429). CreateEmailIdentity — POST /v2/email/identities, input EmailIdentity @required. For the CloudFormation side the Smithy model says nothing (resource-provider coverage is not modeled), so: recorded behavior only — `AWS::SES::EmailIdentity`'s primary identifier is the `EmailIdentity` value itself, so `{"Ref": "<logical-id>"}` resolves to that domain or address (the same requirement is recorded in issues/things-that-do-not-work/20261029T202037Z-lambda-sesv2-3-net-new-root-causes-a.md: "`Ref` on the AWS::SES::EmailIdentity resolves to the identity name"). A handler that reads that Ref out of an environment variable and calls PutEmailIdentityDkimAttributes with SigningEnabled=true therefore addresses the identity the stack created and gets 200 with an empty body.

Emulator: the stack reaches CREATE_COMPLETE, but the value substituted for `{"Ref": "RealIdentity"}` is a CloudFormation-generated physical name of the shape `<stack-name>-<logical-id>-<random-suffix>` — not the `EmailIdentity` domain the template declared. PutEmailIdentityDkimAttributes on that name is then refused with HTTP 400 BadRequestException and the sentence whose fixed text is `Domain ` + ` is not verified for DKIM signing.` (its one variable part is the identity from the request; the per-run generated name in the recording only fills that template). The consequence is that the check's positive control — identical code and permissions, pointed at the identity the stack really created — is refused with exactly the same code and sentence as its negative arm, which points at a domain that was never created. Equivalently stated from the SESv2 side: the identity the CFN provisioner wrote is unreachable through the SESv2 API under the name the template gave it.

Note on scope: open issue issues/things-that-do-not-work/20261108T003043Z-lambda-sesv2-7-net-new-causes-sesv2-enforces.md, cause 3, describes a neighbouring symptom (CFN-created SESv2 resources reading back `NotFoundException`) and lists this check id, but the suite marks this failure neither ALREADY FILED nor OWNED, and the refusal is now a different one (400 BadRequestException with the DKIM sentence, not 404 NotFoundException with `Email identity <name> does not exist.`), so it is filed here. Fixing it should close with that cause.

Why: a client that deploys the ordinary SES+Lambda stack shape — declare the identity in the template, hand `Ref` of it to the function, have the function enable DKIM signing — cannot use the identity the stack says it created. Worse, the refusal is identical to the refusal for an identity that genuinely does not exist, so a client (and any negative test) cannot distinguish "you named an identity AWS never accepted" from "your own stack's identity is unusable": a deployment that silently produced nothing reads as a caller mistake.

Fix: the CloudFormation stack engine's `AWS::SES::EmailIdentity` resource provider — its physical-resource-id assignment and `Ref`/`GetAtt` resolution must be the resource's `EmailIdentity` property value, and the identity it provisions must be written into the same sesv2 identity state that PutEmailIdentityDkimAttributes and GetEmailIdentity read; and PutEmailIdentityDkimAttributes' validation in the sesv2 handler must accept an identity the account holds rather than refusing it 400.

Traces: lambda__sesv2.cfn-deploy-lambda-sesv2-wrong-arn-negative (the negative arm of the same check, against a never-created domain, now passes; it is the positive control that fails).

## Evidence
Check lambda__sesv2.cfn-deploy-lambda-sesv2-wrong-arn-negative, at the positive control (check file line 113):

AssertionError: expected success against the real identity, got {'ok': False, 'errorCode': 'BadRequestException', 'message': 'An error occurred (BadRequestException) when calling the PutEmailIdentityDkimAttributes operation: Domain sim2real-xsvc-stk-3d49d8d5bb1b-RealIdentity-7F0A933EBC9B is not verified for DKIM signing.'}

Expected (AWS): HTTP 200 with an empty body, so the handler returns {'ok': True, 'identity': '<the EmailIdentity the template declared>'} — a domain of the form sim2real-xsvc-real-<run-id>.example.com in this check.
Actual (emulator): HTTP 400 BadRequestException, fixed text `Domain ` + ` is not verified for DKIM signing.`

Two things diverge in that one line and both are part of this cause:
1. The identity named in the refusal is `sim2real-xsvc-stk-<run>-RealIdentity-<suffix>` — a CloudFormation-generated physical name (`<stack-name>-<logical-id>-<random-suffix>`), where the template set Properties.EmailIdentity to the domain and the function's IDENTITY_NAME environment variable is `{"Ref": "RealIdentity"}`. AWS resolves that Ref to the EmailIdentity value. (Stack name, logical-id suffix and the domain are per-run generated values; the requirement is that Ref yields the declared EmailIdentity.)
2. Given that name, PutEmailIdentityDkimAttributes refuses 400 — the same code and the same sentence the check's earlier, deliberately-wrong arm receives for a domain that was never created (that arm asserts and passes). So the positive and negative arms are indistinguishable.

For contrast, the prior refusal recorded for this check before 20261109T210043Z was fixed was `NotFoundException` / `Email identity <name> does not exist.`; the classification changed, the call is still refused.

## Steps to Reproduce
1. cloudformation.create_stack with Capabilities CAPABILITY_IAM/CAPABILITY_NAMED_IAM and Resources:
   - RealIdentity: AWS::SES::EmailIdentity, Properties {EmailIdentity: "<rand>.example.com"}
   - ExecRole: AWS::IAM::Role trusting lambda.amazonaws.com, with an inline policy allowing logs:* and ses:PutEmailIdentityDkimAttributes on "*"
   - ValidFn: AWS::Lambda::Function (python3.12) whose Environment.Variables.IDENTITY_NAME is {"Ref": "RealIdentity"}, handler calls sesv2 put_email_identity_dkim_attributes(EmailIdentity=os.environ["IDENTITY_NAME"], SigningEnabled=True) and returns {"ok": True, "identity": identity} or the ClientError's code/message.
2. Poll describe_stacks until CREATE_COMPLETE.
3. lambda.invoke ValidFn with payload {}. Observe {'ok': False, 'errorCode': 'BadRequestException', 'message': '... Domain <stack>-RealIdentity-<suffix> is not verified for DKIM signing.'} where AWS answers 200 and the handler returns {'ok': True, 'identity': '<rand>.example.com'}.
4. Note that the value reported in the message is the CFN-generated physical name, not the EmailIdentity the template declared — read it back with describe_stack_resources/Ref to confirm.
5. Separately, sesv2.put_email_identity_dkim_attributes(EmailIdentity='<rand>.example.com', SigningEnabled=True) for the domain the stack declared must also return 200.
6. Re-replay lambda__sesv2.cfn-deploy-lambda-sesv2-wrong-arn-negative.

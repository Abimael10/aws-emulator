Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lexv2-models.cfn-lex-bot-description-over-max-length-rejected-leaving-the-original cloudformation__lexv2-models.cfn-lex-bot-duplicate-name-rejected-leaving-the-original
Issue-Key: cloudformation__lexv2-models.cfn-lex-bot-description-over-max-length-rejected-leaving-the-original|failed_reason#17|AssertionError|assert len(rows) == 1, f"expected one {status} row for {logical_id}, got {rows!r}"
Issue-Key: cloudformation__lexv2-models.cfn-lex-bot-duplicate-name-rejected-leaving-the-original|run#21|AssertionError|assert got == _DUPLICATE, f"reason {got!r} != {_DUPLICATE!r}"
Issue-Commit: 455b877557af92974b1e1b0549e81b06a05b022f

# cloudformation__lexv2-models: 3 net-new causes — an over-max AWS::Lex::Bot Description is not caught by property validation (no stack-level CREATE_FAILED row), and the duplicate-bot-name rollback reason carries the wrong message and HandlerErrorCode

> _Note: this issue was identified against the code base at commit `455b877557af92974b1e1b0549e81b06a05b022f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Three net-new root causes in the CloudFormation AWS::Lex::Bot provisioning path (service heading `cloudformation__lexv2-models`). None is covered by the open issues for this pair — 20261109T170659Z-cloudformation-lexv2-models-2-net-new-causes-aws.md (stub provisioning / child resources), 20261117T134552Z-cloudformation-lexv2-models-a-create-complete-aws-lex.md (Outputs without OutputValue) and 20261118T115344Z-cloudformation-lexv2-models-aws-lex-botalias-fails-to.md (BotAlias create) — which describe different defects; the three checks those issues own were skipped.

Cause 1 of 3 — a Description one character over the maximum rolls the stack back without recording the property-validation failure on the stack.
AWS: the AWS::Lex::Bot resource schema bounds Description at maxLength 2000 (Smithy: CreateBot.description -> Description, @length min 0 max 2000), so a 2000-character Description deploys and a 2001-character one is rejected by CloudFormation's pre-provision property validation — before any resource is provisioned. AWS then (recorded behavior) puts the stack into ROLLBACK_COMPLETE and emits exactly one stack-level CREATE_FAILED event whose LogicalResourceId is the STACK's own logical id (the stack name) with the fixed ResourceStatusReason "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation." (fixed text; only the error count varies with the number of validation failures), plus a DescribeEvents OperationEvents row of EventType VALIDATION_ERROR with ValidationName=PROPERTY_VALIDATION, ValidationStatus=FAILED, ValidationFailureMode=FAIL, ValidationPath=/Resources/Bot/Properties/Description and ValidationStatusReason "#/Description: expected maxLength: 2000, actual: 2001" (fixed text; the member path and the two numbers come from the schema and the request), and DescribeStackResources reports no resource at all.
Emulator: the stack does reach ROLLBACK_COMPLETE, but DescribeStackEvents contains NO CREATE_FAILED row for the stack's own logical id — the check's `failed_reason(..., status="CREATE_FAILED")` on the stack logical id got `[]`, so the aggregated validation reason is never recorded and the later VALIDATION_ERROR / no-resources assertions cannot even be reached. Hypothesis: the emulator skips pre-provision property validation of AWS::Lex::Bot and instead attempts the Lex CreateBot call, which refuses the over-long description, so the failure is recorded (if at all) against the Bot resource rather than the stack.
Why: a client that reads the stack's CREATE_FAILED reason (console, CLI `describe-stack-events`, CDK/SAM deploy output) sees a rolled-back stack with no explanation of what was wrong, and tooling that calls DescribeEvents to list the property errors gets nothing — the very path AWS's reason tells it to use. Callers also lose AWS's guarantee that a property-validation rejection provisions nothing.
Fix: the CloudFormation create path's pre-provision resource-property validation stage for AWS::Lex::Bot (the resource schema's Description maxLength), and the stack-event/operation-event recording that emits the stack-level CREATE_FAILED reason and the VALIDATION_ERROR operation event.
Traces: cloudformation__lexv2-models.cfn-lex-bot-description-over-max-length-rejected-leaving-the-original.

Cause 2 of 3 — the Lex duplicate-bot-name refusal message is not AWS's.
AWS: CreateBot with a botName Lex already holds is refused with ConflictException (Smithy: CreateBot errors include ConflictException, httpError 409), whose message is (recorded behavior) the fixed sentence "Couldn't create the bot. A bot with the same name already exists. Use a different name and try your request again." — all fixed text, with no value interpolated from the request. CloudFormation surfaces it verbatim inside the resource-handler reason (`Resource handler returned message: "<message>" (RequestToken: ..., HandlerErrorCode: ...)`) on the Bot resource's CREATE_FAILED row.
Emulator: the message is "Bot with name <name> already exists. Use another name." — different fixed text, and it interpolates the requested bot name, which AWS's message does not contain.
Why: a client matching or displaying the conflict message (CDK/CFN failure output, retry logic keyed on the sentence) sees text AWS never emits, and the interpolated name makes the message non-constant where AWS's is constant.
Fix: the lexv2-models CreateBot handler's duplicate-name conflict path — the ConflictException message it constructs.
Traces: cloudformation__lexv2-models.cfn-lex-bot-duplicate-name-rejected-leaving-the-original.

Cause 3 of 3 — the AWS::Lex::Bot handler classifies the duplicate-name conflict as HandlerErrorCode: ResourceConflict instead of AlreadyExists.
AWS: when the name is already taken, the AWS::Lex::Bot resource handler reports the failure with HandlerErrorCode `AlreadyExists` in the CREATE_FAILED ResourceStatusReason (recorded behavior; the reason parses as `Resource handler returned message: "..." (RequestToken: <token>, HandlerErrorCode: AlreadyExists)`).
Emulator: the same reason carries `HandlerErrorCode: ResourceConflict`.
Why: HandlerErrorCode is the machine-readable classification clients and CFN itself branch on (an already-exists conflict is retriable by renaming, a generic ResourceConflict is not), so automation keyed on AlreadyExists never recognises the name collision.
Fix: the AWS::Lex::Bot resource provider's Lex-error -> HandlerErrorCode mapping in the CloudFormation resource-handler layer.
Traces: cloudformation__lexv2-models.cfn-lex-bot-duplicate-name-rejected-leaving-the-original.

## Evidence
Check cfn-lex-bot-description-over-max-length-rejected-leaving-the-original (mismatch):
  stack with Description of 2001 chars reached ROLLBACK_COMPLETE (expected), then
  AssertionError: expected one CREATE_FAILED row for sim2real-behav-cfn-lexdesclen-bad-935d5592a552, got []
  Expected: exactly one stack-level CREATE_FAILED event with ResourceStatusReason
    "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list of issues with resource and property details, resolve each error, then retry the operation."
  and a DescribeEvents OperationEvents VALIDATION_ERROR row
    ("Bot", "AWS::Lex::Bot", "PROPERTY_VALIDATION", "FAILED", "FAIL", "#/Description: expected maxLength: 2000, actual: 2001", "/Resources/Bot/Properties/Description")
  and DescribeStackResources == [].
  Actual: no stack-level CREATE_FAILED row at all (empty list).
  The 2000-character boundary stack created and read back fine, so only the over-max leg diverges.

Check cfn-lex-bot-duplicate-name-rejected-leaving-the-original (mismatch):
  AssertionError: reason ('Bot with name <name> already exists. Use another name.', 'ResourceConflict')
                      != ("Couldn't create the bot. A bot with the same name already exists. Use a different name and try your request again.", 'AlreadyExists')
  (left = emulator's (masked handler message, HandlerErrorCode) parsed from the Bot CREATE_FAILED ResourceStatusReason; right = AWS's. '<name>' is the harness masking the run's bot name — AWS's message contains no name at all.)
  Stack B did roll back to ROLLBACK_COMPLETE and created no second bot; only the reason's message text and HandlerErrorCode diverge.

## Steps to Reproduce
Cause 1: CreateStack (Capabilities=[CAPABILITY_IAM]) with a template holding an AWS::IAM::Role (lexv2.amazonaws.com trust) and an AWS::Lex::Bot whose Properties are {Name, Description: 'A'*2001, RoleArn: Fn::GetAtt Role.Arn, DataPrivacy: {ChildDirected: false}, IdleSessionTTLInSeconds: 300}. Wait for the terminal status (ROLLBACK_COMPLETE), then DescribeStackEvents(StackName=<stackId>) and look for a CREATE_FAILED row whose LogicalResourceId is the stack name: none exists. Also call DescribeEvents(StackName=<stackId>) and look for an EventType=VALIDATION_ERROR OperationEvents row, and DescribeStackResources(StackName=<stackId>). The same template with Description of exactly 2000 characters must (and does) reach CREATE_COMPLETE.

Causes 2 and 3: CreateStack the same template (no Description) and wait for CREATE_COMPLETE, then CreateStack a second stack whose AWS::Lex::Bot carries the SAME Name. Wait for ROLLBACK_COMPLETE and read the Bot resource's CREATE_FAILED ResourceStatusReason from DescribeStackEvents: its quoted message is 'Bot with name <botName> already exists. Use another name.' and its HandlerErrorCode is ResourceConflict, where AWS gives "Couldn't create the bot. A bot with the same name already exists. Use a different name and try your request again." and AlreadyExists.

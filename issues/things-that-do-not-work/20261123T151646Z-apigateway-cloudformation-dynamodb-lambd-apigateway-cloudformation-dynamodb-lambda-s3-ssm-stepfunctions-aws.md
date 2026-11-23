Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys
Issue-Key: apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys|_run_cli#13|AssertionError|assert result.returncode == 0, (
Issue-Commit: a7ab2a4527a82fa9910f5ebaa0f490e37d2a6ca8

# apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions: AWS::Lex::BotVersion exposes no BotVersion attribute, so Fn::GetAtt on it fails the AWS::Lex::BotAlias resource and the CDK reference estate's LexBot stack never deploys

> _Note: this issue was identified against the code base at commit `a7ab2a4527a82fa9910f5ebaa0f490e37d2a6ca8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new root cause (Cause 1 of 1) in CloudFormation's AWS::Lex::* resource providers, reached from the 39-stack CDK reference estate.

NOTE on existing issues: three open issues touch CloudFormation's Lex resource types, and none of them covers this failure or carries its replay key. 20261109T170659Z-cloudformation-lexv2-models-2-net-new-causes-aws.md (Cause 2) and 20261117T134552Z-cloudformation-lexv2-models-a-create-complete-aws-lex.md both describe a stack that reaches CREATE_COMPLETE and then hands back DescribeStacks Outputs entries with an OutputKey but no OutputValue - i.e. the GetAtt is silently dropped at OUTPUT-serialization time on a SUCCEEDING stack. 20261118T115344Z-cloudformation-lexv2-models-aws-lex-botalias-fails-to.md describes AWS::Lex::BotAlias CREATE_FAILED with the refusal reason not observable from the replay, and hypothesizes (its own word) an empty/unresolved BotVersion property. The failure here is a different, now-observable behavior: the engine actively REFUSES the intrinsic with the AWS template-error wording for an attribute the resource type does not declare, as the BotAlias resource's status reason, so the stack operation fails outright. It also appears under a different service heading and in a different check, which the suite marked neither OWNED nor ALREADY FILED.

Cause 1 of 1: CloudFormation's resource-type registry entry for AWS::Lex::BotVersion declares no readable `BotVersion` attribute, so resolving {"Fn::GetAtt": [<AWS::Lex::BotVersion logical id>, "BotVersion"]} is refused as an unsupported attribute instead of resolving to the version number Lex assigned. Because AWS::Lex::BotAlias's BotVersion property is normally wired that way, the alias resource fails and the whole Lex stack fails with it.
Failing check: apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys.

AWS: AWS::Lex::BotVersion's return values include a `BotVersion` Fn::GetAtt attribute - the numeric version string Lex assigns when the version is cut - and the canonical CDK/CloudFormation pattern for an aliased bot declares AWS::Lex::BotAlias {BotId: {Ref: <Bot>}, BotAliasName: <name>, BotVersion: {Fn::GetAtt: [<BotVersion>, BotVersion]}}. That GetAtt resolves and the stack reaches CREATE_COMPLETE. The value is the one Lex's own API returns: Smithy (lexmodelsv2) CreateBotVersion is PUT /bots/{botId}/botversions, success code 202, output member botVersion -> NumericalBotVersion (@length min 1 max 5, @pattern ^[0-9]+$); CreateBotAlias is PUT /bots/{botId}/botaliases, success code 202, and its optional input member botVersion targets the same NumericalBotVersion shape - so the attribute a template reads off AWS::Lex::BotVersion is exactly what the alias's botVersion input wants. AWS's message "Template error: resource <LogicalId> does not support attribute type <AttributeName> in Fn::GetAtt" is the correct refusal for an attribute a resource type genuinely does not expose (fixed text: "Template error: resource ", " does not support attribute type ", " in Fn::GetAtt"; variable parts: the referenced resource's LOGICAL ID from the template and the attribute NAME from the request) - it is simply the wrong answer for BotVersion on AWS::Lex::BotVersion, which AWS does expose. There is no Smithy model for CloudFormation resource types, so the attribute surface itself is recorded/documented behavior only, not modelled.

Emulator: during `cdk deploy --all` of the reference estate, the LexBot stack fails. The CLI reports the resource-level failure verbatim: "<stack>-LexBot failed to deploy / DeploymentError: Resource updates failed: <stack>-LexBot/FirstBotAliasWithCFN (AWS::Lex::BotAlias FirstBotAliasWithCFN) Template error: resource BookTripBotVersionWithCFN does not support attribute type BotVersion in Fn::GetAtt". So the engine reaches the alias resource, tries to resolve its BotVersion property, finds no `BotVersion` attribute registered for the AWS::Lex::BotVersion resource type, and records the unsupported-attribute template error as the resource's failure reason rather than handing the alias provider the version number. (The logical ids FirstBotAliasWithCFN / BookTripBotVersionWithCFN and the generated stack-name prefix are this estate's/run's values, not part of the requirement; the ECS minHealthyPercent and circuitBreaker lines in the same output tail are CDK construct WARNINGS and are not failures.) The check then fails its own assertion: `AssertionError: \`cdk deploy --all\` exited 1, expected 0`.

Why: a client cannot deploy an aliased Lex bot through CloudFormation or CDK at all, and because `cdk deploy --all` exits non-zero on the first failing stack, one unresolvable attribute takes down a whole multi-stack estate deployment - every later stack in the app is never attempted. A client following AWS's documented AWS::Lex::BotVersion return values gets told its template is malformed, so the error points at the template instead of at the emulator, and there is no workaround short of hard-coding a version number that does not exist until the version resource has run. Downstream, nothing addressable by botAliasId exists: DescribeBotAlias, ListBotAliases and every lexv2-runtime conversation against the deployed alias have no target.

Fix: the CloudFormation crate's resource-type registry entry for AWS::Lex::BotVersion - its GetAtt attribute surface must declare `BotVersion` (and resolve it to the numeric version the lexmodelsv2 CreateBotVersion handler returned for that resource, matching NumericalBotVersion's ^[0-9]+$/@length 1-5) - together with the stack engine's Fn::GetAtt resolver, which must only emit "Template error: resource <LogicalId> does not support attribute type <Attr> in Fn::GetAtt" for attributes the resource type genuinely does not expose. The AWS::Lex::BotAlias provider's create path must then receive that resolved value as its botVersion property.

Traces: apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys.

## Evidence
Suite: simparity-behavioral-aws-v1, service heading `### apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions` - 0/1 checks passed. This was the only failing check in the whole suite run that the report marked neither "OWNED by" nor "ALREADY FILED" (370 failing checks listed, 369 of them already owned/filed).

Check apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys [mismatch]
- The check deploys a 39-stack CDK app (parts include AppRoles, LambdaCron, ..., LexBot, ApiTokenAuth, ...) into us-east-1 with `cdk deploy --all`, then serves and destroys it.
- expected: `cdk deploy --all` exits 0 and every stack, including LexBot, reaches CREATE_COMPLETE. The LexBot stack declares an AWS::Lex::BotAlias (logical id FirstBotAliasWithCFN) whose BotVersion property is {"Fn::GetAtt": ["BookTripBotVersionWithCFN", "BotVersion"]} against an AWS::Lex::BotVersion resource; AWS resolves that attribute to the numeric Lex version.
- actual: exit code 1. Output tail (quoted from the replay):
    "...sim2real-ref-<id>-LexBot failed to deploy
     DeploymentError: Resource updates failed:
     sim2real-ref-<id>-LexBot/FirstBotAliasWithCFN  (AWS::Lex::BotAlias FirstBotAliasWithCFN)
       Template error: resource BookTripBotVersionWithCFN does not support attribute type BotVersion in Fn::GetAtt"
  Assertion: `AssertionError: \`cdk deploy --all\` exited 1, expected 0; output tail: ...` at _run_cli.
- In that message, "Template error: resource " / " does not support attribute type " / " in Fn::GetAtt" is AWS's fixed text; "BookTripBotVersionWithCFN" is the referenced resource's logical id from the template and "BotVersion" is the requested attribute name. The sim2real-ref-<id> stack-name prefix is a per-run generated value.
- Also present in the same tail and NOT part of this finding: two CDK construct WARNINGs about @aws-cdk/aws-ecs:minHealthyPercent and @aws-cdk/aws-ecs:shouldUseCircuitBreaker on the EcsFargateEfs stack (advisory annotations, not failures), and a `cdk diagnose` note that only the most recent 500 CloudTrail events in the window were checked.

Smithy (lexmodelsv2, services/service-com-amazonaws-lexmodelsv2-lexmodelbuildingservicev2/smithy-model/com-amazonaws-lexmodelsv2-lexmodelbuildingservicev2.json):
- CreateBotVersion PUT /bots/{botId}/botversions -> 202; output botVersion -> NumericalBotVersion (@length min 1 max 5, @pattern ^[0-9]+$); errors ConflictException 409, PreconditionFailedException 412, ServiceQuotaExceededException 402, ThrottlingException 429, ValidationException 400, InternalServerException 500.
- CreateBotAlias PUT /bots/{botId}/botaliases -> 202; required botId -> Id (@length 10/10, @pattern ^[0-9a-zA-Z]+$) and botAliasName -> Name (@length 1-100, @pattern ^([0-9a-zA-Z][_-]?){1,100}$); optional botVersion -> NumericalBotVersion (the same shape the GetAtt must supply).
- CloudFormation resource types are not Smithy-modelled, so AWS::Lex::BotVersion's `BotVersion` return value is documented/recorded behavior only.

Not filed here (not emulator-caused): nothing. The failure is the emulator's own refusal of a documented intrinsic; the ECS construct warnings above are the CDK toolkit's, and the check did not fail on them.

## Steps to Reproduce
1. CreateStack (or CreateChangeSet + ExecuteChangeSet, as `cdk deploy` does) in us-east-1 with Capabilities=[CAPABILITY_IAM] and a template containing:
   - AWS::IAM::Role trusting lexv2.amazonaws.com;
   - AWS::Lex::Bot {Name: <unique>, RoleArn: {Fn::GetAtt: [Role, Arn]}, DataPrivacy: {ChildDirected: false}, IdleSessionTTLInSeconds: 300, AutoBuildBotLocales: true, BotLocales: [{LocaleId: "en_US", NluConfidenceThreshold: 0.40, Intents: [...]}]};
   - AWS::Lex::BotVersion, logical id e.g. BookTripBotVersionWithCFN, {BotId: {Ref: <Bot>}, BotVersionLocaleSpecification: [{LocaleId: "en_US", BotVersionLocaleDetails: {SourceBotVersion: "DRAFT"}}]};
   - AWS::Lex::BotAlias, logical id e.g. FirstBotAliasWithCFN, {BotId: {Ref: <Bot>}, BotAliasName: <name>, BotVersion: {Fn::GetAtt: [BookTripBotVersionWithCFN, "BotVersion"]}}.
2. Poll DescribeStacks / DescribeStackEvents to a terminal status. The AWS::Lex::BotAlias resource fails and its ResourceStatusReason is "Template error: resource BookTripBotVersionWithCFN does not support attribute type BotVersion in Fn::GetAtt"; the stack operation fails rather than completing.
3. Equivalently, run the suite's reference estate: `cdk deploy --all --ci --no-version-reporting --no-notices` for the 39-stack app in us-east-1. The CLI exits 1 with "<stack>-LexBot failed to deploy / DeploymentError: Resource updates failed: <stack>-LexBot/FirstBotAliasWithCFN (AWS::Lex::BotAlias FirstBotAliasWithCFN) Template error: resource BookTripBotVersionWithCFN does not support attribute type BotVersion in Fn::GetAtt", so the stacks after LexBot are never deployed.
4. Expected instead: Fn::GetAtt [<AWS::Lex::BotVersion>, BotVersion] resolves to the numeric version lexmodelsv2 CreateBotVersion returned for that resource (a NumericalBotVersion matching ^[0-9]+$, length 1-5); the alias is created with that botVersion; the LexBot stack reaches CREATE_COMPLETE and `cdk deploy --all` exits 0. Reading the same attribute in an Output must likewise return that version value (not an Output entry without OutputValue).
5. Negative control: Fn::GetAtt on an attribute AWS::Lex::BotVersion genuinely does not expose must still answer "Template error: resource <LogicalId> does not support attribute type <Attr> in Fn::GetAtt" with the template's logical id and the requested attribute name.

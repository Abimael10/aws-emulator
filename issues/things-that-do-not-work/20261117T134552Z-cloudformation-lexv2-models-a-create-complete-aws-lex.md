Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__lexv2-models.cfn-lex-bot-version-alias-description-and-sentiment-analysis-read-back
Issue-Key: cloudformation__lexv2-models.cfn-lex-bot-version-alias-description-and-sentiment-analysis-read-back|stack_outputs#9|KeyError|return {o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}
Issue-Commit: 6b5656206a559bae439cbcd0e2a96a2a62a67d12

# cloudformation__lexv2-models: a CREATE_COMPLETE AWS::Lex::Bot/BotVersion/BotAlias stack returns DescribeStacks Outputs entries with an OutputKey but no OutputValue, so the version/alias read-back can never run

> _Note: this issue was identified against the code base at commit `6b5656206a559bae439cbcd0e2a96a2a62a67d12`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStacks

## Description
One net-new root cause (1 of 1).

NOTE on an existing issue: 20261109T170659Z-cloudformation-lexv2-models-2-net-new-causes-aws.md describes the same class of defect as its Cause 2, but it does NOT carry this check's replay key (its Issue-Keys name only cfn-lex-bot-version-and-alias-read-back-then-resource-not-found-after-delete and cfn-lex-resource-policy-read-back-then-resource-not-found-after-delete, and the suite did not mark this check OWNED). This issue is filed so the check below is gated; a fixer closing that issue should re-replay this check too.

Cause 1 of 1: DescribeStacks reports Outputs entries for a CREATE_COMPLETE stack whose Output values are {Ref: <AWS::Lex::BotAlias>} and {Fn::GetAtt: [<AWS::Lex::BotVersion>, BotVersion]} with an OutputKey member but no OutputValue member at all.
Failing check: cloudformation__lexv2-models.cfn-lex-bot-version-alias-description-and-sentiment-analysis-read-back.
AWS: every Output of a stack that reached CREATE_COMPLETE carries both OutputKey and OutputValue. Smithy (cloudformation): Stack.Outputs targets the list `Outputs` of structure `Output` with members OutputKey, OutputValue, Description, ExportName; `OutputValue` is "The value associated with the output" and is populated for every resolved output of a successfully created stack. An intrinsic the engine cannot resolve (a Ref or Fn::GetAtt naming an attribute the resource type does not expose) is a template/handler error: CloudFormation fails the resource and rolls the stack back (ROLLBACK_COMPLETE with a resource-level reason), it never serves a successful stack carrying a value-less Output. For AWS::Lex::BotVersion, Fn::GetAtt BotVersion.BotVersion is the numeric version Lex assigned (the same value DescribeBotVersion echoes as botVersion); for AWS::Lex::BotAlias, Ref is the composite "<botAliasId>|<botId>".
Emulator: the stack reaches CREATE_COMPLETE, DescribeStacks(StackName=<StackId>) lists Stacks[0].Outputs with the three declared keys (BotId, BotVersionNumber, BotAliasId), but the BotVersionNumber and BotAliasId entries have no OutputValue key, so an SDK consumer indexing the modeled member raises: `KeyError: 'OutputValue'` from `{o["OutputKey"]: o["OutputValue"] for o in stack.get("Outputs", [])}`. The check dies there, before it can assert DescribeBotVersion.description equals the declared BotVersion Description or DescribeBotAlias.sentimentAnalysisSettings equals {"detectSentiment": true} — those two read-backs are therefore still ungraded.
Why: a client that deploys a Lex bot version and alias with CloudFormation believes the deployment succeeded (CREATE_COMPLETE) but cannot obtain the version number or the alias id. SDK/botocore users crash on the declared-but-absent OutputValue member rather than receiving a value or a clear failure; `aws cloudformation describe-stacks --query 'Stacks[0].Outputs'`, cross-stack Fn::ImportValue and any pipeline that feeds the alias id to lexv2-runtime are all unusable, while the stack still reads as a success.
Fix: CloudFormation's stack-engine output resolution (Ref / Fn::GetAtt resolution against resource attributes) and the DescribeStacks Outputs serializer, plus the attribute surfaces of the AWS::Lex::BotVersion and AWS::Lex::BotAlias resource providers — each provider must expose its composite primary identifier (Ref) and its documented GetAtt attributes (BotVersion, Arn), and an output the engine cannot resolve must fail the resource and roll the stack back instead of being emitted without OutputValue.
Traces: cloudformation__lexv2-models.cfn-lex-bot-version-alias-description-and-sentiment-analysis-read-back.

## Evidence
Suite: simparity-behavioral-aws-v1, service heading `### cloudformation__lexv2-models` (0/6 checks passed; the other five failures are already filed/OWNED).

Check cloudformation__lexv2-models.cfn-lex-bot-version-alias-description-and-sentiment-analysis-read-back [error]:
- Template Resources: AWS::IAM::Role (lexv2.amazonaws.com trust), AWS::Lex::Bot (Name, Description, RoleArn={Fn::GetAtt [Role, Arn]}, DataPrivacy{ChildDirected:false}, IdleSessionTTLInSeconds=300, AutoBuildBotLocales=true, one en_US BotLocale with OrderPizza + FallbackIntent), AWS::Lex::BotVersion (BotId={Ref Bot}, Description, BotVersionLocaleSpecification=[{LocaleId:en_US, BotVersionLocaleDetails:{SourceBotVersion:DRAFT}}]), AWS::Lex::BotAlias (BotId={Ref Bot}, BotAliasName, BotVersion={Fn::GetAtt [BotVersion, BotVersion]}, SentimentAnalysisSettings{DetectSentiment:true}).
- Template Outputs: BotId={Ref Bot}, BotVersionNumber={Fn::GetAtt [BotVersion, BotVersion]}, BotAliasId={Ref BotAlias}.
- expected: stack reaches CREATE_COMPLETE and DescribeStacks Stacks[0].Outputs entries each carry OutputKey AND OutputValue, so BotVersionNumber is the Lex-assigned version and BotAliasId is "<botAliasId>|<botId>".
- actual: stack reports CREATE_COMPLETE; reading the outputs raises `KeyError: 'OutputValue'` at `stack_outputs()` (line 127 of the check), i.e. the Output structures were serialized without the OutputValue member.
- Smithy (cloudformation): shape `Output` = {OutputKey, OutputValue, Description, ExportName}; `Stack.Outputs` -> list of `Output`; a CREATE_COMPLETE stack's resolved outputs always carry OutputValue.

Ungraded downstream of this cause (not separately filed): DescribeBotVersion.description read-back and DescribeBotAlias.sentimentAnalysisSettings == {"detectSentiment": true}.

## Steps to Reproduce
1. CreateStack with Capabilities=[CAPABILITY_IAM] and a template containing: an AWS::IAM::Role trusting lexv2.amazonaws.com; an AWS::Lex::Bot with Properties {Name: <unique>, Description: <text>, RoleArn: {Fn::GetAtt: [Role, Arn]}, DataPrivacy: {ChildDirected: false}, IdleSessionTTLInSeconds: 300, AutoBuildBotLocales: true, BotLocales: [{LocaleId: "en_US", NluConfidenceThreshold: 0.40, Intents: [{Name: "OrderPizza", SampleUtterances: [{Utterance: "I want to order a pizza"}]}, {Name: "FallbackIntent", ParentIntentSignature: "AMAZON.FallbackIntent"}]}]}; an AWS::Lex::BotVersion {BotId: {Ref: Bot}, Description: <text>, BotVersionLocaleSpecification: [{LocaleId: "en_US", BotVersionLocaleDetails: {SourceBotVersion: "DRAFT"}}]}; an AWS::Lex::BotAlias {BotId: {Ref: Bot}, BotAliasName: "live", BotVersion: {Fn::GetAtt: [BotVersion, BotVersion]}, SentimentAnalysisSettings: {DetectSentiment: true}}; and Outputs {BotId: {Value: {Ref: Bot}}, BotVersionNumber: {Value: {Fn::GetAtt: [BotVersion, BotVersion]}}, BotAliasId: {Value: {Ref: BotAlias}}}.
2. Poll DescribeStacks(StackName=<StackId>) until CREATE_COMPLETE.
3. Read Stacks[0].Outputs — the BotVersionNumber and BotAliasId entries have an OutputKey but no OutputValue member; any SDK client indexing OutputValue raises KeyError/missing-field.
4. Expected instead: each Output carries OutputValue (the Lex-assigned version number, and "<botAliasId>|<botId>"), so that lexv2-models DescribeBotVersion(botId, botVersion) and DescribeBotAlias(botId, botAliasId) can be called with them.

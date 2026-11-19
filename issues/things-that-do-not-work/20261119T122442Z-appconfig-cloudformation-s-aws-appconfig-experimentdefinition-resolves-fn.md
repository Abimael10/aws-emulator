Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig.deploy-appconfig-experiment-via-cloudformation
Issue-Shape-Id: com.amazonaws.appconfig#AmazonAppConfig
Issue-Key: appconfig|GetExperimentDefinition|ResourceNotFoundException
Issue-Commit: 2714aa03aa1d60831b8d663aed1f4386c9b03868

# appconfig: CloudFormation's AWS::AppConfig::ExperimentDefinition resolves Fn::GetAtt Id to the template Name, so GetExperimentDefinition 404s on a CREATE_COMPLETE stack

> _Note: this issue was identified against the code base at commit `2714aa03aa1d60831b8d663aed1f4386c9b03868`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetExperimentDefinition

## Description
One net-new root cause, grounded in a replayed behavioral check. Numbered; close it before submitting.

=== Cause 1 of 1: an AWS::AppConfig::ExperimentDefinition provisioned by CloudFormation is not addressable — Fn::GetAtt <logical>.Id hands back the resource's Name, and GetExperimentDefinition on it is refused ===

AWS: a stack declaring AWS::AppConfig::ExperimentDefinition creates a live AppConfig experiment definition, and Fn::GetAtt on its `Id` attribute resolves to the AppConfig-generated experiment-definition Id. That Id is an AppConfig `Id`: Smithy ExperimentDefinition$Id targets com.amazonaws.appconfig#Id with pattern ^[a-z0-9]{4,7}$ — never the resource Name (CreateExperimentDefinition$Name is NameWithReservedAwsPrefix, pattern ^(?!AWS\.).{1,64}$, a different value space). GetExperimentDefinition (Smithy: GET /applications/{ApplicationIdentifier}/experimentdefinitions/{ExperimentDefinitionIdentifier}, success 200, errors BadRequestException 400 / ResourceNotFoundException 404 / InternalServerException 500) then returns that definition, with Status one of ExperimentDefinitionStatus (ACTIVE|ARCHIVED|IDLE) — IDLE for a freshly created definition that has had no run — and Name equal to the template's Name. Recorded behavior for the surrounding flow: after the definition is readable, StartExperimentRun on it succeeds and the definition's Status flips IDLE -> ACTIVE.

Emulator: the stack reaches CREATE_COMPLETE and DescribeStacks now yields an Outputs entry for ExperimentDefinitionId, but its value is the template's experiment Name (the ExperimentName parameter, e.g. a 'sim2real-appcfg-cfn-exp-<suffix>' string — a 1-64 char name, not a 4-7 char lowercase-alphanumeric Id). Passing that value straight back as ExperimentDefinitionIdentifier is refused with HTTP 404 ResourceNotFoundException and the message 'Experiment Definition with Application Identifier <applicationId> and Experiment Definition Identifier <experimentDefinitionIdentifier> could not be found for account <account>.' — fixed text 'Experiment Definition with Application Identifier ', ' and Experiment Definition Identifier ', ' could not be found for account ', '.'; the three variable parts are the request's ApplicationIdentifier, the request's ExperimentDefinitionIdentifier and the caller's account id. Note that `Ref` on the sibling AWS::AppConfig::Application in the SAME stack now resolves to a real generated application Id matching ^[a-z0-9]{4,7}$ (the refusal quotes it and resolves it), so the Application provider is no longer the problem — only the ExperimentDefinition resource's Id attribute is. Hypothesis (neither the model nor the recording states which): the provider is using the template Name as the resource's physical id / Id attribute, instead of creating the definition through appconfig's own CreateExperimentDefinition path and recording the returned Id.

Why: a client that provisions an experiment definition by CloudFormation and reads its identifier out of the stack's Outputs — the only way a template can publish it — receives a value that addresses nothing. Every follow-on appconfig call (GetExperimentDefinition, StartExperimentRun, StopExperimentRun, UpdateExperimentDefinition) 404s while the stack reports success, so a CloudFormation-managed experiment estate is indistinguishable from an empty account and the IDLE -> ACTIVE lifecycle can never be exercised through it.

Fix: the CloudFormation resource provider for AWS::AppConfig::ExperimentDefinition and its attribute resolver — create the definition through appconfig's create path and use the returned Id as the resource's physical id, so Ref and Fn::GetAtt ...Id both resolve to an AppConfig Id that GetExperimentDefinition accepts.

Relation to what is already filed: open issue 20261023T032202Z-appconfig-getconfiguration-and-getexperimentdefinition-answer-501-and-cloudformation.md lists this check's id, but its replay key for this check is a KeyError on 'OutputValue' in _deploy_cfn (its Cause 4, missing stack Outputs values) and its Cause 3 is grounded in appconfig.appconfig-application-cloudformation-behaves (Ref on an Application). Both of those symptoms are gone: the Outputs now carry values and the Application Ref now resolves. The check now fails strictly later, on the ExperimentDefinition Id attribute, and that failure carries no replay key in any open issue, so it is filed here.

Traces: appconfig.deploy-appconfig-experiment-via-cloudformation.

## Evidence
Replay report (simparity-behavioral-aws-v1), check [error] appconfig.deploy-appconfig-experiment-via-cloudformation:

  botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the GetExperimentDefinition operation: Experiment Definition with Application Identifier ho65pb1 and Experiment Definition Identifier sim2real-appcfg-cfn-exp-ed2532b5e277 could not be found for account 000000000000.

Expected: HTTP 200 with the definition — Id matching ^[a-z0-9]{4,7}$, Name equal to the template's ExperimentName, Status 'IDLE' (the check asserts initial['Name'] == experiment_name and initial['Status'] == 'IDLE', then StartExperimentRun -> Status 'RUNNING' and the definition's Status 'ACTIVE').

Actual: the identifier the call was given is the stack's Outputs.ExperimentDefinitionId, produced by {"Fn::GetAtt": ["ExperimentDefinition", "Id"]} after the stack_create_complete waiter returned. Its value is the template's experiment Name, not an AppConfig Id, and nothing is registered under it. The ApplicationIdentifier in the same call (Outputs.ApplicationId, from {"Ref": "Application"}) is a well-formed generated id, showing the stack did register the application.

(The quoted ids, name suffix and account are one run's values filling the message template, not part of the requirement.)

## Steps to Reproduce
1. cloudformation.create_stack with a template declaring AWS::AppConfig::Application, ::Environment, ::ConfigurationProfile (LocationUri 'hosted', Type 'AWS.AppConfig.FeatureFlags'), ::HostedConfigurationVersion, and ::ExperimentDefinition (ApplicationIdentifier/ConfigurationProfileIdentifier/EnvironmentIdentifier by Ref, FlagKey 'checkoutflag', AudienceRule, Control and one Treatment), with Outputs including ExperimentDefinitionId = {"Fn::GetAtt": ["ExperimentDefinition", "Id"]}.
2. Wait for CREATE_COMPLETE, then describe_stacks and read Outputs.ExperimentDefinitionId. Observe it equals the template's experiment Name rather than a value matching ^[a-z0-9]{4,7}$.
3. appconfig.get_experiment_definition(ApplicationIdentifier=<Outputs.ApplicationId>, ExperimentDefinitionIdentifier=<Outputs.ExperimentDefinitionId>). Emulator: 404 ResourceNotFoundException 'Experiment Definition with Application Identifier ... and Experiment Definition Identifier ... could not be found for account ...'. AWS: 200 with Status 'IDLE' and Name equal to the template's ExperimentName, after which StartExperimentRun succeeds and the Status becomes 'ACTIVE'.
Replay appconfig.deploy-appconfig-experiment-via-cloudformation to confirm.

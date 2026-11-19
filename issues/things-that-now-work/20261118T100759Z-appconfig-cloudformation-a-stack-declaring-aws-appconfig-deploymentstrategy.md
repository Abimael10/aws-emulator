Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: appconfig__cloudformation.deploy-cfn-appconfig-deployment-strategy-and-deployment-reaches-complete
Issue-Key: appconfig__cloudformation.deploy-cfn-appconfig-deployment-strategy-and-deployment-reaches-complete|run#11|AssertionError|assert status == "CREATE_COMPLETE", f"expected stack status CREATE_COMPLETE, got {status!r}"
Issue-Commit: fca4932e35500ec04fefe7b09c8bfcdb79838c9d

# appconfig__cloudformation: a stack declaring AWS::AppConfig::DeploymentStrategy + AWS::AppConfig::Deployment ends ROLLBACK_COMPLETE instead of CREATE_COMPLETE

> _Note: this issue was identified against the code base at commit `fca4932e35500ec04fefe7b09c8bfcdb79838c9d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: a CloudFormation stack whose resources are AWS::AppConfig::Application, ::Environment, ::ConfigurationProfile (LocationUri: hosted), ::HostedConfigurationVersion, ::DeploymentStrategy (DeploymentDurationInMinutes 0, GrowthFactor 100, FinalBakeTimeInMinutes 0, ReplicateTo NONE) and ::Deployment (ConfigurationVersion from !GetAtt HostedVersion.VersionNumber) never completes its create: the stack leaves CREATE_IN_PROGRESS in ROLLBACK_COMPLETE, so none of its AppConfig resources survive and no stack output is readable.

AWS: recorded behavior only for the stack verdict — every resource in this template is a valid, documented AppConfig resource type, so AWS creates them and the stack reaches terminal status CREATE_COMPLETE; DescribeStacks then reports Outputs for Ref App / Ref Env / Ref Strategy and for !GetAtt Deployment.DeploymentNumber and !GetAtt Deployment.State, appconfig GetDeploymentStrategy answers with the template's Name/DeploymentDurationInMinutes/GrowthFactor/FinalBakeTimeInMinutes/ReplicateTo, and appconfig GetDeployment (Smithy: appconfig#GetDeployment -> Deployment with members DeploymentNumber, State, PercentageComplete, DeploymentStrategyId) reaches the terminal state with PercentageComplete 100. Only after DeleteStack do GetDeploymentStrategy and GetApplication answer ResourceNotFoundException (messages `DeploymentStrategy with Id <strategyId> could not be found.` and `Application with Id <applicationId> could not be found for account <accountId>.` — fixed text plus the id from the request and the caller's account id).
Emulator: the stack's terminal status is 'ROLLBACK_COMPLETE', i.e. at least one AppConfig resource in the template reported CREATE_FAILED and the whole create was rolled back. The check asserts on the stack status before it can read any output, so the specific failing resource is not visible in the replay output; the stack's own events carry the reason. Hypothesis: the AWS::AppConfig::Deployment resource cannot be created or cannot stabilize (its ConfigurationVersion comes from !GetAtt HostedConfigurationVersion.VersionNumber and its completion is read from the deployment's State), which is consistent with the separately filed AppConfig defects around Deployment$State and CloudFormation's AppConfig resource handlers (20261117T184012Z-appconfig-cloudformation-3-net-new-causes-in-cloudformation-s-appconfig.md, 20261023T032202Z-appconfig-getconfiguration-and-getexperimentdefinition-answer-501-and-cloudformation.md); neither of those issues covers this check's current failure — this check previously failed later, on missing Output.OutputValue, and now fails earlier because the create itself rolls back.
Why: a client cannot deploy AppConfig at all through infrastructure-as-code. A template that AWS accepts is rolled back, so the application, environment, configuration profile, hosted version, deployment strategy and deployment never exist; every downstream stack, pipeline or CDK app that provisions AppConfig this way fails, and the rollback masks which resource was at fault.
Fix: CloudFormation's AWS::AppConfig::* resource handlers — the Create/stabilize path of AWS::AppConfig::Deployment (it must start the deployment with the resolved ConfigurationVersion and treat the deployment reaching its terminal state as stabilized) and of AWS::AppConfig::DeploymentStrategy / ::HostedConfigurationVersion, plus the GetAtt resolution those properties depend on.
Traces: appconfig__cloudformation.deploy-cfn-appconfig-deployment-strategy-and-deployment-reaches-complete

## Evidence
Replay of appconfig__cloudformation.deploy-cfn-appconfig-deployment-strategy-and-deployment-reaches-complete:

  File "check_appconfig__cloudformation_deploy_cfn_appconfig_deployment_strategy_and_deployment_reaches_complete.py", line 182, in run
    assert status == "CREATE_COMPLETE", f"expected stack status CREATE_COMPLETE, got {status!r}"
  AssertionError: expected stack status CREATE_COMPLETE, got 'ROLLBACK_COMPLETE'

Expected: DescribeStacks(StackName=<stackId>) reports StackStatus 'CREATE_COMPLETE' for this template, with Outputs AppId, EnvId, StrategyId, DeploymentNumber, DeploymentState.
Actual: the first terminal status the poll observes is 'ROLLBACK_COMPLETE', so the stack's AppConfig resources are gone and no assertion past the stack status is reached.

## Steps to Reproduce
1. Replay the behavioral suite against the emulator.
2. Re-run check appconfig__cloudformation.deploy-cfn-appconfig-deployment-strategy-and-deployment-reaches-complete: CreateStack with the template declaring AWS::AppConfig::Application / ::Environment / ::ConfigurationProfile (LocationUri hosted) / ::HostedConfigurationVersion / ::DeploymentStrategy (duration 0, growth 100, bake 0, ReplicateTo NONE) / ::Deployment (ConfigurationVersion = !GetAtt HostedVersion.VersionNumber), then poll DescribeStacks by StackId until the status leaves *_IN_PROGRESS.
3. Observe the terminal status is ROLLBACK_COMPLETE instead of CREATE_COMPLETE; read the stack's events (DescribeStackEvents) to see which AppConfig resource reported CREATE_FAILED and its reason.

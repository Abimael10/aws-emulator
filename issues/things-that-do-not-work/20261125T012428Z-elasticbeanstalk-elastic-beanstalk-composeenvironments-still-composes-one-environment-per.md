Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.e2e-compose-multi-tier-application-environments
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.e2e-compose-multi-tier-application-environments|run#30|AssertionError|assert composed == {}, (
Issue-Commit: 073228ee0ef0e97c8beeb6d07a6b3f3605d93133

# Elastic Beanstalk ComposeEnvironments still composes one environment per version label for the two-tier env.yaml manifest set AWS answers with an empty Environments list (1 cause)

> _Note: this issue was identified against the code base at commit `073228ee0ef0e97c8beeb6d07a6b3f3605d93133`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ComposeEnvironments

## Description
Cause 1 of 1: ComposeEnvironments, called with two PROCESSED application versions whose source bundles each carry only a doc-conformant `env.yaml` environment manifest (AWSConfigurationTemplateVersion 1.1.0.0, EnvironmentName ending in `+`, a WebServer/Standard tier and a Worker/SQS-HTTP tier, SolutionStack taken from ListAvailableSolutionStacks, EnvironmentLinks wiring the web tier to the worker tier, OptionSettings for EnvironmentType / ServiceRole / IamInstanceProfile), returns a non-empty `Environments` list — one member per supplied VersionLabel — where the recorded AWS behavior is HTTP 200 with `Environments: []` and no environment created under the application.

PRIOR WORK, read before touching the handler. This is the symptom of resolved issue 20261123T222354Z-elasticbeanstalk-elastic-beanstalk-composeenvironments-materializes-environments-for-a-two.md, which was CLOSED WITH ITS CAUSE DECLINED: the replayed symptom was left in place deliberately, because no property a handler can read was found that separates this recording from the other recorded ComposeEnvironments runs in the same corpus that AWS does compose. It is re-filed here only because the replay still fails and a resolved issue does not carry this check's replay key; it is not new evidence. It also stands in direct tension with open issue 20261123T140417Z-elasticbeanstalk-elastic-beanstalk-composeenvironments-is-a-no-op-it.md, which is grounded in three OTHER checks (e2e-compose-environments-then-terminate-cleans-up, compose-environments-creates-envs-from-versions-lifecycle, e2e-compose-environments-from-templates-produces-linked-envs) and asks for the opposite — that one environment per version label be materialized. Inverting one to satisfy the other is not a fix; either a discriminator grounded in a readable manifest/version/solution-stack property is found, or this report is declined again for the same stated reason.

AWS: Smithy (services/service-com-amazonaws-elasticbeanstalk-awselasticbeanstalkservice) models ComposeEnvironments with input ApplicationName (@length 1..100), GroupName (@length 1..19), VersionLabels (list) and output EnvironmentDescriptionsMessage{Environments: EnvironmentDescriptionsList, NextToken}; its only modeled errors are InsufficientPrivilegesException (403) and TooManyEnvironmentsException (400). The model documents the compose workflow but says nothing about a manifest set of this shape being withheld, so for that part this is recorded behavior only: the recording shows ComposeEnvironments answering HTTP 200 with `Environments: []` and a following DescribeEnvironments(ApplicationName=<app>, EnvironmentNames=[<front-prefix>-<GroupName>, <worker-prefix>-<GroupName>], IncludeDeleted=False) answering `Environments: []`. No error code and no non-2xx is involved — the status and the response shape are right, the CONTENT of Environments is not.

Emulator: the call answers HTTP 200 with one EnvironmentDescription per supplied VersionLabel, so the check's `assert composed == {}` fails with a two-entry VersionLabel -> EnvironmentName mapping. The environment names, the version labels and the group suffix in the replay are per-run values the check generates (`front-<group>` / `worker-<group>`, group = 's2r' + 8 random hex); the requirement is the CARDINALITY — zero members — not those literal strings.

Why: a client that composes an environment group is told two group members exist that real AWS never created. It polls, configures, links and later terminates environments that do not exist on AWS; conversely a client written against AWS's withdrawal (empty list = nothing composed) sees phantom members here, and any orchestration that branches on whether ComposeEnvironments produced members takes the wrong branch.

Fix: the ComposeEnvironments handler and its environment state in the elasticbeanstalk service crate — specifically the decision of WHICH supplied version labels/manifests are composed at all, and the construction of the response's Environments list, which must stay empty (and leave DescribeEnvironments empty under the manifest-derived names) for the manifest set recorded here, without un-composing the manifest sets the three checks of 20261123T140417Z record AWS composing.

Traces: elasticbeanstalk.e2e-compose-multi-tier-application-environments.

## Evidence
Check elasticbeanstalk.e2e-compose-multi-tier-application-environments [mismatch]:
  AssertionError: ComposeEnvironments reported environments for a doc-conformant two-tier manifest set ({'front-s2rdbb85152': 'front-s2rdbb85152', 'worker-s2rdbb85152': 'worker-s2rdbb85152'}) -- if this stack's ComposeEnvironments support has been restored, this check's contract (asserting the withdrawal) is stale and should be rewritten back to asserting the linked environments themselves reach Ready with EnvironmentLinks wired.
  (raised at check_elasticbeanstalk_e2e_compose_multi_tier_application_environments.py line 108, `assert composed == {}`)

Expected (AWS, recorded): ComposeEnvironments -> HTTP 200, `Environments: []`, NextToken absent; then DescribeEnvironments(ApplicationName=<app>, EnvironmentNames=['<front-prefix>-<GroupName>', '<worker-prefix>-<GroupName>'], IncludeDeleted=False) -> `Environments: []`.
Actual (emulator): HTTP 200 with two EnvironmentDescription members, one per supplied VersionLabel.

Positive control inside the same check: eb.create_environment on the IDENTICAL SolutionStack with the IDENTICAL IamInstanceProfile / ServiceRole option settings reached Status 'Ready' on the emulator, so the divergence is isolated to the ComposeEnvironments manifest path and not to environment creation generally.

The quoted names/labels are per-run generated values; only the zero cardinality of Environments (and of the subsequent DescribeEnvironments read) is the requirement.

## Steps to Reproduce
1. eb.create_application(ApplicationName=APP).
2. bucket = eb.create_storage_location()['S3Bucket']; stack = a Node.js / Amazon Linux 2023 entry from eb.list_available_solution_stacks().
3. Optionally create an EC2 instance profile and an elasticbeanstalk service role via IAM (the check degrades gracefully if IAM is unreachable).
4. Build two zips, each containing only an `env.yaml`:
   - front: AWSConfigurationTemplateVersion: 1.1.0.0 / SolutionStack: <stack> / EnvironmentName: 'front+' / EnvironmentTier {Name: WebServer, Type: Standard} / EnvironmentLinks { "WORKERQUEUE": "worker+" } / OptionSettings (aws:elasticbeanstalk:environment EnvironmentType=SingleInstance [+ ServiceRole], aws:autoscaling:launchconfiguration IamInstanceProfile).
   - worker: same header, EnvironmentName: 'worker+', EnvironmentTier {Name: Worker, Type: SQS/HTTP}, same IAM option settings.
   s3.put_object each zip into that bucket.
5. eb.create_application_version(ApplicationName=APP, VersionLabel='front-<group>' / 'worker-<group>', SourceBundle={S3Bucket,S3Key}, Process=True) and poll describe_application_versions until Status is PROCESSED.
6. Positive control: eb.create_environment(ApplicationName=APP, EnvironmentName=<ctrl>, SolutionStackName=stack, OptionSettings=[EnvironmentType=SingleInstance, InstanceType=t3.micro, IamInstanceProfile, ServiceRole]) and wait for Status 'Ready'.
7. resp = eb.compose_environments(ApplicationName=APP, GroupName='<group>', VersionLabels=['front-<group>', 'worker-<group>'])
   -> AWS (recorded): resp['Environments'] == []
   -> emulator: resp['Environments'] has one entry per version label.
8. eb.describe_environments(ApplicationName=APP, EnvironmentNames=['front-<group>', 'worker-<group>'], IncludeDeleted=False)
   -> AWS (recorded): 'Environments' == [].

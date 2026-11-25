Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.e2e-compose-environments-then-terminate-cleans-up elasticbeanstalk.compose-environments-creates-envs-from-versions-lifecycle elasticbeanstalk.e2e-compose-environments-from-templates-produces-linked-envs
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.e2e-compose-environments-then-terminate-cleans-up|run#31|AssertionError|assert len(env_descs) == 2, (
Issue-Key: elasticbeanstalk.compose-environments-creates-envs-from-versions-lifecycle|_wait_all_ready#26|Timeout|time.sleep(interval_s)
Issue-Key: elasticbeanstalk.e2e-compose-environments-from-templates-produces-linked-envs|_discover_composed_env_name#32|Timeout|time.sleep(interval_s)
Issue-Commit: 94f7b27553b727586aac9de0add3c17de28dbaa9

# Elastic Beanstalk ComposeEnvironments is a no-op: it answers 200 with an empty Environments list and creates no environment from each version's env.yaml manifest (1 cause)

> _Note: this issue was identified against the code base at commit `94f7b27553b727586aac9de0add3c17de28dbaa9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ComposeEnvironments

## Description
Cause 1 of 1: ComposeEnvironments accepts the request, returns HTTP 200 with `Environments: []`, and creates no environment at all. No environment ever appears under the application afterwards either, so every follow-up read (DescribeEnvironments, DescribeConfigurationSettings) sees nothing.

AWS: Smithy (services/service-com-amazonaws-elasticbeanstalk-awselasticbeanstalkservice) models ComposeEnvironments with input ApplicationName (@length 1..100), GroupName (@length 1..19) and VersionLabels (list), output EnvironmentDescriptionsMessage{Environments: EnvironmentDescriptionsList, NextToken}, errors InsufficientPrivilegesException (403) and TooManyEnvironmentsException (400). The modeled documentation of VersionLabels: "A list of version labels, specifying one or more application source bundles that belong to the target application. Each source bundle must include an environment manifest that specifies the name of the environment and the name of the solution stack to use, and optionally can specify environment links to create." So AWS creates (or updates) ONE environment per supplied version label, taking each environment's name, solution stack, tier and OptionSettings from the `env.yaml` manifest inside that version's source bundle, and names it `<manifest EnvironmentName prefix>-<GroupName>` when the manifest's EnvironmentName ends in `+` (group-name suffixing). Each composed environment is a first-class environment: it goes Launching -> Ready, is listed by DescribeEnvironments with its ApplicationName, VersionLabel, EnvironmentArn and Tier{Name,Type} taken from the manifest's EnvironmentTier, carries the manifest's OptionSettings in DescribeConfigurationSettings, and can be terminated individually with TerminateEnvironment. The response's Environments list may be empty or partial because materialization is asynchronous, but the environments must subsequently appear.

Emulator: the call succeeds (HTTP 200, Environments empty) and nothing is ever created. Replay evidence below: after two PROCESSED application versions each carrying an `env.yaml`, ComposeEnvironments returned `{'Environments': [], ...}` with HTTPStatusCode 200 and polling DescribeEnvironments for the application returned `[]`, so the checks that then wait for the composed environments to reach Ready simply run out of time.

Why: a client that composes an environment group gets a success it cannot act on. It has no environment name to poll, nothing to terminate, and no configuration to read, so any workflow built on ComposeEnvironments (the documented way to stand up a linked group of web/worker environments from env.yaml manifests) silently produces nothing while reporting success. The manifest-derived fields (EnvironmentName suffixed with the group name, EnvironmentTier Name/Type, SolutionStack, OptionSettings) are never honoured anywhere.

Fix: the ComposeEnvironments handler and its state in the elasticbeanstalk service crate. It must read each named application version's source bundle, parse the `env.yaml` environment manifest (EnvironmentName with `+` group suffixing, EnvironmentTier, SolutionStack, OptionSettings), create one environment per version label in the named group, register it in the environment state so DescribeEnvironments/DescribeConfigurationSettings/TerminateEnvironment see it, and drive it Launching -> Ready. No existing open issue covers this: 20261107T192913Z-elasticbeanstalk-...-no-data-plane and 20261109T144630Z-elasticbeanstalk-...-publishes-environment-cname cover an existing environment's data plane and published addresses, not the absence of the environments ComposeEnvironments must create.

Traces: elasticbeanstalk.e2e-compose-environments-then-terminate-cleans-up (the explicit assertion, with the 200/empty-list response quoted), plus elasticbeanstalk.compose-environments-creates-envs-from-versions-lifecycle and elasticbeanstalk.e2e-compose-environments-from-templates-produces-linked-envs, which both exhaust their poll waiting for an environment that is never created.

## Evidence
Check elasticbeanstalk.e2e-compose-environments-then-terminate-cleans-up:
  AssertionError: expected 2 composed environments, got []; stack='64bit Amazon Linux 2023 v4.7.0 running Python 3.14'; full ComposeEnvironments response={'Environments': [], 'ResponseMetadata': {'RequestId': '...', 'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'text/xml', 'content-length': '302', ...}, 'RetryAttempts': 0}}

Expected (AWS): two environments, one per supplied VersionLabel, named '<manifest EnvironmentName prefix>-<GroupName>' (manifest prefixes 'cmpa'/'cmpb'), each listed by DescribeEnvironments with ApplicationName = the composed application, the manifest's EnvironmentTier as Tier{Name,Type}, the manifest's OptionSettings in DescribeConfigurationSettings, and each individually terminable via TerminateEnvironment after which it leaves the IncludeDeleted=False view.
Actual (emulator): HTTP 200 with Environments = [], and DescribeEnvironments for the application keeps returning [] indefinitely.

Check elasticbeanstalk.compose-environments-creates-envs-from-versions-lifecycle: 'Check timed out after 60.0s' inside _wait_all_ready, which polls DescribeEnvironments(ApplicationName=...) until every requested VersionLabel has a Ready environment with Tier WebServer/Standard and Worker/SQS-HTTP from its manifest; no environment ever appears.

Check elasticbeanstalk.e2e-compose-environments-from-templates-produces-linked-envs: 'Check timed out after 60.0s' inside _discover_composed_env_name, which polls DescribeEnvironments for exactly one composed environment named '<env_prefix>-<group_name>' and then asserts its aws:autoscaling:launchconfiguration/InstanceType is the manifest-inherited value; count stays at 0.

The status (200) and the shape of the response are not themselves wrong — AWS may answer with an empty or partial Environments list — the defect is that no environment is ever materialized, so the asynchronous completion never happens.

## Steps to Reproduce
1. eb.create_application(ApplicationName=APP)
2. bucket = eb.create_storage_location()['S3Bucket']
3. Build two zip bundles, each containing an 'env.yaml' manifest with AWSConfigurationTemplateVersion: 1.1.0.0, EnvironmentName: '<prefix>+', EnvironmentTier (Name/Type), SolutionStack set to a stack from eb.list_available_solution_stacks(), and OptionSettings; s3.put_object them into that bucket.
4. eb.create_application_version(ApplicationName=APP, VersionLabel=L, SourceBundle={...}, Process=True) for each bundle, and poll describe_application_versions until Status is PROCESSED.
5. resp = eb.compose_environments(ApplicationName=APP, GroupName=G, VersionLabels=[L1, L2])
   -> emulator: HTTP 200, resp['Environments'] == []
6. Poll eb.describe_environments(ApplicationName=APP, IncludeDeleted=False)
   -> emulator: 'Environments' stays [] forever.
   -> AWS: two environments named '<prefix1>-G' and '<prefix2>-G' appear, go Launching -> Ready, carry the manifests' Tier and OptionSettings, and can each be terminated with eb.terminate_environment(EnvironmentName=...).

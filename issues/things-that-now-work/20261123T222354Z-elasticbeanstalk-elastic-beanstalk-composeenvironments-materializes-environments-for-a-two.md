Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: elasticbeanstalk.e2e-compose-multi-tier-application-environments
Issue-Shape-Id: com.amazonaws.elasticbeanstalk#AWSElasticBeanstalkService
Issue-Key: elasticbeanstalk.e2e-compose-multi-tier-application-environments|run#30|AssertionError|assert composed == {}, (
Issue-Commit: dac70573d94ca20489dac9623e5401c47c25359d

# Elastic Beanstalk ComposeEnvironments materializes environments for a two-tier env.yaml manifest set where AWS answers with an empty Environments list and creates none

> **CLOSED WITH ITS ONE CAUSE DECLINED — THE REPLAYED SYMPTOM REMAINS.** This report sits
> under `things-that-now-work/` because it has been worked and needs no further fixer, not
> because the recorded check passes: ComposeEnvironments STILL composes one environment per
> supplied version label for the manifest set below, so the check's `assert composed == {}`
> still fails. What the emulator owns was verified correct against the model and pinned by
> test; why the recorded empty answer cannot be turned into a rule is stated under
> "## Resolution" at the end of this file. Do not re-open it as a service defect without
> new recorded evidence of the kind named there.

> _Note: this issue was identified against the code base at commit `dac70573d94ca20489dac9623e5401c47c25359d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ComposeEnvironments

## Description
Cause 1 of 1: ComposeEnvironments, called with two PROCESSED application versions whose source bundles each carry a doc-conformant `env.yaml` environment manifest (AWSConfigurationTemplateVersion 1.1.0.0, EnvironmentName ending in `+`, EnvironmentTier WebServer/Standard and Worker/SQS-HTTP, SolutionStack taken from ListAvailableSolutionStacks, EnvironmentLinks wiring the web tier to the worker tier, OptionSettings), reports composed environments in its response where the recorded AWS behavior is a 200 with an empty `Environments` list and NO environment created under the application.

AWS: Smithy (services/service-com-amazonaws-elasticbeanstalk-awselasticbeanstalkservice) models ComposeEnvironments with input ApplicationName (@length 1..100), GroupName (@length 1..19), VersionLabels (list) and output EnvironmentDescriptionsMessage{Environments: EnvironmentDescriptionsList, NextToken}; its only modeled errors are InsufficientPrivilegesException (403) and TooManyEnvironmentsException (400). The model does not state whether a manifest set of this shape is actually composed, so for that part this is recorded behavior only: the recording for this check shows ComposeEnvironments answering HTTP 200 with `Environments: []`, and a subsequent DescribeEnvironments(ApplicationName=<app>, EnvironmentNames=[<prefix1>-<GroupName>, <prefix2>-<GroupName>], IncludeDeleted=False) returning `Environments: []` — i.e. AWS accepts the call and composes nothing for this two-tier, EnvironmentLinks-bearing manifest set on a current solution stack, while plain CreateEnvironment on the SAME solution stack and the SAME IAM instance profile / service role reaches Ready (the check's positive control, which passes against the emulator too, so the account/stack path is healthy and the divergence is specific to the ComposeEnvironments manifest path).

Emulator: the call answers 200 but reports one environment per supplied VersionLabel. Replayed response mapped VersionLabel -> EnvironmentName as two entries, `{'<front-version-label>': '<front-env-name>', '<worker-version-label>': '<worker-env-name>'}` (in the replay both keys and values were the `front-<group>` / `worker-<group>` names, the group being the per-run GroupName), so `Environments` was non-empty where the recording has it empty, and the check's `assert composed == {}` fails.

Why: a client that composes an environment group is told two environments exist that real AWS would never have created. It will poll, configure, link and later terminate environments that do not exist on AWS, and conversely a client written against AWS's withdrawal (treating the empty list as "nothing was composed") sees phantom group members on the emulator. Any parity test or orchestration that branches on whether ComposeEnvironments produced members takes the wrong branch.

NOTE for the fixer — there is a direct tension with open issue 20261123T140417Z-elasticbeanstalk-elastic-beanstalk-composeenvironments-is-a-no-op-it.md, which is grounded in three OTHER checks (e2e-compose-environments-then-terminate-cleans-up, compose-environments-creates-envs-from-versions-lifecycle, e2e-compose-environments-from-templates-produces-linked-envs) and asks for the opposite: that ComposeEnvironments materialize one environment per version label. That issue does not carry this check's replay key and therefore does not cover it. Both recordings are AWS behavior, so the handler must distinguish the manifest/version shapes that AWS composes from the shape recorded here (a two-tier WebServer+Worker set carrying EnvironmentLinks), which AWS leaves uncomposed with an empty Environments list; satisfying one issue by inverting the other is not a fix.

Fix: the ComposeEnvironments handler and its environment state in the elasticbeanstalk service crate — specifically the decision of which supplied version labels/manifests are composed at all, and the construction of the response's Environments list, which must stay empty (and leave DescribeEnvironments empty under the manifest-derived names) for the manifest set recorded here.

Traces: elasticbeanstalk.e2e-compose-multi-tier-application-environments.

## Evidence
Check elasticbeanstalk.e2e-compose-multi-tier-application-environments:
  AssertionError: ComposeEnvironments reported environments for a doc-conformant two-tier manifest set ({'front-s2r62f7513e': 'front-s2r62f7513e', 'worker-s2r62f7513e': 'worker-s2r62f7513e'}) -- if this stack's ComposeEnvironments support has been restored, this check's contract (asserting the withdrawal) is stale ...
  (raised at check_elasticbeanstalk_e2e_compose_multi_tier_application_environments.py line 108, `assert composed == {}`)

Expected (AWS, recorded): ComposeEnvironments -> HTTP 200 with `Environments: []` (NextToken absent), and DescribeEnvironments(ApplicationName=<app>, EnvironmentNames=['<front-prefix>-<GroupName>', '<worker-prefix>-<GroupName>'], IncludeDeleted=False) -> `Environments: []`. No error code and no non-2xx is involved: the status and the response shape are correct, the CONTENT of Environments is not.
Actual (emulator): HTTP 200 with two EnvironmentDescription members, one per supplied VersionLabel. The names, group suffix and version labels quoted above are per-run values generated by the check (`front-<group>` / `worker-<group>`, group = 's2r' + 8 random hex); the requirement is the CARDINALITY — zero members — not those literal strings.

Positive control in the same check: eb.create_environment on the identical SolutionStack with the identical IamInstanceProfile / ServiceRole option settings reached Status 'Ready' on the emulator, so the failure is isolated to the ComposeEnvironments manifest path and not to environment creation generally.

## Steps to Reproduce
1. eb.create_application(ApplicationName=APP).
2. bucket = eb.create_storage_location()['S3Bucket']; stack = a Node.js / Amazon Linux 2023 entry from eb.list_available_solution_stacks().
3. Create an EC2 instance profile and an elasticbeanstalk service role via IAM (optional; the check degrades gracefully if IAM is unreachable).
4. Build two zips, each containing only an `env.yaml`:
   - front: AWSConfigurationTemplateVersion: 1.1.0.0 / SolutionStack: <stack> / EnvironmentName: 'front+' / EnvironmentTier {Name: WebServer, Type: Standard} / EnvironmentLinks { "WORKERQUEUE": "worker+" } / OptionSettings (aws:elasticbeanstalk:environment EnvironmentType=SingleInstance [+ ServiceRole], aws:autoscaling:launchconfiguration IamInstanceProfile).
   - worker: same header, EnvironmentName: 'worker+', EnvironmentTier {Name: Worker, Type: SQS/HTTP}, same IAM option settings.
   s3.put_object each zip into that bucket.
5. eb.create_application_version(ApplicationName=APP, VersionLabel='front-<group>'/'worker-<group>', SourceBundle={S3Bucket,S3Key}, Process=True) and poll describe_application_versions until Status is PROCESSED.
6. Positive control: eb.create_environment(ApplicationName=APP, EnvironmentName=<ctrl>, SolutionStackName=stack, OptionSettings=[EnvironmentType=SingleInstance, InstanceType=t3.micro, IamInstanceProfile, ServiceRole]) and wait for Status 'Ready'.
7. resp = eb.compose_environments(ApplicationName=APP, GroupName='<group>', VersionLabels=['front-<group>', 'worker-<group>'])
   -> AWS (recorded): resp['Environments'] == []
   -> emulator: resp['Environments'] has one entry per version label.
8. eb.describe_environments(ApplicationName=APP, EnvironmentNames=['front-<group>', 'worker-<group>'], IncludeDeleted=False)
   -> AWS (recorded): 'Environments' == [].

## Resolution

DECLINED: what the emulator answers IS the modelled behaviour, and no rule that is not
keyed to this one recording's instance produces the recorded empty answer.

The model (`smithy_query` on `com.amazonaws.elasticbeanstalk#ComposeEnvironments`) states
that the operation "create[s] or update[s] a group of environments that each run a separate
component of a single application", "takes a list of version labels that specify
application source bundles for each of the environments to create or update", that "the
name of each environment and other required information must be included in the source
bundles in an environment manifest named env.yaml", that each bundle's manifest
"optionally can specify environment links to create", and that `GroupName` is to be given
"only if the environment name defined in each target environment's manifest ends with a +
(plus) character". The request recorded here — `+`-named manifests, a GroupName, a web and
a worker tier, links wiring the two, a published solution stack — is that documented
workflow exactly, and `services/service-com-amazonaws-elasticbeanstalk-awselasticbeanstalkservice/src/ops/compose_environments.rs`
implements it: one environment per version label whose manifest names one, named by the
manifest with a `+` completed by the group, carrying that manifest's solution stack, tier,
option settings and links with each link target completed the same way.

Why no discriminator exists. The same operation's other recorded runs in this corpus — the
three carried by `issues/things-that-do-not-work/20261123T140417Z-elasticbeanstalk-elastic-beanstalk-composeenvironments-is-a-no-op-it.md`
(`e2e-compose-environments-then-terminate-cleans-up`,
`compose-environments-creates-envs-from-versions-lifecycle`,
`e2e-compose-environments-from-templates-produces-linked-envs`) and
`compose-environments-insufficient-privileges-denied` — show AWS COMPOSING environments for
manifest sets that share every property of this one that a handler can read: the same
`AWSConfigurationTemplateVersion: 1.1.0.0`, the same `+`-terminated names with a GroupName,
the same WebServer/Standard plus Worker/SQS-HTTP tier pair, successfully PROCESSED versions,
bundles whose only entry is the manifest, and current Amazon Linux 2023 stacks taken from
`ListAvailableSolutionStacks`. The only property left is the presence of `EnvironmentLinks`,
which the model documents as something this operation CREATES — so making a link-bearing
manifest set compose nothing turns the documented primary workflow into a success-shaped
discarded write (200 with an empty list and no stored record), which is why two attempts at
such a rule (first "any declared link composes nothing", then "a link's target must already
be running") were rejected at review. The recorded empty answer is AWS withholding a
documented capability for a reason this recording does not expose; the check that owns it
says as much, asserting "the withdrawal" and calling its own contract stale if support "has
been restored".

What was verified and pinned instead
(`services/service-com-amazonaws-elasticbeanstalk-awselasticbeanstalkservice/tests/all/composed_environment_links.rs`,
beside the composition rules the `composed_environments` suite already pins):

* A composition creates the links its manifests declare, over a table that varies which
  manifest declares them, how many it declares, whether a target is a group template or a
  plain name, and whether the target is composed by the same call or by an earlier one.
* Each link is reported by the composition AND stored on the environment, read back through
  `DescribeEnvironments`, with every expected environment name and link target written out
  as literal table data rather than recomputed from the service.
* A manifest that declares no link composes an environment that reports none.

What would have to change for this report to be actionable: recorded evidence of a property
a handler can read — a manifest member, a version attribute, a platform-branch or
solution-stack attribute — that separates this run from the composing runs above. With such
a property the rule becomes implementable in
`src/ops/compose_environments.rs`; without it, any change that satisfies this check breaks
those three.

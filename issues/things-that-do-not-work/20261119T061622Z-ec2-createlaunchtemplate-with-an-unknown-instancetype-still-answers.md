Issue-Source: trace-sweep-divergence
Issue-Trace: asglf-lt-contents-refusals
Issue-Step: task#14
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: asglf-lt-contents-refusals
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: ec2|CreateLaunchTemplate|2xx->4xx|status
Issue-Related: 20261107T071147Z-ec2-launch-template-writes-refuse-an-unknown-instance.md
Issue-Commit: 744a403bcd89e11c6cc0112b0c3d4fd5ce52e365

# ec2: CreateLaunchTemplate with an unknown instanceType still answers 400 instead of creating the template with a ValidationWarning

> _Note: this issue was identified against the code base at commit `744a403bcd89e11c6cc0112b0c3d4fd5ce52e365`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLaunchTemplate

## Description
One net-new root cause: EC2's CreateLaunchTemplate write path treats a non-existent instanceType in LaunchTemplateData as a hard refusal, where AWS creates the launch template and reports the problem advisorily in the response's warning element.

AWS: Smithy (com.amazonaws.ec2#AmazonEC2) models CreateLaunchTemplate with input {LaunchTemplateName required, LaunchTemplateData required (RequestLaunchTemplateData), VersionDescription, TagSpecifications, ClientToken, DryRun} and output members LaunchTemplate (xmlName launchTemplate) and Warning (xmlName warning, target ValidationWarning, whose Errors member has xmlName errorSet); the operation binds NO modeled errors for this case. So AWS answers HTTP 200, creates the template (defaultVersionNumber 1, latestVersionNumber 1, createdBy the calling principal's ARN, operator.managed false, the request's TagSpecifications echoed in tagSet) and returns warning.errorSet.item with code 'InvalidInstanceType' and a message whose fixed text is "The following supplied instance types do not exist: [" + the instance type(s) named in the request + "]".
Emulator: HTTP 400 with no CreateLaunchTemplateResponse body at all - the template is never created, and the ValidationWarning element the model declares is never produced.
Why: AWS's contract is that an unresolvable instance type in launch-template data is advisory, not fatal: callers (Auto Scaling trajectories, CloudFormation, deployment tooling) legitimately create the template and then read warning.errorSet. Refusing aborts the whole workflow - every later step that references the template id/name fails too - and a client that inspects the warning element gets nothing to inspect.
Fix: the LaunchTemplateData validation in services/service-com-amazonaws-ec2-amazonec2/src/ops/create_launch_template.rs - accept the unknown instance type, create the template, and have the output serializer emit the warning/errorSet member instead of refusing.
Traces: asglf-lt-contents-refusals#task#14.

Note: the open issue 20261107T071147Z-ec2-launch-template-writes-refuse-an-unknown-instance.md describes this same fault and names this fixture step, but the replay reports this cluster as unowned (no owning issue carries its replay key, and no stub blocks it), so it is still diverging and is filed here as net-new. The ids, names and timestamps in the recorded body are per-run values, not requirements.

## Evidence
Replay of fixture corpus simparity-wire-aws-v1 against the emulator built from this checkout.

asglf-lt-contents-refusals#task#14, ec2 CreateLaunchTemplate [mismatch], cluster affects 1 step, owned_by empty, stub_path empty:
- status: expected 200, got 400
- $.CreateLaunchTemplateResponse: missing - expected {requestId, launchTemplate {createTime, createdBy 'arn:aws:iam::000000000000:user/emulator', defaultVersionNumber '1', latestVersionNumber '1', launchTemplateId, launchTemplateName 'sim2real-probe-asglf-lt-5e08b7d3-btype', operator {managed 'false'}, tagSet {item {key 'sim2real', value 'asglf'}}}, warning {errorSet {item {code 'InvalidInstanceType', message 'The following supplied instance types do not exist: [t3.bogus]'}}}}, got None.

The recorded message's fixed text is "The following supplied instance types do not exist: [" ... "]"; 't3.bogus' is the instance type taken from the request. The template id, account id and timestamps in the recorded body are per-run values.

## Steps to Reproduce
1. Replay the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout.
2. Inspect asglf-lt-contents-refusals#task#14: a CreateLaunchTemplate whose LaunchTemplateData.InstanceType names a non-existent instance type (e.g. t3.bogus).
3. The call must answer HTTP 200 with launchTemplate created (defaultVersionNumber 1, latestVersionNumber 1) and warning.errorSet.item {code 'InvalidInstanceType', message 'The following supplied instance types do not exist: [<requested type>]'}; the emulator answers HTTP 400 with no body.

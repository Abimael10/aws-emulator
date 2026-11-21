Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override
Issue-Step: task#10
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override ec2-ibp-traj-launch-template-from-failed-ami-poll
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|CreateLaunchTemplate|2xx->4xx|$.CreateLaunchTemplateResponse
Issue-Key: ec2|DescribeLaunchTemplates|2xx->2xx|$.DescribeLaunchTemplatesResponse.launchTemplates.item.launchTemplateId
Issue-Related: 20261119T061622Z-ec2-createlaunchtemplate-with-an-unknown-instancetype-still-answers.md
Issue-Commit: 17b8d6926fcb04a314774f8dcf7dba28eb7ebe93

# ec2 CreateLaunchTemplate refuses a network-interface spec without DeviceIndex that AWS accepts

> _Note: this issue was identified against the code base at commit `17b8d6926fcb04a314774f8dcf7dba28eb7ebe93`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateLaunchTemplate

## Description
2 causes, both rooted in CreateLaunchTemplate applying a launch-time network-interface constraint at template-creation time, so a template AWS stores is never created and every later read of it is wrong.

Cause 1 of 2: CreateLaunchTemplate answers 400 for a LaunchTemplateData whose NetworkInterfaces entry carries AssociatePublicIpAddress but no DeviceIndex.
AWS: Smithy model com-amazonaws-ec2-amazonec2.json - CreateLaunchTemplate input LaunchTemplateData.NetworkInterfaces targets LaunchTemplateInstanceNetworkInterfaceSpecificationRequest, whose DeviceIndex member is NOT @required; its documentation states that "If you create a launch template that includes secondary network interfaces but not a primary network interface, then you must add a primary network interface as a launch parameter when you launch an instance from the template", i.e. the device-index requirement is a LAUNCH-time rule, not a template-creation rule. AWS therefore returns 200 with a CreateLaunchTemplateResponse carrying launchTemplate{launchTemplateId, launchTemplateName, createTime, createdBy, defaultVersionNumber=1, latestVersionNumber=1, operator{managed=false}}. The recorded response confirms exactly that 200 body shape. The deferred check is visible in the SAME fixture: the later UpdateAutoScalingGroup step is the step AWS refuses, with Sender/InvalidQueryParameter and the message "Incompatible launch template: Each network interface requires a device index."
Emulator: the create itself is refused - HTTP 400 and no CreateLaunchTemplateResponse at all, so no launch template, no id and no version 1 are ever produced.
Why: a client that builds a template and only afterwards discovers the incompatibility (the normal AWS sequence, and the one Auto Scaling console/CLI flows follow) cannot proceed at all: it never gets a launchTemplateId, so it cannot create versions, cannot attach the template to an Auto Scaling group, and cannot observe AWS's real refusal, which belongs to UpdateAutoScalingGroup/RunInstances. Validation that is too early is as wrong as validation that is missing - it moves the error to an operation AWS answers 200.
Fix: CreateLaunchTemplate's request validation - drop the DeviceIndex presence check from template creation and keep it on the launch/attach path (ec2 CreateLaunchTemplate handler and its LaunchTemplateData validator).
Traces: ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override#task#10 (cluster also covers ec2-ibp-traj-launch-template-from-failed-ami-poll#task#7).

Cause 2 of 2: DescribeLaunchTemplates reports a launch-template id that is not the id of the template the fixture created.
AWS: recorded behavior only - DescribeLaunchTemplatesResponse.launchTemplates.item.launchTemplateId must be the id minted by the CreateLaunchTemplate call that created the template (the recording binds it from that earlier step). The requirement is identity: the id read back equals the id returned at create.
Emulator: the read-back id is an id minted by the emulator's own counter for a different template, because the create that should have produced the asserted id was refused (Cause 1); the step's asserted id never comes into existence. Recorded divergence: $.DescribeLaunchTemplatesResponse.launchTemplates.item.launchTemplateId value_mismatch, got a counter-minted lt-0000000000000000x id instead of the created template's id.
Why: a client that creates a template and then lists templates cannot correlate the two - the id it holds is absent from the listing, so version management and ASG attachment by id both fail.
Fix: ec2 CreateLaunchTemplate handler/state (same root as Cause 1) plus DescribeLaunchTemplates' projection off the launch-template store; re-replay this step after Cause 1 is fixed to confirm the id round-trips.
Traces: ec2-ibp-traj-launch-template-from-failed-ami-poll#task#10.

Related but NOT covering these: 20261119T061622Z-ec2-createlaunchtemplate-with-an-unknown-instancetype-still-answers.md (unknown instanceType) and 20261119T122355Z-ec2-refusal-fidelity-7-net-new-causes-createlaunchtemplate.md are about different CreateLaunchTemplate inputs and do not carry these replay keys.

## Evidence
Cause 1 - fixture ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override, step task#10, operation CreateLaunchTemplate.
  Expected: status 200, body $.CreateLaunchTemplateResponse = {requestId, launchTemplate{createTime, createdBy, defaultVersionNumber '1', latestVersionNumber '1', launchTemplateId, launchTemplateName, operator{managed 'false'}}}.
  Actual: status 400; $.CreateLaunchTemplateResponse missing (got None).
  Cluster also affects ec2-ibp-traj-launch-template-from-failed-ami-poll#task#7.
  Same fixture, step task#13 (UpdateAutoScalingGroup) shows where AWS DOES refuse: 400 Sender/InvalidQueryParameter "Incompatible launch template: Each network interface requires a device index."

Cause 2 - fixture ec2-ibp-traj-launch-template-from-failed-ami-poll, step task#10, operation DescribeLaunchTemplates.
  Expected: launchTemplates.item.launchTemplateId = the id returned by the fixture's CreateLaunchTemplate step.
  Actual: a counter-minted id (recorded run showed 'lt-000000000000000a1'), i.e. not the created template's id.

## Steps to Reproduce
1. Replay fixture ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override against the emulator and stop at step task#10: CreateLaunchTemplate with LaunchTemplateData.NetworkInterfaces[0] carrying AssociatePublicIpAddress and no DeviceIndex. Observe HTTP 400 instead of 200 + CreateLaunchTemplateResponse.
2. Continue the fixture to task#13 (UpdateAutoScalingGroup) to see that this is the step AWS refuses.
3. Replay fixture ec2-ibp-traj-launch-template-from-failed-ami-poll: step task#7 shows the same create refusal; step task#10 then shows DescribeLaunchTemplates returning a launchTemplateId other than the created template's id.

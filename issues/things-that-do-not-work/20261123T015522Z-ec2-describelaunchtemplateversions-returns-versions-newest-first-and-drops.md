Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override
Issue-Step: task#15
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeLaunchTemplateVersions|2xx->2xx|$.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[*].defaultVersion
Issue-Key: ec2|DescribeLaunchTemplateVersions|2xx->2xx|$.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[*].launchTemplateData.networkInterfaceSet.item.associatePublicIpAddress
Issue-Key: ec2|DescribeLaunchTemplateVersions|2xx->2xx|$.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[*].launchTemplateData.networkInterfaceSet.item.groupSet
Issue-Key: ec2|DescribeLaunchTemplateVersions|2xx->2xx|$.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[*].versionNumber
Issue-Commit: 6c5e437ac482b043e6839b0d94a89be6f95b8112

# ec2 DescribeLaunchTemplateVersions returns versions newest-first and drops network-interface members from the stored launch template data

> _Note: this issue was identified against the code base at commit `6c5e437ac482b043e6839b0d94a89be6f95b8112`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLaunchTemplateVersions

## Description
Cause 1 of 1 — DescribeLaunchTemplateVersions on a template with two versions answers 200, but the version list is ordered the wrong way and the per-version launchTemplateData loses network-interface members the client supplied when the version was created.
AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2/smithy-model): DescribeLaunchTemplateVersions output member LaunchTemplateVersions (xmlName launchTemplateVersionSet) of LaunchTemplateVersion{launchTemplateId, launchTemplateName, versionNumber(long), versionDescription, createTime, createdBy, defaultVersion(boolean - "Indicates whether the version is the default version"), launchTemplateData -> ResponseLaunchTemplateData}. AWS returns the set in ascending versionNumber order (version 1 first), flags exactly the template's default version with defaultVersion=true, and echoes the stored launch template data verbatim, including each networkInterfaceSet.item's groupSet (the security groups given as Groups) and associatePublicIpAddress. The recording is the authority for the ordering and for which members are echoed (recorded behavior only for ordering; the members themselves are modeled).
Emulator: the response carries the versions in descending order - item[0] is versionNumber 2 (defaultVersion false) and item[1] is versionNumber 1 (defaultVersion true), where AWS puts versionNumber 1 (defaultVersion true) at item[0] and versionNumber 2 (defaultVersion false) at item[1]. In the same response the default version's launchTemplateData.networkInterfaceSet.item.groupSet is missing (AWS emits the security group recorded on that interface specification) and the newer version's launchTemplateData.networkInterfaceSet.item.associatePublicIpAddress is missing (AWS emits it as 'true', the value the CreateLaunchTemplateVersion request set).
Why: a client that reads launchTemplateVersionSet positionally - Auto Scaling and launch tooling commonly take the first element, or diff item[0] against item[1] to show what changed - resolves the wrong version against the emulator, and because the echoed data silently drops associatePublicIpAddress and the interface's security groups, a round-trip read of the template reports a network configuration (no public IP association, no security group) that the template does not actually have. Validation built on that read (e.g. refusing to launch into a public subnet without a public IP) takes the opposite branch from AWS.
Fix: the DescribeLaunchTemplateVersions handler - order the version set by ascending versionNumber as AWS does and take defaultVersion from the template's recorded default version number - plus the ResponseLaunchTemplateData serializer/state for launch-template versions, which must round-trip every network-interface member it stored (groupSet from Groups, associatePublicIpAddress) rather than emitting the subset it recognises.
Traces: ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override#task#15.

Related open issues that do NOT carry this replay key and so do not cover this cluster: 20261102T114505Z-ec2-launch-template-read-validation-and-subnet-projection.md and 20261109T063048Z-ec2-5-net-new-divergences-launch-template-version.md.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override, step task#15 (divergence_key service=ec2|DescribeLaunchTemplateVersions|status=200->200), expected_status 200 / actual_status 200, verdict mismatch, affected_count 1:
  $.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[0].versionNumber: expected '1', got '2'
  $.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[0].defaultVersion: expected 'true', got 'false'
  $.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[0].launchTemplateData.networkInterfaceSet.item.groupSet: missing (expected {'groupId': '<security-group-id of the interface spec>'}, got None)
  $.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[1].versionNumber: expected '2', got '1'
  $.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[1].defaultVersion: expected 'false', got 'true'
  $.DescribeLaunchTemplateVersionsResponse.launchTemplateVersionSet.item[1].launchTemplateData.networkInterfaceSet.item.associatePublicIpAddress: missing (expected 'true', got None)
The generated sg-/lt- ids in the recording are per-run values; the requirement is the ascending versionNumber ordering, the defaultVersion flag landing on the template's default version, and the presence of the groupSet/associatePublicIpAddress members that the stored version data was created with.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1.
2. Inspect fixture ec2-asgssh-traj-template-public-ip-flag-shape-and-version-override, step task#15.
3. By hand: CreateLaunchTemplate with a NetworkInterfaces[0] specification that carries Groups=[<sg-id>], then CreateLaunchTemplateVersion for the same template with NetworkInterfaces[0].AssociatePublicIpAddress=true, leaving version 1 as the default; then call aws ec2 describe-launch-template-versions --launch-template-id <lt-id> --endpoint-url <emulator>.
4. Observe launchTemplateVersionSet arrives newest-first instead of ascending by versionNumber, item for version 1 has no launchTemplateData.networkInterfaceSet.item.groupSet, and the item for version 2 has no associatePublicIpAddress.

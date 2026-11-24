Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam__imagebuilder.ibp-distributed-amis-are-region-scoped-and-outlive-the-deleted-image
Issue-Key: ec2__iam__imagebuilder.ibp-distributed-amis-are-region-scoped-and-outlive-the-deleted-image|run#17|AssertionError|assert sorted(by_region) == sorted((HOME, DIST)) and len(amis) == 2, (
Issue-Commit: ee29b9ed80499ffda1e629bc10e5c9c45fea7a64

# ec2__iam__imagebuilder: a distribution configuration's second target Region produces no AMI — GetImage outputResources.amis lists only the build Region

> _Note: this issue was identified against the code base at commit `ee29b9ed80499ffda1e629bc10e5c9c45fea7a64`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetImage

## Description
Cause 1 of 1: Image Builder ignores every distribution entry other than the build Region, so an image built from a pipeline whose distribution configuration names two Regions reports a single output AMI and no AMI is created in the second Region. Failing check: ec2__iam__imagebuilder.ibp-distributed-amis-are-region-scoped-and-outlive-the-deleted-image.

AWS: Smithy (services/service-com-amazonaws-imagebuilder-imagebuilder) models Distribution{region (required), amiDistributionConfiguration{name, amiTags, ...}, ...} and OutputResources{amis: list<Ami>}, documenting amis as "The Amazon EC2 AMIs created by this image. The list contains one entry per AMI, including copies that distribution created in each target Amazon Web Services Region and account"; Ami carries region, image, name, state and accountId. So for a distribution configuration with one Distribution per target Region, a build that reaches status AVAILABLE answers GetImage with one amis entry per target Region, each with that Region's own AMI id (distinct across Regions), with the name its amiDistributionConfiguration.name resolves to (the {{ imagebuilder:buildDate }} tag expanded, never left literal) and the amiTags that entry configured; the same set appears in ListImagePipelineImages' imageSummaryList outputResources, each AMI is 'available' in its own Region's EC2 registry on a completed snapshot, and each AMI id is unknown to the other Region.

Emulator: after the build reaches AVAILABLE, GetImage(imageBuildVersionArn=...)['image']['outputResources']['amis'] holds one entry, for the build Region only; the Region named by the second Distribution entry has no AMI at all. The call succeeds — nothing is refused — so the caller is told distribution completed with a single output.

Why: cross-Region AMI distribution, the whole purpose of a distribution configuration, silently does nothing. A client that builds in one Region and launches from the copy in another gets no AMI id for that Region (or, worse, reuses the build Region's id), and a pipeline whose contract is 'one golden AMI per Region' cannot be verified or consumed. Because the image still reports AVAILABLE, no error surfaces anywhere.

Fix: the Image Builder image-build/distribution path in the imagebuilder service crate — on build completion it must execute every Distribution entry (creating the AMI and its snapshot in each target Region's EC2 state, applying that entry's amiDistributionConfiguration name template and amiTags) and record one Ami per target Region in OutputResources, which GetImage and ListImagePipelineImages then serve. Related open issues that do NOT cover this cause: 20261115T043558Z-ec2-iam-imagebuilder-image-builder-image-builds-never-run-startimagepipelineexecution-lands.md (builds land straight on AVAILABLE), 20261116T091224Z-ec2-iam-imagebuilder-the-ec2-image-catalog-is.md (the EC2 image catalog is not Region-scoped) and 20261117T002936Z-imagebuilder-image-builder-getimage-omits-modeled-image-fields-and.md (GetImage field coverage).

Traces: ec2__iam__imagebuilder.ibp-distributed-amis-are-region-scoped-and-outlive-the-deleted-image.

## Evidence
Check: ec2__iam__imagebuilder.ibp-distributed-amis-are-region-scoped-and-outlive-the-deleted-image.
Setup: CreateDistributionConfiguration(distributions=[{region: <build region>, amiDistributionConfiguration:{name: '<tok>-<region>-{{ imagebuilder:buildDate }}', amiTags:{Name, DistributionRegion:<region>, ...}}}, {region: <second region>, ...same shape...}]), bound to the pipeline; StartImagePipelineExecution; the build reaches status AVAILABLE.
Expected (AWS): GetImage outputResources.amis has two entries, one per target Region, with distinct 'image' ids; each AMI is 'available' in its own Region, named from the expanded template and tagged with that entry's amiTags; each id is unknown in the other Region.
Actual (emulator): AssertionError: output AMIs by Region ['us-east-1'], expected one each in ['us-east-1', 'us-east-2'] — a single amis entry for the build Region (the Region names are this run's values; the requirement is one entry per configured target Region).

## Steps to Reproduce
1. In the build Region, create a component, image recipe, infrastructure configuration and a distribution configuration with TWO distributions, each naming a different region and its own amiDistributionConfiguration name template and amiTags; create a pipeline over them.
2. StartImagePipelineExecution and poll GetImage until status is AVAILABLE.
3. Read image.outputResources.amis: AWS lists one Ami per configured target Region with distinct image ids; the emulator lists only the build Region's.
4. DescribeImages in the second Region for that Region's expected AMI id and DescribeSnapshots for its backing snapshot — AWS has them ('available' / 'completed'); the emulator created neither.

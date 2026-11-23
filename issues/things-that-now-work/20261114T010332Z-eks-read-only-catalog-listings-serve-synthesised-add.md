Issue-Source: trace-sweep-divergence
Issue-Trace: eks-catalog-describe-addon-versions-accuknox-kubearmor-eg-innovations-eg-universal-agent-operator
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eks-catalog-describe-addon-versions-accuknox-kubearmor-eg-innovations-eg-universal-agent-operator eks-catalog-describe-addon-versions-eks-node-monitoring-agent-vpc-cni eks-mbes-traj-addon-name-discovery eks-catalog-describe-cluster-versions eks-efsfips-traj-catalog-and-schema-discovery eks-eks-catalog-read-only-discovery
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|DescribeAddonVersions|2xx->2xx|$.addons[*]
Issue-Key: eks|DescribeClusterVersions|2xx->2xx|$.clusterVersions[*]
Issue-Key: eks|DescribeAddonConfiguration|2xx->2xx|$.configurationSchema
Issue-Related: 20261104T053622Z-eks-responses-omit-fields-aws-returns-cluster-vendedlogs.md
Issue-Related: 20261109T131212Z-eks-describeaddonversions-catalog-omits-further-marketplace-add-ons.md
Issue-Commit: 1abd47375f83d7e7585dbdf10baa9b4ff1f855bb

# eks read-only catalog listings serve synthesised add-on versions, cluster versions and configuration schemas

> _Note: this issue was identified against the code base at commit `1abd47375f83d7e7585dbdf10baa9b4ff1f855bb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAddonVersions

## Description
Five net-new root causes share one fault: the three read-only EKS catalogs (DescribeAddonVersions, DescribeClusterVersions, DescribeAddonConfiguration) are backed by synthesised data instead of the AWS catalog, and they ignore the request filters that select WHICH catalog rows to return. Every one of these clusters answers HTTP 200 — the status, the error class and the response shape are all correct; only the catalog CONTENT diverges, so no error wording is at stake here.

Open issues that look adjacent but do NOT carry these replay keys (the replay report lists these clusters as unowned): 20261102T083656Z-eks-add-on-and-cluster-version-catalogs-serve.md (wrong owner/publisher/type and wrong dates on OTHER fixtures, plus the DescribeAddonConfiguration 404/501 routing defects), 20261109T131212Z-eks-describeaddonversions-catalog-omits-further-marketplace-add-ons.md (a marketplace productUrl on eks-efsfips-traj-catalog-and-schema-discovery#task#8), 20261104T053622Z-eks-responses-omit-fields-aws-returns-cluster-vendedlogs.md and 20261112T043436Z-eks-2-net-new-causes-describeclusterversions-publishes-versions.md. None of them covers the five causes below.

Cause 1 of 5: DescribeAddonVersions ignores the kubernetesVersion filter and serves one fabricated newest version per add-on, with a marketplaceInformation.productUrl in the wrong form.
  AWS: DescribeAddonVersions is GET /addons/supported-versions returning `addons` : Addons of AddonInfo (Smithy: AddonInfo members addonName, type, addonVersions, publisher, owner, marketplaceInformation, defaultNamespace; MarketplaceInformation members productId, productUrl; input members kubernetesVersion/addonName/types/publishers/owners all httpQuery). The returned addonVersions are the add-on's PUBLISHED releases that are compatible with the requested kubernetesVersion, each AddonVersionInfo carrying its own architecture, computeTypes, compatibilities (clusterVersion / platformVersions / defaultVersion) and requiresConfiguration / requiresIamPermissions flags. For a Marketplace-owned add-on (owner 'aws-marketplace') marketplaceInformation carries the catalog's productId and the product's Marketplace URL as published for that product.
  Emulator: serves a single synthesised entry — the newest fabricated version — irrespective of the kubernetesVersion asked for, so a query scoped to an older cluster version comes back with a release whose compatibilities list only recent cluster versions and never includes the releases AWS returns for that filter; and the Marketplace entry's productUrl is a different URL form than the catalog's for the same product.
  Why: a client asking "which versions of this add-on can I install on cluster version X" is handed versions that are not compatible with X, so it either picks an uninstallable version or concludes the add-on is unavailable for its cluster; a client following marketplaceInformation.productUrl for subscription/legal checks lands on a URL that does not identify the product.
  Fix: the add-on catalog data behind DescribeAddonVersions' handler, and that handler's kubernetesVersion / addonName filtering (the filter must select rows from the catalog, not be dropped).
  Traces: eks-catalog-describe-addon-versions-accuknox-kubearmor-eg-innovations-eg-universal-agent-operator#task#1 — 18 affected steps across that fixture and eks-catalog-describe-addon-versions-eks-node-monitoring-agent-vpc-cni.

Cause 2 of 5: for the remaining catalogued add-ons, the published version history is absent entirely — the served addons[0] is a different add-on record than the catalog's.
  AWS: the same operation returns, per add-on, the full published version list newest-first, each release with its own compatibilities set and computeTypes (for the recorded observability add-on, several consecutive published releases, the newest flagged defaultVersion true and the older ones false).
  Emulator: the expected catalog entry is not present at addons[0] at all; a synthesised record with a single fabricated release is served in its place, so no requested release can be found.
  Why: a client that pins, upgrades or rolls back an add-on to a named version checks the catalog first; with the version history collapsed to one invented release, pinning and downgrading are impossible and the client reports the version as nonexistent.
  Fix: the add-on catalog data behind DescribeAddonVersions' handler (the per-add-on release history, not just the newest row).
  Traces: eks-catalog-describe-addon-versions-accuknox-kubearmor-eg-innovations-eg-universal-agent-operator#task#2 — 67 affected steps across both describe-addon-versions fixtures.

Cause 3 of 5: DescribeClusterVersions' full listing diverges at every position — the served cluster-version rows are not the published ones.
  AWS: DescribeClusterVersions is GET /cluster-versions returning clusterVersions : ClusterVersionList of ClusterVersionInformation (Smithy: members clusterVersion, clusterType, defaultPlatformVersion, defaultVersion, releaseDate, endOfStandardSupportDate, endOfExtendedSupportDate, status [deprecated since 2025-02-15], versionStatus, kubernetesPatchVersion, controlPlaneScalingTiers [@length 0..10], controlPlaneComponentConfig). The recorded listing is the published EKS version table: one row per supported Kubernetes version with its real defaultPlatformVersion, kubernetesPatchVersion, lifecycle dates, versionStatus, and a controlPlaneScalingTiers list whose per-tier apiRequestConcurrency, podSchedulingRatePerSecond, clusterDatabaseSizeGb and controlPlaneComponentConfigOverrides bounds are the published per-tier limits.
  Emulator: answers 200 with a list that matches the recorded one at NO index — all fifteen positions diverge in version, dates, platform/patch version and scaling-tier capabilities.
  Why: this listing is how a client discovers which Kubernetes versions exist, which one is default, when support ends and what control-plane scaling tiers it may ask for. A synthesised table makes upgrade planning, support-deadline computation and tier selection all wrong, and a client validating its control-plane configuration against the advertised constraints accepts values AWS refuses.
  Fix: the cluster-version catalog data behind DescribeClusterVersions' handler.
  Traces: eks-catalog-describe-cluster-versions#task#1.

Cause 4 of 5: DescribeAddonConfiguration serves a configurationSchema that is not the published schema for the requested add-on version, and drops a podIdentityConfiguration entry.
  AWS: DescribeAddonConfiguration is GET /addons/configuration-schemas with addonName and addonVersion both httpQuery and @required, returning 200 with addonName, addonVersion, configurationSchema and podIdentityConfiguration (Smithy: AddonPodIdentityConfiguration members serviceAccount, recommendedManagedPolicies). configurationSchema is the exact published JSON-Schema document for THAT add-on version — the recorded aws-efs-csi-driver schema is a draft 2019-09 document with additionalProperties false and a specific property set and placement (controller.*, node.*, useFIPS) — and podIdentityConfiguration has one entry per Kubernetes service account the add-on uses.
  Emulator: serves a schema document whose property set/placement differs from the published one for the requested version, and a podIdentityConfiguration list that is missing its second entry.
  Why: the schema is a contract a client validates its configurationValues against before CreateAddon/UpdateAddon, so a divergent schema makes the client send configuration AWS rejects (or reject configuration AWS accepts); and a client that builds pod-identity IAM from podIdentityConfiguration provisions no role for the service account that entry describes, leaving that component of the add-on unauthorized.
  Fix: the add-on configuration-schema catalog data behind DescribeAddonConfiguration's handler (schema text and podIdentityConfiguration keyed by addonName+addonVersion).
  Traces: eks-efsfips-traj-catalog-and-schema-discovery#task#1.

Cause 5 of 5: the same defect for the VPC CNI add-on — a different schema dialect and document than the published one.
  AWS: for the recorded vpc-cni version the configurationSchema is a draft-06 document built from $ref/definitions (VpcCni, EniConfig, Env with the AWS_VPC_*/ENABLE_* string-typed keys and their format annotations), i.e. the add-on's own published schema, which differs structurally from other add-ons' schemas.
  Emulator: answers 200 with a different document, so the dialect, the definitions and the typed env-var property set a client validates against are not the published ones.
  Why: a client generating or validating vpc-cni configurationValues (env var names, formats, enums) from the fetched schema cannot tell which keys are legal, and silently diverges from what AWS will accept on CreateAddon/UpdateAddon.
  Fix: the add-on configuration-schema catalog data behind DescribeAddonConfiguration's handler.
  Traces: eks-eks-catalog-read-only-discovery#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (score 72.5%, 108 mismatched steps). All five clusters are 200 -> 200: the HTTP status and error class are correct, the payload content is not.

Cause 1 — eks-catalog-describe-addon-versions-accuknox-kubearmor-eg-innovations-eg-universal-agent-operator#task#1, DescribeAddonVersions, expected 200 got 200, keys $.addons[0] and $.addons[0].marketplaceInformation.productUrl, 18 affected steps. Expected addons[0] = {'addonName': 'accuknox_kubearmor', 'type': 'security', 'addonVersions': [{'addonVersion': 'v0.10.2-eksbuild.1', 'architecture': ['amd64','arm64'], 'computeTypes': ['ec2'], 'compatibilities': five entries for clusterVersion 1.27/1.26/1.25/1.24/1.23 each defaultVersion True, requiresConfiguration False, requiresIamPermissions False}], 'publisher': 'AccuKnox', 'owner': 'aws-marketplace', 'marketplaceInformation': {'productId': '...', 'productUrl': '...'}, 'defaultNamespace': 'kubearmor'}; got the same addonName/type but addonVersions starting at 'v1.11.3-eksbuild.1' with compatibilities 1.37..1.30 — i.e. the newest synthesised release instead of the releases compatible with the cluster version in the request.
Cause 2 — same fixture #task#2, DescribeAddonVersions, 200 -> 200, key $.addons[0], 67 affected steps. Expected addons[0] = {'addonName': 'adot', 'type': 'observability', 'addonVersions': ['v0.156.0-eksbuild.1' (computeTypes ['ec2','hybrid','fargate','auto'], 1.37..1.31 defaultVersion True), 'v0.151.0-eksbuild.2', 'v0.151.0-eksbuild.1', ...], 'requiresIamPermissions': True}; the emulator served no such record at addons[0].
Cause 3 — eks-catalog-describe-cluster-versions#task#1, DescribeClusterVersions, 200 -> 200, diverging keys $.clusterVersions[0] through $.clusterVersions[14] (every index). Expected clusterVersions[0] = {'clusterVersion': '1.37', 'clusterType': 'eks', 'defaultPlatformVersion': 'eks.4', 'defaultVersion': False, 'releaseDate': 1790812800.0, 'endOfStandardSupportDate': 1827619200.0, 'endOfExtendedSupportDate': 1859241600.0, 'status': 'STANDARD_SUPPORT', 'versionStatus': 'STANDARD_SUPPORT', 'kubernetesPatchVersion': '1.37.0', 'controlPlaneScalingTiers': [tier-xl apiRequestConcurrency 2000 / podSchedulingRatePerSecond 167 / clusterDatabaseSizeGb 16 with kubeApiServerConfig and kubeSchedulerConfig overrides null and terminatedPodGcThreshold constrained min 10000 max 12500, tier-2xl 4000/283, tier-4xl 8001/400, ...]}; nothing matching was served at that index, and the same holds for indices 1..14. (The concrete dates, version numbers and tier numbers above are the recorded catalog rows, cited to show WHICH rows are missing — the requirement is that the served table be the published EKS version table, not these literal values.)
Cause 4 — eks-efsfips-traj-catalog-and-schema-discovery#task#1, DescribeAddonConfiguration, 200 -> 200, keys $.configurationSchema and $.podIdentityConfiguration[1]. Expected configurationSchema is the draft 2019-09 aws-efs-csi-driver document ($schema https://json-schema.org/draft/2019-09/schema, additionalProperties false, controller.{deleteAccessPointRootDir,logLevel,nodeSelector,podAnnotations,tolerations}, node.{logLevel,nodeSelector,podAnnotations,tolerations,replicaCount}, useFIPS); the served document diverges from it, and podIdentityConfiguration[1] diverges as well.
Cause 5 — eks-eks-catalog-read-only-discovery#task#3, DescribeAddonConfiguration, 200 -> 200, key $.configurationSchema. Expected the vpc-cni document ({'$ref': '#/definitions/VpcCni', '$schema': 'http://json-schema.org/draft-06/schema#', definitions Affinity/EniConfig/Env with AWS_VPC_* and ENABLE_* typed properties}); a different document was served.

Smithy model consulted: services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json — DescribeAddonVersions (GET /addons/supported-versions, 200, output addons : Addons, httpQuery filters kubernetesVersion/addonName/types/publishers/owners, paginated on addons), AddonInfo and MarketplaceInformation shapes, DescribeClusterVersions (GET /cluster-versions, 200, output clusterVersions : ClusterVersionList) and ClusterVersionInformation, DescribeAddonConfiguration (GET /addons/configuration-schemas, addonName and addonVersion both httpQuery + required, output configurationSchema and podIdentityConfiguration) and AddonPodIdentityConfiguration.

## Steps to Reproduce
1. git clone the project repository and build the emulator from this checkout.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Representative divergence: fixture eks-catalog-describe-addon-versions-accuknox-kubearmor-eg-innovations-eg-universal-agent-operator, step task#1. The other four causes are at eks-catalog-describe-addon-versions-accuknox-kubearmor-eg-innovations-eg-universal-agent-operator#task#2, eks-catalog-describe-cluster-versions#task#1, eks-efsfips-traj-catalog-and-schema-discovery#task#1 and eks-eks-catalog-read-only-discovery#task#3 — all four must be re-replayed, they are separate causes.
4. Manually, against the emulator's data listener:
   - GET /addons/supported-versions?addonName=<a Marketplace add-on>&kubernetesVersion=<an older supported version> and compare addons[0].addonVersions against the AWS catalog: the emulator returns its newest synthesised release with recent-only compatibilities rather than the releases compatible with the requested kubernetesVersion, and marketplaceInformation.productUrl is in the wrong form.
   - GET /addons/supported-versions (unfiltered) and check that each add-on's full published release history is present, newest first, with computeTypes and per-release compatibilities.
   - GET /cluster-versions and compare the whole list against the published EKS version table (clusterVersion, defaultPlatformVersion, kubernetesPatchVersion, defaultVersion, releaseDate / endOfStandardSupportDate / endOfExtendedSupportDate, versionStatus, controlPlaneScalingTiers and controlPlaneComponentConfig bounds): no index matches.
   - GET /addons/configuration-schemas?addonName=aws-efs-csi-driver&addonVersion=<the recorded version> and diff configurationSchema against the add-on's published schema, and check podIdentityConfiguration has one entry per service account the add-on uses (the second entry is missing).
   - GET /addons/configuration-schemas?addonName=vpc-cni&addonVersion=<the recorded version> and diff configurationSchema against the published draft-06 VpcCni document.

## Fix note

An add-on's published builds are now the ADD-ON's own history rather than a function of the
Kubernetes version calendar, in two parts (`src/state/addon_catalog.rs`):

- `ADDON_PUBLISHED_BUILDS` carries the published build table of an add-on whose table this
  catalog can cite — the version of each build and the newest Kubernetes version it was
  published for — so such an add-on publishes exactly those builds, one build may be the only
  one published across several Kubernetes versions, and an add-on whose publisher has stopped
  publishing has NO build for a Kubernetes version released since its last one. It is still
  listed, with the builds it has, and drops out of a listing filtered to a newer version (and
  out of an install on a cluster running one). The KubeArmor listing is cited that way: one
  published build, offered for the versions that were in support when it was published.
- every build, cited or derived, is offered for the Kubernetes versions Amazon EKS still
  OFFERED when the build was published (`build_compatibilities`), so a build names no version
  whose extended support had ended by then. The window now moves along the calendar with the
  add-on's line instead of reaching back to the add-on's first row for ever.

`DescribeAddonConfiguration`'s `podIdentityConfiguration` is published from ONE minimum build
per add-on (`ADDON_POD_IDENTITY_FIRST_BUILD`), which is how the minimum version is published:
a build at or above it publishes an entry for EVERY service account the add-on runs as — the
second entry that used to be withheld from the builds between two invented per-component
minimums — and an older build publishes none.

Cause 1 is closed: the representative step's `addons[0]` now matches, built out of the add-on's
published build table, its published compatibility window and its listing's cited product id
and product page.

What remains, and why it is not guessed at here:

- Cause 2 (the published release history of the REST of the catalogue): the mechanism is in
  place — a row in `ADDON_PUBLISHED_BUILDS` is all a cited build table needs — but the
  published tables of the other catalogued add-ons cannot be cited from inside this emulator
  (the replay answers "recorded values withheld" on every divergence), so their builds are
  still derived along their own release lines. Adding a row per add-on as its table becomes
  citable is the remaining work.
- Causes 4 and 5 (the configuration SCHEMA documents): the EFS CSI driver's `node` definition
  gained the `replicaCount` key its published document declares, and the pod-identity half of
  cause 4 is closed; the published JSON-Schema DOCUMENTS of those two add-ons are third-party
  artifacts this catalogue cannot cite from here, so the documents are still built from the
  key set the catalogue carries per add-on.
- Cause 3 (the cluster-version rows): the calendar itself, and every row member derived from
  it, is the published one; the row's remaining divergence is inside `controlPlaneComponentConfig`,
  whose published bounds are owned by the open issue
  20261114T191058Z-eks-catalog-reads-publish-synthesised-control-plane-constraints.md (it cites
  them), and inside `controlPlaneScalingTiers`, whose published per-tier capability table cannot
  be cited from here. Nothing here invents a tier capability or a constraint bound.

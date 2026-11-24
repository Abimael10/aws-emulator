Issue-Source: trace-sweep-divergence
Issue-Trace: eks-ekscc-traj-version-and-support-mistakes
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eks-ekscc-traj-version-and-support-mistakes eks-mbes-traj-create-addon-request-mistakes
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|DescribeClusterVersions|2xx->2xx|$.clusterVersions[*].controlPlaneComponentConfig.kubeApiServerConfig.eventTtl.constraints.min
Issue-Key: eks|DescribeClusterVersions|2xx->2xx|$.clusterVersions[*].controlPlaneComponentConfig.kubeApiServerConfig.serviceNodePortRange.constraints.maxPort.max
Issue-Key: eks|DescribeClusterVersions|2xx->2xx|$.clusterVersions[*].controlPlaneComponentConfig.kubeApiServerConfig.serviceNodePortRange.constraints.maxPort.min
Issue-Key: eks|DescribeClusterVersions|2xx->2xx|$.clusterVersions[*].controlPlaneComponentConfig.kubeApiServerConfig.serviceNodePortRange.constraints.minPort.max
Issue-Key: eks|DescribeClusterVersions|2xx->2xx|$.clusterVersions[*].controlPlaneComponentConfig.kubeApiServerConfig.serviceNodePortRange.constraints.minPort.min
Issue-Key: eks|DescribeClusterVersions|2xx->2xx|$.clusterVersions[*].controlPlaneComponentConfig.kubeControllerManagerConfig.horizontalPodAutoscalerControllerConfig.horizontalPodAutoscalerSyncPeriod.constraints.max
Issue-Key: eks|DescribeClusterVersions|2xx->2xx|$.clusterVersions[*].controlPlaneComponentConfig.kubeControllerManagerConfig.podGcControllerConfig.terminatedPodGcThreshold.constraints.min
Issue-Key: eks|DescribeClusterVersions|2xx->2xx|$.clusterVersions[*].controlPlaneComponentConfig.kubeSchedulerConfig.nodeResourcesFit.scoringStrategy.constraints.resources.name.allowedValues[*]
Issue-Key: eks|DescribeAddonVersions|2xx->2xx|$.addons[*].addonVersions[*]
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# eks catalog reads publish synthesised control-plane constraints and add-on version histories

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeClusterVersions

## Description
Two net-new root causes share one fault: the read-only EKS catalogs are backed by emulator-authored data rather than the published AWS catalog, so the per-Kubernetes-version control-plane configuration CONSTRAINTS and the per-add-on published RELEASE HISTORY that clients validate against are not AWS's. Both clusters answer HTTP 200 with the correct response shape and no error is involved; only the catalog content diverges, so no status or error wording is at stake.

Adjacent OPEN issues that do NOT carry these two replay keys (the replay report lists both clusters as unowned): 20261114T010332Z-eks-read-only-catalog-listings-serve-synthesised-add.md (synthesised add-on/cluster-version/configuration-schema content on OTHER fixtures and keys — it does not cover clusterVersions[*].controlPlaneComponentConfig at all, nor eks-mbes-traj-create-addon-request-mistakes), 20261112T043436Z-eks-2-net-new-causes-describeclusterversions-publishes-versions.md (which versions appear in the default listing, not the constraints inside a row) and 20261109T131212Z-eks-describeaddonversions-catalog-omits-further-marketplace-add-ons.md (marketplaceInformation.productUrl). Neither cause below is covered by any of them.

Cause 1 of 2: DescribeClusterVersions serves a clusterVersions row whose controlPlaneComponentConfig advertises the wrong default/constraint bounds and an incomplete allowedValues set.
  AWS: DescribeClusterVersions is GET /cluster-versions returning 200 with clusterVersions : ClusterVersionList of ClusterVersionInformation; its member controlPlaneComponentConfig : ControlPlaneConfigInfo is documented as "The default control plane component configuration and constraints for this Kubernetes version", with kubeApiServerConfig : KubeApiServerVersionConfig (eventTtl : DurationParameterConfig, serviceNodePortRange : PortRangeParameterConfig), kubeSchedulerConfig : KubeSchedulerVersionConfig (nodeResourcesFit) and kubeControllerManagerConfig : KubeControllerManagerVersionConfig (podGcControllerConfig, horizontalPodAutoscalerControllerConfig). Each of those parameter configs publishes a defaultValue plus the constraints (min/max, or allowedValues) that EKS will actually enforce on UpdateClusterConfig / CreateCluster for that Kubernetes version, and the scheduler's NodeResourcesFit scoring-strategy resource name allowedValues enumerate every schedulable resource EKS permits for that version, including the accelerator resources (GPU and Neuron device/core names), not just cpu/memory. The recorded response is that published constraint table for the requested version.
  Emulator: answers 200 with a row whose constraints are invented rather than the published ones, in both directions — the API-server serviceNodePortRange bounds are served as the generic Linux ephemeral-port window instead of the narrower window EKS publishes (minPort/maxPort min and max both diverge), eventTtl's constraint minimum and the controller-manager's terminatedPodGcThreshold minimum and horizontalPodAutoscalerSyncPeriod maximum are each a different value than the published one, and the scheduler's nodeResourcesFit scoring-strategy resource-name allowedValues list is truncated so the accelerator resource names are absent entirely.
  Why: this table is the contract a client reads to decide which control-plane parameter values it may send. Where the emulator's bounds are WIDER than AWS's, a client validates a value locally, sends it, and AWS refuses it in production though the emulator accepted it; where they are NARROWER, the client never offers a value AWS supports. With the accelerator resource names missing from allowedValues, a client configuring GPU/Neuron-aware scheduling concludes those resources cannot be scored and silently drops that configuration.
  Fix: the cluster-version catalog data behind DescribeClusterVersions' handler — specifically the per-version controlPlaneComponentConfig defaults/constraints it serves (and, so the two stay consistent, the same bounds the control-plane-config validation enforces).
  Traces: eks-ekscc-traj-version-and-support-mistakes#task#8.

Cause 2 of 2: DescribeAddonVersions serves a fabricated newest-release list for the requested add-on instead of that add-on's published release history.
  AWS: DescribeAddonVersions is GET /addons/supported-versions returning 200 with addons : Addons of AddonInfo, whose addonVersions : AddonVersionInfoList is documented as "information about available add-on versions and compatible Kubernetes versions"; each AddonVersionInfo carries addonVersion, architecture, computeTypes, compatibilities (clusterVersion / platformVersions / defaultVersion) and requiresConfiguration / requiresIamPermissions. The input members addonName, kubernetesVersion, types, publishers and owners are all httpQuery filters that select WHICH catalog rows and releases are returned, so a query scoped to an add-on (and, where given, a cluster version) returns that add-on's published releases compatible with it, each release carrying its own compatibilities set and its own defaultVersion flag.
  Emulator: answers 200 with an addons[0] whose leading addonVersions entries are synthesised recent releases — a newer version string than any the catalog publishes for that add-on, a computeTypes set and a compatibilities span covering recent cluster versions, and defaultVersion flagged true on releases AWS does not flag — in place of the published releases (which span the older cluster versions and are not the default). The request's filters therefore do not select catalog rows; the handler generates a plausible-looking newest-first list instead.
  Why: this listing is how a client discovers which add-on versions are installable before CreateAddon/UpdateAddon. Handed invented version strings, a client pins or upgrades to a version that does not exist (CreateAddon then fails in production), cannot downgrade or roll back because the real release history is absent, and trusts a defaultVersion flag that names the wrong release.
  Fix: the add-on catalog data behind DescribeAddonVersions' handler and that handler's addonName/kubernetesVersion filtering — the filter must select releases from the catalog rather than drive generation of a release list.
  Traces: eks-mbes-traj-create-addon-request-mistakes#task#7.

In both causes the concrete version strings, dates and numeric bounds in the recorded responses are cited only to show WHICH published rows are missing; the requirement is that the served catalog be the published one for the requested filters, not these literal values.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator up, 55 fixtures replayed, 393 steps, 293 passed, 100 mismatched, score 74.6%, 13 divergence clusters. Both clusters below are 200 -> 200 (status and error class correct, payload content wrong) and both are reported with an empty owned_by, i.e. no open issue carries their replay key.

Cause 1 — fixture eks-ekscc-traj-version-and-support-mistakes, step task#8, DescribeClusterVersions, expected 200 got 200, diverging keys all under $.clusterVersions[0].controlPlaneComponentConfig:
  - kubeApiServerConfig.eventTtl.constraints.min: expected '10m', got '30m'
  - kubeApiServerConfig.serviceNodePortRange.constraints.minPort.min: expected 10260, got 1024
  - kubeApiServerConfig.serviceNodePortRange.constraints.minPort.max: expected 32767, got 65535
  - kubeApiServerConfig.serviceNodePortRange.constraints.maxPort.min: expected 10260, got 1024
  - kubeApiServerConfig.serviceNodePortRange.constraints.maxPort.max: expected 32767, got 65535
  - kubeSchedulerConfig.nodeResourcesFit.scoringStrategy.constraints.resources.name.allowedValues[2]: expected 'nvidia.com/gpu', got None (absent)
  - kubeSchedulerConfig.nodeResourcesFit.scoringStrategy.constraints.resources.name.allowedValues[3]: expected 'aws.amazon.com/neuron', got None (absent)
  - kubeSchedulerConfig.nodeResourcesFit.scoringStrategy.constraints.resources.name.allowedValues[4]: expected 'aws.amazon.com/neuroncore', got None (absent)
  - kubeControllerManagerConfig.podGcControllerConfig.terminatedPodGcThreshold.constraints.min: expected 12500, got 10000
  - kubeControllerManagerConfig.horizontalPodAutoscalerControllerConfig.horizontalPodAutoscalerSyncPeriod.constraints.max: expected '15s', got '1m'
  Note the direction: the served nodePort window (1024..65535) is wider than the published one (10260..32767), while eventTtl's minimum and the HPA sync-period maximum are narrower/looser than published.

Cause 2 — fixture eks-mbes-traj-create-addon-request-mistakes, step task#7, DescribeAddonVersions, expected 200 got 200, diverging keys $.addons[0].addonVersions[0] and $.addons[0].addonVersions[1]. Expected addonVersions[0] = {'addonVersion': 'v1.5.9-eksbuild.1', 'architecture': ['amd64','arm64'], 'computeTypes': ['ec2'], 'compatibilities': eleven entries for clusterVersion 1.33 down to 1.23 each with platformVersions ['*'] and defaultVersion False, 'requiresConfiguration': False, 'requiresIamPermissions': True}; got {'addonVersion': 'v3.2.0-eksbuild.1', 'architecture': ['amd64','arm64'], 'computeTypes': ['ec2','auto'], 'compatibilities': entries for clusterVersion 1.37 downwards each with defaultVersion True, ...} — a version string the catalog does not publish for that add-on, a different computeTypes set, a recent-only compatibilities span and defaultVersion flipped to true.

Smithy model consulted: services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json — DescribeClusterVersions (GET /cluster-versions, code 200, output clusterVersions : ClusterVersionList, httpQuery filters clusterType/clusterVersions/defaultOnly/includeAll/versionStatus/status[deprecated since 2025-02-15], paginated on clusterVersions); ClusterVersionInformation.controlPlaneComponentConfig : ControlPlaneConfigInfo ("The default control plane component configuration and constraints for this Kubernetes version") -> KubeApiServerVersionConfig{eventTtl : DurationParameterConfig, serviceNodePortRange : PortRangeParameterConfig}, KubeSchedulerVersionConfig{nodeResourcesFit : NodeResourcesFitVersionConfig}, KubeControllerManagerVersionConfig{podGcControllerConfig, horizontalPodAutoscalerControllerConfig}; DescribeAddonVersions (GET /addons/supported-versions, code 200, output addons : Addons, httpQuery filters kubernetesVersion/addonName/types/publishers/owners, paginated on addons); AddonInfo.addonVersions : AddonVersionInfoList and AddonVersionInfo{addonVersion, architecture, computeTypes, compatibilities, requiresConfiguration, requiresIamPermissions}.

## Steps to Reproduce
1. git clone the project repository and build the emulator from this checkout.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Representative divergence: fixture eks-ekscc-traj-version-and-support-mistakes, step task#8 (Cause 1). The second cause is at fixture eks-mbes-traj-create-addon-request-mistakes, step task#7 — it is a SEPARATE cause and must be re-replayed too; both must pass before this issue is closed.
4. Manually, against the emulator's data listener:
   - GET /cluster-versions (optionally scoped with clusterVersions=<a supported version>) and diff clusterVersions[0].controlPlaneComponentConfig against the published EKS control-plane configuration constraints for that version: kubeApiServerConfig.serviceNodePortRange.constraints.minPort/maxPort min and max, kubeApiServerConfig.eventTtl.constraints.min, kubeControllerManagerConfig.podGcControllerConfig.terminatedPodGcThreshold.constraints.min and kubeControllerManagerConfig.horizontalPodAutoscalerControllerConfig.horizontalPodAutoscalerSyncPeriod.constraints.max all come back as emulator-chosen bounds, and kubeSchedulerConfig.nodeResourcesFit.scoringStrategy.constraints.resources.name.allowedValues is truncated to the non-accelerator resources (the GPU and Neuron resource names are missing).
   - Then send UpdateClusterConfig with a serviceNodePortRange value that is legal under the advertised bounds but outside the published ones and observe that the emulator's advertised contract disagrees with AWS's enforcement.
   - GET /addons/supported-versions?addonName=<the add-on named in the fixture's request> (with the fixture's kubernetesVersion filter, if present) and compare addons[0].addonVersions against the AWS catalog: the emulator returns synthesised newest releases with recent-only compatibilities and defaultVersion true, instead of the add-on's published release history (older cluster-version compatibilities, defaultVersion false).

## Fix note

Cause 1 is closed. The control-plane component configuration catalog now carries the
PUBLISHED per-version configuration limits parameter by parameter, in its own file
(`src/state/control_plane_component_config.rs`, moved out of `src/state.rs` whole), instead of
deriving each window from the upstream Kubernetes component's default and from a duration a
member's documentation names as an example of its spelling: the API server's NodePort window is
the published one rather than a host's unprivileged port space, the event-TTL floor and the two
controller-manager windows are the published ones, and the scheduler's scoreable-resource set
names every accelerator resource beside the two the default strategy weights. The defaults are
untouched, and the cluster slice keeps reading the same constants, so a cluster that overrides
nothing still reports exactly what the catalog publishes for it.

Cause 2 is NOT closed and is not guessed at here. The divergence is which published builds the
catalog holds for one add-on and in what order: the emulator derives an add-on's builds along its
own release line, one per Kubernetes calendar row, while the recorded listing leads with an older
build carrying a wider compatibility span and a narrower `computeTypes` set. The replay answers
"recorded values withheld" for that step, and the published order cannot be derived either — the
corpus shows one add-on listed newest build first and this one led by an older build, so no single
ordering rule produces both. The mechanism for a cited build table is already in place
(`ADDON_PUBLISHED_BUILDS` in `src/state/addon_catalog.rs`); it needs that add-on's published table,
which cannot be cited from inside this emulator. Writing a history or an ordering that puts one
build at index 0 would fit the recorded instance rather than implement a rule, so the add-on
catalog is left exactly as it was.

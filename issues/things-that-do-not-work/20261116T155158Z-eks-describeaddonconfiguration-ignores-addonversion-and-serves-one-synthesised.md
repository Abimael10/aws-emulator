Issue-Source: trace-sweep-divergence
Issue-Trace: eks-efsfips-traj-catalog-and-schema-discovery
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eks-efsfips-traj-catalog-and-schema-discovery
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|DescribeAddonConfiguration|2xx->2xx|$.podIdentityConfiguration[*]
Issue-Related: 20261114T010332Z-eks-read-only-catalog-listings-serve-synthesised-add.md
Issue-Commit: 68169c86c7603a2902f84035dfc94fd02b30ddfc

# eks DescribeAddonConfiguration ignores addonVersion and serves one synthesised schema and pod-identity list

> _Note: this issue was identified against the code base at commit `68169c86c7603a2902f84035dfc94fd02b30ddfc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAddonConfiguration

## Description
One net-new root cause (1 of 1): DescribeAddonConfiguration does not key its answer on the requested addonVersion. The emulator serves a single synthesised configuration-schema document and a single synthesised podIdentityConfiguration list per add-on NAME, so a request for a different published version of the same add-on comes back with the other version's (already divergent) document instead of that version's own published schema.

Cause 1 of 1: DescribeAddonConfiguration returns a configurationSchema and podIdentityConfiguration[0] that are not the published ones for the requested addonName+addonVersion pair.
  AWS: DescribeAddonConfiguration is GET /addons/configuration-schemas (Smithy: services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json; input members addonName and addonVersion are both @httpQuery and @required; output members addonName, addonVersion, configurationSchema : String, podIdentityConfiguration : AddonPodIdentityConfigurationList; errors InvalidParameterException 400, ResourceNotFoundException 404, ServerException 500; success code 200). configurationSchema is the exact JSON-Schema document PUBLISHED FOR THAT ADD-ON VERSION: the schema is version-scoped, so two published versions of the same add-on legitimately differ in dialect, in which properties exist and in where a property sits in the document (for the recorded aws-efs-csi-driver version the deleteAccessPointRootDir boolean is a TOP-LEVEL property of the draft 2019-09 document with additionalProperties false, whereas the neighbouring version nests the same property under controller). podIdentityConfiguration carries one AddonPodIdentityConfiguration (serviceAccount — 'The Kubernetes Service Account name used by the add-on' — plus recommendedManagedPolicies, 'A suggested IAM Policy for the add-on') per service account THAT version of the add-on uses. Both are a function of (addonName, addonVersion), not of addonName alone. Status, error class and response shape are not at stake: AWS answers 200 and so does the emulator; only the content diverges.
  Emulator: answers 200 with a configurationSchema whose property set and placement are those of a different (also synthesised) document than the one published for the requested version — the top-level deleteAccessPointRootDir property is absent and the property layout under controller/node differs — and with a podIdentityConfiguration whose first entry (serviceAccount / recommendedManagedPolicies) is not the one published for that version. Because the answer is keyed on the add-on name only, varying addonVersion across requests does not vary the response.
  Why: configurationSchema is the contract a client validates its configurationValues against before CreateAddon/UpdateAddon, and it is version-specific precisely because a client upgrading or pinning an add-on must know which keys that version accepts. Given a schema that belongs to no requested version, the client sends configuration AWS rejects (unknown property under additionalProperties false) or refuses configuration AWS accepts, and a client that provisions pod-identity IAM from podIdentityConfiguration creates a role for the wrong service account, leaving that add-on component unauthorized.
  Fix: the add-on configuration-schema catalog data behind DescribeAddonConfiguration's handler, and that handler's lookup key — the schema text and the podIdentityConfiguration list must be selected by addonName AND addonVersion, not by addonName alone.
  Traces: eks-efsfips-traj-catalog-and-schema-discovery#task#2.

Adjacent open issues that do NOT carry this replay key (the replay report lists this cluster as unowned): 20261114T010332Z-eks-read-only-catalog-listings-serve-synthesised-add.md (its Cause 4 covers the SAME fixture at task#1 — a different requested addonVersion — and its Cause 5 the vpc-cni document at eks-eks-catalog-read-only-discovery#task#3, but neither covers task#2 nor the version-keying defect), 20261102T083656Z-eks-add-on-and-cluster-version-catalogs-serve.md and 20261109T131212Z-eks-describeaddonversions-catalog-omits-further-marketplace-add-ons.md.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (score 75.6%, 96 mismatched steps, 10 divergence clusters). This is the only cluster the report lists with no owning issue and no blocking stub.

Fixture eks-efsfips-traj-catalog-and-schema-discovery, step task#2, DescribeAddonConfiguration, expected status 200, actual status 200 (so no status, error code or error message is in question here). Diverging keys, per the replay output: $.configurationSchema (value_mismatch) and $.podIdentityConfiguration[0] (missing/divergent).

Expected configurationSchema (recorded response, cited to identify WHICH document is required, not as literal values to hard-code): the aws-efs-csi-driver draft 2019-09 document — {"$schema":"https://json-schema.org/draft/2019-09/schema","additionalProperties":false,"description":"Configurable parameters for the aws-efs-csi-driver","properties":{"controller":{additionalProperties false, properties logLevel (integer 0..6), nodeSelector, podAnnotations, tolerations},"deleteAccessPointRootDir":{boolean, TOP-LEVEL},"node":{additionalProperties false, properties logLevel, nodeSelector, podAnnotations, tolerations, replicaCount},"useFIPS":{boolean}}}. Served: a document that begins with the same $schema line but differs in the property set/placement — notably deleteAccessPointRootDir is not a top-level property — i.e. the document the emulator also serves for the OTHER addonVersion of the same add-on (task#1 of this fixture), confirming the response is not keyed on addonVersion.

$.podIdentityConfiguration[0]: the recorded entry for the requested version is absent from the served list.

Smithy model consulted: services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json — DescribeAddonConfiguration (GET /addons/configuration-schemas, code 200, addonName + addonVersion both @httpQuery @required, output configurationSchema : String and podIdentityConfiguration : AddonPodIdentityConfigurationList) and AddonPodIdentityConfiguration (members serviceAccount, recommendedManagedPolicies).

## Steps to Reproduce
1. git clone the project repository and build the emulator from this checkout.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. The divergence is fixture eks-efsfips-traj-catalog-and-schema-discovery, step task#2 (the single cause of this issue); re-replay that step to confirm a fix.
4. Manually, against the emulator's data listener: issue GET /addons/configuration-schemas?addonName=aws-efs-csi-driver&addonVersion=<version A> and then the same request with <version B>, two different published versions of that add-on. The emulator returns byte-identical configurationSchema and podIdentityConfiguration for both, whereas AWS returns each version's own published schema (they differ in dialect and in which properties exist and where they sit — e.g. deleteAccessPointRootDir top-level versus nested under controller) and that version's own podIdentityConfiguration entries. Diff each served configurationSchema against the add-on version's published schema document.

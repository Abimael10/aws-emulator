Issue-Source: trace-sweep-divergence
Issue-Trace: opensearch-catalog-get-package-version-history
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: opensearch-catalog-get-package-version-history opensearch-catalog-describe-packages
Issue-Shape-Id: com.amazonaws.opensearch#AmazonOpenSearchService
Issue-Key: opensearch|GetPackageVersionHistory|2xx->4xx|$.PackageID
Issue-Key: opensearch|GetPackageVersionHistory|2xx->4xx|$.PackageVersionHistoryList
Issue-Key: opensearch|GetPackageVersionHistory|2xx->4xx|status
Issue-Key: opensearch|DescribePackages|2xx->2xx|$.PackageDetailsList[*]
Issue-Commit: c715480170741a84c2fa772a3d743f7ed03fb9c6

# opensearch: the published package catalog still answers neither read as AWS does (2 causes)

> _Note: this issue was identified against the code base at commit `c715480170741a84c2fa772a3d743f7ed03fb9c6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPackageVersionHistory

## Description
The emulator's OpenSearch package plane is still backed by its own catalog of legacy Elasticsearch analysis plugins rather than the AWS-managed package catalog, and the per-package read of a well-formed PackageID that the catalog does not hold is refused on the wrong error channel. 2 causes.

Cause 1 of 2: GetPackageVersionHistory answers HTTP 400 for a well-formed PackageID instead of 200 with the package's history, and when a package genuinely cannot be resolved it refuses on the 400 channel instead of the modeled 409 ResourceNotFoundException.
AWS: Smithy: GetPackageVersionHistory GET /2021-01-01/packages/{PackageID}/history, success code 200; input member PackageID is an @httpLabel with pattern ^([FG][0-9]+)$|^(pkg-[a-f0-9]+)$; output members PackageID, PackageVersionHistoryList (list of PackageVersionHistory{PackageVersion, CommitMessage, CreatedAt, PluginProperties, PackageConfiguration}) and NextToken. For a package visible to the account AWS answers 200, echoes PackageID and returns one PackageVersionHistoryList entry per published version, newest first. Smithy: the operation's modeled errors are AccessDeniedException (403), BaseException (400, defaulted), InternalException (500), ResourceNotFoundException (409) and ValidationException (400); ResourceNotFoundException is documented "An exception for accessing or deleting a resource that doesn't exist", so a syntactically valid PackageID that resolves to nothing is a 409 ResourceNotFoundException, while 400 ValidationException is the channel for a malformed request. The recorded response for this step is 200 with the id echoed and a two-version history (v2 then v1), each entry carrying CreatedAt, PackageConfiguration{ConfigurationRequirement, LicenseRequirement, LicenseFilepath, RequiresRestartForConfigurationUpdate} and PluginProperties{Name, Description, Version, ClassName, UncompressedSizeInBytes}.
Emulator: answers HTTP 400 with neither PackageID nor PackageVersionHistoryList in the body, for a PackageID that satisfies the modeled pattern. Two things diverge here independently of which ids the catalog mints: a package the service publishes has no readable version history at all, and the not-resolvable case is answered 400 rather than the modeled ResourceNotFoundException/409. The error message for an unresolvable PackageID must be AWS's fixed text for that error class with the requested PackageID substituted in as its only variable part; the identifier itself is a request value, not part of the template.
Why: the normal client sequence is DescribePackages to enumerate, then GetPackageVersionHistory per package to choose a PackageVersion before AssociatePackages. Against the emulator every package read is a hard client error, so version selection cannot be automated at all; and because the refusal arrives as a 400 rather than a 409 ResourceNotFoundException, SDK and CLI callers classify a missing resource as a malformed request and do not take their not-found branch (nor is it retried/handled as a resource error). This is the widest divergence in the corpus: 90 replayed steps in this cluster.
Fix: the GetPackageVersionHistory handler and the package-catalog state it reads — a package the catalog publishes must have a readable newest-first version history, and an unresolvable but well-formed PackageID must be refused with ResourceNotFoundException (HTTP 409), not a bare 400.
Traces: opensearch-catalog-get-package-version-history#task#1 (representative; affected_count 90, task#1..task#25 and beyond in the same cluster).

Cause 2 of 2: DescribePackages serves an emulator-authored legacy Elasticsearch plugin list instead of the AWS-managed package catalog, so every element of PackageDetailsList diverges.
AWS: Smithy: DescribePackages POST /2021-01-01/packages/describe, success 200, output PackageDetailsList of PackageDetails. Smithy PackageDetails members: PackageID, PackageName, PackageType, PackageDescription, PackageStatus, CreatedAt, LastUpdatedAt, AvailablePackageVersion, ErrorDetails ("Additional information if the package is in an error state. Null otherwise"), EngineVersion (pattern ^Elasticsearch_X.Y$|^OpenSearch_X.Y$), AvailablePluginProperties, AvailablePackageConfiguration, AllowListedUserList, PackageOwner, PackageVendingOptions, PackageEncryptionOptions. Recorded behavior: the catalog AWS returns is the current AWS-managed/marketplace plugin set — entries at EngineVersion OpenSearch_2.17 with AvailablePackageVersion 'v2', a third-party PackageOwner, an AvailablePackageConfiguration{ConfigurationRequirement: OPTIONAL, LicenseRequirement: REQUIRED, LicenseFilepath, RequiresRestartForConfigurationUpdate: true} and an explicit ErrorDetails member.
Emulator: returns a different catalog entirely — roughly 93 entries of legacy Elasticsearch analysis plugins (analysis-phonetic and siblings) pinned to EngineVersion 'Elasticsearch_7.10' and carrying no AvailablePackageConfiguration and no ErrorDetails member. The replay reports $.PackageDetailsList[0] through $.PackageDetailsList[92] as divergent, i.e. no element of the listing matches. The requirement here is the SET of packages the service publishes and the per-entry members above, not any one recorded id or timestamp: server-minted PackageIDs, CreatedAt/LastUpdatedAt instants and UncompressedSizeInBytes are per-environment values, but which packages are published, their names, PackageType, EngineVersion family, owner and configuration/licence requirements are the service's catalog and are stable.
Why: a client cannot discover any package AWS actually offers, cannot read the licence requirement or RequiresRestartForConfigurationUpdate flag that decides whether associating a package needs a licence file or a domain restart, and cannot see ErrorDetails at all. Every package name a client learns here is one no real account publishes, so an association plan built against the emulator names packages that do not exist on AWS. It is also the upstream of Cause 1: the packages the listing publishes are exactly the ones whose history read is refused.
Fix: the package-catalog seed/state behind DescribePackages and the PackageDetails projection (it must emit AvailablePackageConfiguration and ErrorDetails, and the published set must be the AWS-managed catalog).
Traces: opensearch-catalog-describe-packages#task#1.

Related but NOT covering these: 20261114T093806Z-opensearch-package-catalog-diverges-from-aws-describepackages-serves.md is RESOLVED (its resolution changed only the FORM of the version label to 'v<n>' and declared the catalog contents divergent by design), and the two clusters above are still reported by this replay with owned_by=[] and stub_path="". 20261029T190009Z-es-package-vpc-endpoint-tagging-and-reserved-instance.md is open but concerns `es` AssociatePackages/DissociatePackages refusal wording, not the opensearch catalog contents or the GetPackageVersionHistory status channel.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, score_pct 69.5).

Cause 1 — opensearch-catalog-get-package-version-history#task#1 (GetPackageVersionHistory), verdict mismatch, expected_status 200, actual_status 400, affected_count 90 (task#1 .. task#25 and beyond in the cluster):
  status: expected 200, got 400
  $.PackageID: missing (expected the requested pkg-<hex> id echoed, got None)
  $.PackageVersionHistoryList: missing (expected a 2-entry newest-first history [{PackageVersion 'v2', CreatedAt, PackageConfiguration{ConfigurationRequirement: OPTIONAL, LicenseRequirement: REQUIRED, LicenseFilepath, RequiresRestartForConfigurationUpdate: true}, PluginProperties{Name, Description, Version, ClassName, UncompressedSizeInBytes}}, {PackageVersion 'v1', ...}], got None)
  divergence_key: service=opensearch|GetPackageVersionHistory|status=200->400|keys=$.PackageID,$.PackageVersionHistoryList,status
  Modeled refusal for a resource that does not exist: ResourceNotFoundException, httpError 409 — not the 400 the emulator answers.

Cause 2 — opensearch-catalog-describe-packages#task#1 (DescribePackages), verdict mismatch, expected_status 200, actual_status 200, body diverges:
  $.PackageDetailsList[0]: expected an AWS-managed ZIP-PLUGIN entry with PackageOwner set, EngineVersion 'OpenSearch_2.17', AvailablePackageVersion 'v2', AvailablePackageConfiguration{ConfigurationRequirement: OPTIONAL, LicenseRequirement: REQUIRED, LicenseFilepath, RequiresRestartForConfigurationUpdate: true} and ErrorDetails present (null when not in error); got an 'analysis-phonetic' entry with EngineVersion 'Elasticsearch_7.10', no AvailablePackageConfiguration and no ErrorDetails
  divergence_key enumerates $.PackageDetailsList[0] .. $.PackageDetailsList[92] — every element of the listing diverges

Both clusters: stub_path="", owned_by=[].

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. replay_wire_corpus() over trace set simparity-wire-aws-v1.
3. Cause 1: fixture opensearch-catalog-get-package-version-history, step task#1 — GET /2021-01-01/packages/{PackageID}/history for a PackageID the service's own DescribePackages publishes. The emulator answers 400 with no PackageID and no PackageVersionHistoryList; AWS answers 200 with the id echoed and the newest-first version history. Then request a well-formed but unresolvable pkg-<hex> id: the refusal must be ResourceNotFoundException with HTTP 409, not 400.
4. Cause 2: fixture opensearch-catalog-describe-packages, step task#1 — POST /2021-01-01/packages/describe with no filters, and compare PackageDetailsList with the recording: the emulator returns ~93 legacy Elasticsearch_7.10 analysis plugins and omits AvailablePackageConfiguration and ErrorDetails, so no element matches.
5. Both steps are reported by replay_wire_corpus as mismatches with stub_path="" and owned_by=[].

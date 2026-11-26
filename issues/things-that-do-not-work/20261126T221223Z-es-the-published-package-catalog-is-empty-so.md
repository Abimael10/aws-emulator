Issue-Source: trace-sweep-divergence
Issue-Trace: es-catalog-get-package-version-history
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: es-catalog-get-package-version-history es-catalog-describe-packages
Issue-Shape-Id: com.amazonaws.elasticsearchservice#AmazonElasticsearchService2015
Issue-Key: es|GetPackageVersionHistory|2xx->4xx|$.PackageID
Issue-Key: es|GetPackageVersionHistory|2xx->4xx|$.PackageVersionHistoryList
Issue-Key: es|GetPackageVersionHistory|2xx->4xx|status
Issue-Key: es|DescribePackages|2xx->2xx|$.PackageDetailsList[*]
Issue-Commit: 5c5c591725aedc34d8f099d3b5a455b07de0e5f1

# es: the published package catalog is empty, so neither package read answers as AWS does (2 causes)

> _Note: this issue was identified against the code base at commit `5c5c591725aedc34d8f099d3b5a455b07de0e5f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPackageVersionHistory

## Description
The emulator's Elasticsearch Service (`es`) package plane publishes no packages at all: the account-visible package catalog that DescribePackages projects is empty, and the per-package version-history read of an id the service is supposed to publish is answered on an unmodelled 404 channel. The two causes share one root: there is no seeded/queryable package catalog behind the `es` package operations, and the GetPackageVersionHistory handler's not-found branch is not the modelled one. 2 causes.

Cause 1 of 2: GetPackageVersionHistory answers HTTP 404 with an empty body for a PackageID that the service's own package catalog publishes, instead of 200 with the id echoed and the newest-first version history; and the genuinely-unresolvable case is answered 404, which is not a status this operation models at all.
AWS: Smithy (com.amazonaws.elasticsearchservice): GetPackageVersionHistory GET /2015-01-01/packages/{PackageID}/history, success code 200; input member PackageID is a required @httpLabel, MaxResults (@httpQuery maxResults, @range max 100) and NextToken (@httpQuery nextToken) are the paginator's inputToken/pageSize; output members PackageID, PackageVersionHistoryList (list of PackageVersionHistory{PackageVersion, CommitMessage @length 0..160, CreatedAt timestamp}) and NextToken. Modelled errors: AccessDeniedException (403), BaseException (400, defaulted), InternalException (500), ResourceNotFoundException (409), ValidationException (400). ResourceNotFoundException is documented "An exception for accessing or deleting a resource that does not exist", so a well-formed PackageID that resolves to nothing is a 409 ResourceNotFoundException — 404 is not in the operation's error set. For a package visible to the account AWS answers 200, echoes the requested PackageID and returns one PackageVersionHistoryList entry per published version, newest first (recorded response: PackageID echoed, two entries, PackageVersion 'v2' then 'v1', each with CreatedAt and a null CommitMessage).
Emulator: answers HTTP 404 with no PackageID and no PackageVersionHistoryList in the body, for every PackageID the corpus reads. The requirement here is the status/shape contract, not any one recorded id: a package the service publishes must have a readable newest-first history keyed by its own PackageID, and a well-formed-but-unresolvable PackageID must be refused ResourceNotFoundException with HTTP 409 and that error class's fixed AWS text, whose only variable part is the requested PackageID taken from the request path — the specific pkg-<hex> value in the recording fills that template and is not part of it.
Why: the normal client sequence is DescribePackages to enumerate, then GetPackageVersionHistory per package to pick a PackageVersion before AssociatePackage. Against the emulator every history read is a dead end, so version selection cannot be automated; and because the refusal arrives as 404 — a status the SDK's modelled error set for this operation does not contain — SDK/CLI callers get an unmodelled/opaque HTTP failure rather than taking their ResourceNotFoundException branch. This is the widest cluster in the corpus: 91 replayed steps.
Fix: the GetPackageVersionHistory handler (its route/lookup and its not-found branch) plus the `es` package-catalog state it reads.
Traces: es-catalog-get-package-version-history#task#1 (representative; affected_count 91, task#1 .. task#25 and beyond in the same cluster).

Cause 2 of 2: DescribePackages answers 200 with an empty PackageDetailsList where AWS publishes the account-visible package catalog, so no element of the listing is served.
AWS: Smithy: DescribePackages POST /2015-01-01/packages/describe, success 200; input Filters (DescribePackagesFilterList), MaxResults (@range max 100), NextToken; output PackageDetailsList + NextToken (paginated). Smithy PackageDetails members: PackageID ("Internal ID of the package"), PackageName (@length 3..28, pattern ^[a-z][a-z0-9\-]+$), PackageType (enum), PackageDescription (@length 0..1024), PackageStatus (enum COPYING/COPY_FAILED/VALIDATING/VALIDATION_FAILED/AVAILABLE/DELETING/DELETED/DELETE_FAILED), CreatedAt, LastUpdatedAt, AvailablePackageVersion, ErrorDetails ("Additional information if the package is in an error state. Null otherwise."). Recorded behavior only for the catalog's contents: an unfiltered describe returns a populated PackageDetailsList — 92 entries in the recording — of plugin packages at PackageStatus AVAILABLE with an AvailablePackageVersion ('v1'..'v6'), a PackageDescription, distinct CreatedAt/LastUpdatedAt instants and ErrorDetails present-and-null. Note the on-disk model's PackageType enum lists only TXT-DICTIONARY while the recording carries PackageType 'ZIP-PLUGIN', so the serialized PackageType must carry the service's value rather than be clamped to the modelled enum.
Emulator: answers 200 but every element the recording expects is absent — the replay reports $.PackageDetailsList[0] through $.PackageDetailsList[91] as "missing ... got None", i.e. the served PackageDetailsList is empty. The requirement is that the service publishes a non-empty catalog and projects each entry's full PackageDetails member set (including PackageType, AvailablePackageVersion, PackageStatus and an explicit ErrorDetails member); server-minted PackageIDs and CreatedAt/LastUpdatedAt instants are per-environment values and are not requirements, but which packages are published, their names, type, status and available version are the service's catalog.
Why: a client can discover no package at all, so nothing downstream of discovery is reachable — it cannot read a package's available version, cannot see whether a package is in an error state (ErrorDetails), and cannot build an association plan. It is also the upstream of Cause 1: the ids whose history read 404s are exactly the ones this listing should have published.
Fix: the `es` package-catalog seed/state behind DescribePackages and its PackageDetails projection/serializer.
Traces: es-catalog-describe-packages#task#1.

Related but NOT covering either cause: 20261122T115902Z-opensearch-the-published-package-catalog-still-answers-neither.md is open for the SEPARATE `opensearch` service (different API version, and there the catalog is populated-but-wrong and the history read answers 400, not an empty catalog and 404). 20261029T190009Z-es-package-vpc-endpoint-tagging-and-reserved-instance.md is open but concerns `es` AssociatePackages/DissociatePackages refusal WORDING, not the catalog contents or the GetPackageVersionHistory status channel. Both replay clusters are reported with stub_path="" and owned_by=[].

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false, score_pct 16.4, steps_total 159, steps_passed 26, steps_mismatched 133).

Cause 1 — es-catalog-get-package-version-history#task#1 (GetPackageVersionHistory), verdict mismatch, expected_status 200, actual_status 404, affected_count 91 (task#1 .. task#25 and beyond in the cluster):
  status: expected 200, got 404
  $.PackageID: missing (expected the requested pkg-<hex> id echoed, got None)
  $.PackageVersionHistoryList: missing (expected a 2-entry newest-first history [{PackageVersion 'v2', CreatedAt, CommitMessage null}, {PackageVersion 'v1', CreatedAt, CommitMessage null}], got None)
  divergence_key: service=es|GetPackageVersionHistory|status=200->404|keys=$.PackageID,$.PackageVersionHistoryList,status
  Modelled refusal for a resource that does not exist: ResourceNotFoundException, httpError 409. 404 is not in this operation's modelled error set (403/400/500/409/400).

Cause 2 — es-catalog-describe-packages#task#1 (DescribePackages), verdict mismatch, expected_status 200, actual_status 200, body diverges:
  $.PackageDetailsList[0]: missing (expected a ZIP-PLUGIN entry with PackageName, PackageDescription, PackageStatus 'AVAILABLE', AvailablePackageVersion 'v2', CreatedAt, LastUpdatedAt and ErrorDetails present-and-null; got None)
  $.PackageDetailsList[1] .. $.PackageDetailsList[91]: likewise missing (got None)
  divergence_key enumerates $.PackageDetailsList[0] .. $.PackageDetailsList[91] — the served list is empty, so no element of the listing matches

Both clusters: stub_path="", owned_by=[].

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. replay_wire_corpus() over trace set simparity-wire-aws-v1.
3. Cause 2: fixture es-catalog-describe-packages, step task#1 — POST /2015-01-01/packages/describe with no Filters. The emulator answers 200 with an empty PackageDetailsList; AWS answers 200 with the account-visible catalog (92 entries in the recording), each a full PackageDetails including PackageType, PackageStatus AVAILABLE, AvailablePackageVersion and an explicit ErrorDetails member.
4. Cause 1: fixture es-catalog-get-package-version-history, step task#1 — GET /2015-01-01/packages/{PackageID}/history for a PackageID the service's own DescribePackages publishes. The emulator answers 404 with no PackageID and no PackageVersionHistoryList; AWS answers 200 with the id echoed and the newest-first version history (v2 then v1).
5. Then GET the same path with a well-formed PackageID that resolves to nothing: the refusal must be ResourceNotFoundException with HTTP 409 and AWS's text for that error class (the requested PackageID being its only variable part), not 404.
6. Both steps are reported by replay_wire_corpus as mismatches with stub_path="" and owned_by=[].

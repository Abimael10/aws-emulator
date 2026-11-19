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
Issue-Commit: 8515d06fefc8f03e29b5ebfae2a7b9211fa7d3b5

# es package plane serves an emulator-authored plugin catalog: DescribePackages lists the wrong set and GetPackageVersionHistory refuses real catalog PackageIDs

> _Note: this issue was identified against the code base at commit `8515d06fefc8f03e29b5ebfae2a7b9211fa7d3b5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPackageVersionHistory

## Description
The 2015-01-01 Elasticsearch Service package plane answers from a read-only table of invented plugin rows (services/service-com-amazonaws-elasticsearchservice-amazonelasticsearchservice2015/src/state/package_catalog.rs, whose own module doc states the row set 'is this emulator's own stand-in for the catalogue a Region serves ... not a transcription of what the service offers today'). That one substitution produces two distinct wire faults, filed here as 2 causes: the catalog listing reports a set of packages no account would see, and every per-package read of a PackageID the real catalog publishes is refused outright.

Cause 1 of 2: GetPackageVersionHistory refuses a PackageID that the service's own package listing publishes.
AWS: Smithy (com.amazonaws.elasticsearchservice) GetPackageVersionHistory, GET /2015-01-01/packages/{PackageID}/history, success code 200; output members PackageID and PackageVersionHistoryList (PackageVersionHistory{PackageVersion, CommitMessage, CreatedAt}). For a package visible to the account — one it created, or one the service publishes and DescribePackages lists — AWS answers 200 with the PackageID echoed and one history entry per published version, newest first (each entry's CreatedAt is the instant that revision was minted, CommitMessage null for a service-published revision). The recording shows exactly that: 200, PackageID echoed, a two-entry history (v2 then v1). The only modeled not-found answer is ResourceNotFoundException, annotated @httpError(409) in this model; 404 is not a status this operation models at all.
Emulator: HTTP 404 with no PackageID and no PackageVersionHistoryList. The handler is implemented (src/ops/get_package_version_history.rs) and resolves the id through state::visible_package, which is 'a package this account created, else one published_package() publishes' — so a well-formed PackageID that the real catalog publishes resolves to nothing and falls into the not-found branch, which deliberately answers crate::scaffolded::RESOURCE_NOT_FOUND_STATUS (404) with ResourceNotFoundException message 'Package not found: <PackageID>' instead of the modeled 409. Fixed text of that refusal is 'Package not found: ' with the request's PackageID as its only variable part; the status it is served on (404) diverges from the model's 409 regardless of which package is asked for.
Why: the normal way to pick a PackageVersion before AssociatePackage is to enumerate packages and then read each one's history. Against this emulator every such read is a hard client error, so package-version selection cannot be automated at all; and because the refusal arrives as 404 rather than the modeled 409 ResourceNotFoundException, SDK/CLI error handling keyed on the modeled exception does not even classify it. This is the widest divergence in the corpus: 91 replayed steps in this one cluster.
Fix: the package catalog state the GetPackageVersionHistory handler reads (state/package_catalog.rs and state::visible_package / published_package) must carry the packages the service actually publishes with their per-version history, and the handler's not-found branch must serve the modeled ResourceNotFoundException status (409), not the scaffold's 404.
Traces: es-catalog-get-package-version-history#task#1 (representative; 91 affected steps, task#1..task#25 and beyond in the same cluster).

Cause 2 of 2: DescribePackages returns the emulator's stand-in plugin table instead of the packages the service publishes.
AWS: Smithy DescribePackages, POST /2015-01-01/packages/describe, success 200, output PackageDetailsList of PackageDetails{PackageID, PackageName, PackageType, PackageDescription, PackageStatus, CreatedAt, LastUpdatedAt, AvailablePackageVersion, ErrorDetails}; the operation is documented as describing all packages available to Amazon ES, i.e. the account's own packages plus the catalog the service publishes. The recording is a 200 whose PackageDetailsList is the current published catalog: third-party/marketplace ZIP-PLUGIN entries (for example a vendor name-matching plugin at AvailablePackageVersion v2, PackageStatus AVAILABLE) each carrying an ErrorDetails member that is null for an AVAILABLE package and distinct CreatedAt/LastUpdatedAt instants per revision.
Emulator: a 200 whose PackageDetailsList is a different set entirely — the invented legacy Elasticsearch analysis-plugin rows (analysis-kuromoji, analysis-phonetic and siblings) with PackageIDs derived from the row name, AvailablePackageVersion v1, a single shared publication instant for CreatedAt and LastUpdatedAt, and no ErrorDetails member emitted. No element of the list matches: the divergence covers $.PackageDetailsList[0] through [92].
Why: a client cannot discover any package the service actually offers; every PackageID it learns here names nothing in a real account, so a plan built from this listing (associate plugin X version v2 with a domain) fails on first contact with AWS. It is also the upstream of Cause 1 — the ids AWS publishes are exactly the ones the history read refuses.
Fix: the published-package catalog table behind DescribePackages (state/package_catalog.rs, read through state::visible_packages) and the PackageDetails projection built from it, which must also emit ErrorDetails for a catalog entry.
Traces: es-catalog-describe-packages#task#1.

Related but NOT covering either cause: 20261114T093806Z-opensearch-package-catalog-diverges-from-aws-describepackages-serves.md files the same substitution on the `opensearch` (2021-01-01) surface — package_catalog.rs's own doc names that crate's table as the sibling statement of this one — and 20261029T190009Z-es-package-vpc-endpoint-tagging-and-reserved-instance.md covers es package REFUSAL WORDING (AssociatePackages/DissociatePackages), not the catalog contents or this status. Replay reports owned_by=[] and stub_path="" for both clusters filed here. The third es cluster in this replay (CreatePackage, es-catalog/elasticsearch-service-package-lifecycle#task#6) is owned by 20261029T190009Z-es-ten-requests-real-aws-refuses-are-accepted.md and is not re-filed.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (score 15.7%, 3 divergence clusters, 2 net-new).

Cause 1 — es-catalog-get-package-version-history#task#1 (GetPackageVersionHistory), 91 affected steps, verdict mismatch:
  status: expected 200, got 404
  $.PackageID: missing (expected the requested pkg-<hex> id echoed, got None)
  $.PackageVersionHistoryList: missing (expected a two-entry history [{PackageVersion 'v2', CreatedAt, CommitMessage null}, {PackageVersion 'v1', CreatedAt, CommitMessage null}], got None)
  Emulator refusal served: 404 + ResourceNotFoundException 'Package not found: <PackageID>' (src/ops/get_package_version_history.rs, the crate::scaffolded::Scaffolded::not_found branch), where the model annotates ResourceNotFoundException @httpError(409).

Cause 2 — es-catalog-describe-packages#task#1 (DescribePackages), status 200 vs 200, every element of PackageDetailsList diverges:
  $.PackageDetailsList[0]: expected a published ZIP-PLUGIN catalog entry (PackageStatus AVAILABLE, AvailablePackageVersion 'v2', distinct CreatedAt/LastUpdatedAt per revision, ErrorDetails member present and null); got an 'analysis-kuromoji' entry with AvailablePackageVersion 'v1', CreatedAt == LastUpdatedAt == one shared constant and no ErrorDetails member
  $.PackageDetailsList[1]: expected the v1 revision of the same published vendor plugin; got 'analysis-phonetic'
  divergence_key lists $.PackageDetailsList[0] .. $.PackageDetailsList[92] — no element matches.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from that checkout.
2. Replay fixture es-catalog-get-package-version-history, step task#1: GET /2015-01-01/packages/{PackageID}/history for a PackageID that the service's package catalog publishes. The emulator answers 404 with no PackageID and no PackageVersionHistoryList; AWS answers 200 with the id echoed and a newest-first version history. The same cluster covers task#1..task#25 and the rest of its 91 affected steps.
3. Replay fixture es-catalog-describe-packages, step task#1: POST /2015-01-01/packages/describe with no filters, and compare PackageDetailsList against the recording — the emulator returns ~92 invented legacy analysis-plugin rows instead of the published catalog and omits ErrorDetails.
4. replay_wire_corpus reports both clusters as verdict=mismatch with stub_path="" and owned_by=[].

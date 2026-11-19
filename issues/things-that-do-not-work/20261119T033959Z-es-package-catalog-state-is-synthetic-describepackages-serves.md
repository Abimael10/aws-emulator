Issue-Source: trace-sweep-divergence
Issue-Trace: es-catalog-describe-packages
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: es-catalog-describe-packages es-catalog-get-package-version-history
Issue-Shape-Id: com.amazonaws.elasticsearchservice#AmazonElasticsearchService2015
Issue-Key: es|DescribePackages|2xx->2xx|$.PackageDetailsList[*]
Issue-Key: es|GetPackageVersionHistory|2xx->4xx|$.PackageID
Issue-Key: es|GetPackageVersionHistory|2xx->4xx|$.PackageVersionHistoryList
Issue-Key: es|GetPackageVersionHistory|2xx->4xx|status
Issue-Commit: 6e2d7185f30747a2356e601c1d412ac8e0be11c4

# es package catalog state is synthetic: DescribePackages serves a built-in seed list and GetPackageVersionHistory answers 404

> _Note: this issue was identified against the code base at commit `6e2d7185f30747a2356e601c1d412ac8e0be11c4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribePackages

## Description
2 net-new root causes on the es (OpenSearch/Elasticsearch Service) package surface. They share one fault: the emulator has no real package catalog state — the package read path is backed by a fixed built-in seed list instead of the packages of the account/region, and the per-package version history does not exist at all.

Cause 1 of 2 - DescribePackages serves a fabricated catalog instead of the account's packages.
AWS: Smithy DescribePackages, http POST /2015-01-01/packages/describe, code 200; output DescribePackagesResponse.PackageDetailsList is a list of PackageDetails plus NextToken (paginated inputToken=NextToken / outputToken=NextToken / pageSize=MaxResults, MaxResults @range max 100). Each PackageDetails describes one package that exists in the caller's account/region, carrying PackageID, PackageName, PackageType, PackageDescription, PackageStatus, CreatedAt, LastUpdatedAt, AvailablePackageVersion and the ErrorDetails member. Recorded behavior: the recorded list is that account's own packages (e.g. PackageName 'babel-street-match', PackageType ZIP-PLUGIN, PackageStatus AVAILABLE, AvailablePackageVersion 'v2' for the package whose history has two versions and 'v1' for the one that has one), with a DISTINCT CreatedAt and LastUpdatedAt per package and per version, and ErrorDetails present (null when there is no error).
Emulator: returns a constant built-in seed catalog that has nothing to do with the account's packages — entries such as 'analysis-kuromoji' and 'analysis-phonetic', every entry stamped with the same placeholder CreatedAt/LastUpdatedAt (1600000000), AvailablePackageVersion always 'v1' regardless of version history, and no ErrorDetails member emitted. The divergence covers the entire list, $.PackageDetailsList[0] through $.PackageDetailsList[91]: not one index matches.
The requirement is structural, not literal: generated PackageIDs, names and timestamps differ per environment and are not themselves requirements. What is required is that PackageDetailsList enumerates the packages the account actually holds (those minted by CreatePackage and not deleted), that CreatedAt/LastUpdatedAt are each package's own creation/last-update instants rather than one shared constant, that AvailablePackageVersion is the latest version in that package's version history, and that the ErrorDetails member is present.
Why: a client cannot discover or page the packages it created — it reads back a catalog of packages it never made, with a version that is always 'v1' and timestamps that are all identical, so package-association and package-update flows built against the emulator break on first contact with AWS.
Fix: the es package state (the package registry that CreatePackage/DeletePackage write and DescribePackages reads) and the DescribePackages handler's projection of it; the seed list must stop being the response source.
Traces: es-catalog-describe-packages#task#1.

Cause 2 of 2 - GetPackageVersionHistory is not served: every call answers HTTP 404.
AWS: Smithy GetPackageVersionHistory, http GET /2015-01-01/packages/{PackageID}/history (PackageID is an @httpLabel, required), code 200; output echoes PackageID and returns PackageVersionHistoryList, one PackageVersionHistory per version carrying PackageVersion, CreatedAt and CommitMessage (null when the version was created without one), newest version first, plus NextToken (paginated, maxResults/nextToken @httpQuery). The operation's modeled not-found error is ResourceNotFoundException with httpError 409, so even an unknown PackageID is NEVER answered 404 by AWS.
Emulator: HTTP 404 with neither $.PackageID nor $.PackageVersionHistoryList in the body, for every package id in the fixture — 91 affected steps in the cluster. Hypothesis: the route for /2015-01-01/packages/{PackageID}/history is unrouted or the handler cannot resolve any PackageID against the seed catalog of cause 1, so the request never reaches a modeled response.
Why: the version history of a package is unreadable, and the failure is indistinguishable from a wrong URL rather than a modeled service error — a client cannot tell 'this package has no such version' from 'this API does not exist', and retry/error handling keyed on ResourceNotFoundException (409) never fires.
Fix: the GetPackageVersionHistory handler and its route binding, reading the same package state as cause 1; a genuinely unknown PackageID must be refused with the modeled ResourceNotFoundException at HTTP 409, never 404.
Traces: es-catalog-get-package-version-history#task#1 (cluster of 91 steps, task#1..task#25 and beyond in that fixture).

Not covered by this issue: the CreatePackage S3-reachability refusal on this surface is already owned by the open issue 20261029T190009Z-es-ten-requests-real-aws-refuses-are-accepted.md (its cause 7) and is deliberately excluded here.

## Evidence
Cause 1 (es-catalog-describe-packages#task#1, DescribePackages, status 200 -> 200, every element of PackageDetailsList mismatched):
  expected $.PackageDetailsList[0] = {'PackageID': 'pkg-10b3c5c4ab016794431c100a4963d8d1e07e3673', 'PackageName': 'babel-street-match', 'PackageType': 'ZIP-PLUGIN', 'PackageStatus': 'AVAILABLE', 'AvailablePackageVersion': 'v2', 'CreatedAt': 1745506273.179, 'LastUpdatedAt': 1753272650.518, 'ErrorDetails': None, 'PackageDescription': '<the package's own description>'}
  actual   $.PackageDetailsList[0] = {'PackageID': 'pkg-5165978b544136d8cb1090c13a2f03aa945c1699', 'PackageName': 'analysis-kuromoji', 'PackageType': 'ZIP-PLUGIN', 'PackageStatus': 'AVAILABLE', 'AvailablePackageVersion': 'v1', 'CreatedAt': 1600000000, 'LastUpdatedAt': 1600000000, 'PackageDescription': 'Kuromoji analysis plugin: ...'} (no ErrorDetails member)
  actual   $.PackageDetailsList[1] = {'PackageName': 'analysis-phonetic', 'CreatedAt': 1600000000, 'LastUpdatedAt': 1600000000, 'AvailablePackageVersion': 'v1', ...} — i.e. a seed catalog, with identical placeholder timestamps on every entry.
  Mismatching keys reported by the replay: $.PackageDetailsList[0] .. $.PackageDetailsList[91] (all 92 indices).
  The per-package PackageID/name/description/timestamps above are one recording's values and are NOT the requirement; the requirement is that the entries are the account's own packages with per-package timestamps, the version-history-derived AvailablePackageVersion and a present ErrorDetails member.

Cause 2 (es-catalog-get-package-version-history#task#1, GetPackageVersionHistory, 91 affected steps):
  expected status 200; actual status 404.
  expected $.PackageID = the requested package id (recording: 'pkg-10b3c5c4ab016794431c100a4963d8d1e07e3673'); actual: member absent.
  expected $.PackageVersionHistoryList = [{'PackageVersion': 'v2', 'CreatedAt': 1753272650.518, 'CommitMessage': None}, {'PackageVersion': 'v1', 'CreatedAt': 1745506273.179, 'CommitMessage': None}] (newest first, one entry per version, CommitMessage null when none was supplied); actual: member absent.
  Model: output code 200; the only not-found error bound to this operation is ResourceNotFoundException at httpError 409, so 404 is not a status this operation can produce.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture es-catalog-describe-packages, step task#1 — POST /2015-01-01/packages/describe. Observe HTTP 200 whose PackageDetailsList is the built-in seed catalog ('analysis-kuromoji', 'analysis-phonetic', ...), every entry carrying CreatedAt = LastUpdatedAt = 1600000000, AvailablePackageVersion 'v1' and no ErrorDetails member, instead of the packages the account holds; all 92 list indices mismatch.
3. Cause 2: replay fixture es-catalog-get-package-version-history, step task#1 (and task#2..task#25, 91 steps in the cluster) — GET /2015-01-01/packages/<PackageID>/history. Observe HTTP 404 with no PackageID and no PackageVersionHistoryList, instead of 200 with the echoed PackageID and the newest-first version list (or, for an unknown id, the modeled ResourceNotFoundException at 409).

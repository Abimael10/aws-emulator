Issue-Source: trace-sweep-divergence
Issue-Trace: es-catalog-get-package-version-history
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: es-catalog-get-package-version-history es-catalog-describe-packages elasticsearch-service-package-lifecycle
Issue-Shape-Id: com.amazonaws.elasticsearchservice#AmazonElasticsearchService2015
Issue-Key: es|GetPackageVersionHistory|2xx->4xx|$.PackageID
Issue-Key: es|GetPackageVersionHistory|2xx->4xx|$.PackageVersionHistoryList
Issue-Key: es|GetPackageVersionHistory|2xx->4xx|status
Issue-Key: es|DescribePackages|2xx->2xx|$.PackageDetailsList[*]
Issue-Key: es|CreatePackage|4xx->2xx|$.message
Issue-Key: es|CreatePackage|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: es|CreatePackage|4xx->2xx|status
Issue-Commit: cd8f73d475d74adb0ca8a470eb19f8c740cf481a

# es package plane diverges: the seeded package catalog replaces AWS's published packages, version history 404s, and an unreadable S3 source is accepted

> _Note: this issue was identified against the code base at commit `cd8f73d475d74adb0ca8a470eb19f8c740cf481a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetPackageVersionHistory

## Description
3 net-new root causes, all in the Elasticsearch Service (es) package plane: the package registry the emulator serves is an emulator-authored seed catalog rather than the packages the account actually holds, no package has a version history, and CreatePackage never validates that the PackageSource S3 object is readable.

Cause 1 of 3 - GetPackageVersionHistory answers 404 for every package in the catalog.
AWS: Smithy GET /2015-01-01/packages/{PackageID}/history (PackageID @httpLabel, required) returns 200 with PackageID echoed and PackageVersionHistoryList, one PackageVersionHistory entry per version of that package (PackageVersion, CreatedAt, optional CommitMessage), newest version first; a package id that resolves to nothing is refused with the modelled ResourceNotFoundException (httpError 409), and the modelled 4xx set for this operation is AccessDeniedException 403 / BaseException 400 / ValidationException 400 / ResourceNotFoundException 409 — 404 is not in it at all.
Emulator: answers HTTP 404 with an empty body — neither $.PackageID nor $.PackageVersionHistoryList is present — for package ids that DescribePackages is itself serving, so the read is unsatisfiable for every package a client can discover.
Why: a client paginating a package's versions to pick an AvailablePackageVersion for AssociatePackage gets a bare 404 that matches none of the modelled error shapes, so its SDK surfaces an unmodelled transport error instead of either the version list or a ResourceNotFoundException it can branch on. The whole version-history read path is unusable, and in this corpus the single cluster covers 91 replayed steps.
Fix: the GetPackageVersionHistory handler and the package state it reads — packages must carry their version history (version id, creation time, commit message) in state and the handler must resolve the {PackageID} label against it, refusing an unknown id as ResourceNotFoundException (409) rather than falling through to a 404.

Cause 2 of 3 - DescribePackages serves an emulator-authored seed catalog, not the account's packages.
AWS: Smithy POST /2015-01-01/packages/describe returns 200 with PackageDetailsList, one PackageDetails per package the account holds (PackageID, PackageName, PackageType, PackageDescription, PackageStatus, CreatedAt, LastUpdatedAt, AvailablePackageVersion, ErrorDetails), filtered by the request's Filters and paginated by MaxResults (@range max 100) / NextToken.
Emulator: returns 200, but the list is a fixed built-in plugin catalog authored in the emulator (entries named analysis-kuromoji, analysis-phonetic, ... each with AvailablePackageVersion 'v1', a constant CreatedAt/LastUpdatedAt and emulator-written PackageDescription prose), so every element of PackageDetailsList diverges from the recorded listing, which holds the packages the account actually created — different PackageIDs, names, descriptions, per-package versions and distinct CreatedAt/LastUpdatedAt stamps. The divergence covers every index of the list, so the read reflects none of the account's state.
Why: packages created through CreatePackage are invisible and packages that were never created are reported AVAILABLE, so the create -> describe -> associate flow cannot be driven: a client that lists the catalog to find the package it just uploaded never finds it, and one that picks an id from the listing names a package the account does not own. Note the per-package values (ids, descriptions, timestamps) differ per environment and are not themselves the requirement — the requirement is that the listing is derived from created-package state, and that each entry's AvailablePackageVersion and LastUpdatedAt track that package's own version history.
Fix: the DescribePackages handler and the package store it reads — project PackageDetails from created packages (and their version history) instead of a hard-coded catalog; keep Filters and MaxResults/NextToken applied over that projection.

Cause 3 of 3 - CreatePackage accepts a PackageSource whose S3 object is not readable.
AWS: Smithy POST /2015-01-01/packages with required PackageName (@length 3..28, pattern ^[a-z][a-z0-9\-]+$), required PackageType and required PackageSource; when the named S3 object cannot be read by the service (missing object, missing bucket, bucket in another region, no access), AWS refuses with HTTP 400 and the fixed message 'Package file not accessible. Please validate that you have access to the S3 object and S3 bucket is present in the same region.' — that sentence is entirely fixed text, with no part interpolated from the request, and it is carried in an error body with an x-amzn-errortype header (the recording asserts the header's presence but not its value; the modelled 400 errors for this operation are BaseException and ValidationException).
Emulator: answers HTTP 200 with a success PackageDetails and emits no $.message and no x-amzn-errortype header at all, i.e. the S3 source is never resolved and the package is created as if the artifact existed.
Why: the refusal is the only signal a client gets that its artifact never reached the service. Accepting it leaves a package in AVAILABLE that has no content behind it, so the failure is deferred to a later association or a domain plugin load, where it is undiagnosable; retry/rollback logic keyed on the 400 never runs.
Fix: CreatePackage's request validation/handler — resolve PackageSource (bucket, key, region and readability) before minting the package, and refuse an unreadable source with the 400 and that exact fixed sentence.

Related open issues that do NOT cover these: 20261029T190009Z-es-package-vpc-endpoint-tagging-and-reserved-instance.md covers es package/VPC/tag/reserved-instance REFUSAL MESSAGE wording (DissociatePackage, DissociatePackages, AssociatePackages, AddTags, ...) and nothing about the catalog reads or CreatePackage source validation; 20261122T115902Z-opensearch-the-published-package-catalog-still-answers-neither.md is filed against the opensearch service token, not es.

## Evidence
Cause 1 (es-catalog-get-package-version-history#task#1, cluster covers 91 steps incl. task#1..task#25): status expected 200, got 404; $.PackageID missing (expected the requested package id, got None); $.PackageVersionHistoryList missing (expected a two-entry list of {PackageVersion, CreatedAt, CommitMessage} newest-first, got None). 404 is not among the operation's modelled errors (ResourceNotFoundException is 409).

Cause 2 (es-catalog-describe-packages#task#1): status 200 both sides, but every index of $.PackageDetailsList diverges ($.PackageDetailsList[0] through [91]). Expected element shape, e.g. {PackageID: 'pkg-<hash>', PackageName: <the account's package name>, PackageType: 'ZIP-PLUGIN', PackageStatus: 'AVAILABLE', AvailablePackageVersion: 'v2', CreatedAt/LastUpdatedAt: that package's own distinct stamps, ErrorDetails: None}. Actual element: an emulator seed row, e.g. PackageName 'analysis-kuromoji' / 'analysis-phonetic', AvailablePackageVersion always 'v1', CreatedAt == LastUpdatedAt == 1600000000, PackageDescription emulator-written plugin prose. (The literal ids/names/timestamps above are one recording's values, not the requirement.)

Cause 3 (elasticsearch-service-package-lifecycle#task#6): status expected 400, got 200; $.message missing — expected the fixed text 'Package file not accessible. Please validate that you have access to the S3 object and S3 bucket is present in the same region.', got None; response header x-amzn-errortype missing (recording asserts presence, value not asserted), got None.

## Steps to Reproduce
1. Replay fixture es-catalog-get-package-version-history step task#1 (and task#2..task#25) against the emulator: GET /2015-01-01/packages/{PackageID}/history for a package id the catalog serves. Observe 404 with an empty body instead of 200 with PackageID + PackageVersionHistoryList.
2. Replay fixture es-catalog-describe-packages step task#1: POST /2015-01-01/packages/describe. Compare $.PackageDetailsList element-by-element with the recording — the emulator serves its built-in plugin seed rows rather than the account's packages, so every index diverges.
3. Replay fixture elasticsearch-service-package-lifecycle step task#6: CreatePackage with a PackageSource naming an S3 object the service cannot read. Observe HTTP 200 with a PackageDetails body instead of HTTP 400 with $.message 'Package file not accessible. Please validate that you have access to the S3 object and S3 bucket is present in the same region.' and an x-amzn-errortype header.

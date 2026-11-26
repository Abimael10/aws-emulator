Issue-Source: trace-sweep-divergence
Issue-Trace: lightsail-catalog-and-operations-lifecycle
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lightsail-catalog-and-operations-lifecycle lightsail-catalog-get-blueprints lightsail-catalog-get-bundles
Issue-Shape-Id: com.amazonaws.lightsail#Lightsail_20161128
Issue-Key: lightsail|GetBlueprints|2xx->2xx|$.blueprints[*].licenseUrl
Issue-Key: lightsail|GetBlueprints|2xx->2xx|$.blueprints[*].productUrl
Issue-Key: lightsail|GetBlueprints|2xx->2xx|$.blueprints[*]
Issue-Key: lightsail|GetBundles|2xx->2xx|$.bundles[*]
Issue-Commit: def095f01c34fc07414155d9c9d1d64186b4ea7c

# Lightsail published catalogs diverge from AWS: wrong blueprint URLs and missing blueprint and bundle entries

> _Note: this issue was identified against the code base at commit `def095f01c34fc07414155d9c9d1d64186b4ea7c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBlueprints

## Description
The two published catalog readers of this service — GetBlueprints and GetBundles — answer HTTP 200 with the right shape but the wrong CONTENT: the const catalog tables in `crate::state` publish entries AWS does not publish, and omit entries AWS does. Three distinct causes, all on the published-catalog tables, all reported as 200->200 body mismatches by the replay. THREE causes must be closed before this issue is submitted.

=== Cause 1 of 3: GetBundles publishes only the general-purpose plan columns, so better than half the plan catalog is absent (fixture lightsail-catalog-get-bundles, step task#1) ===

AWS: Smithy: `GetBundles` -> `GetBundlesResult` with members `bundles` (`BundleList`) and `nextPageToken`; documented as "Returns the bundles that you can apply to an Amazon Lightsail instance when you create it" (smithy_query operation=GetBundles, model services/service-com-amazonaws-lightsail-lightsail-20161128/smithy-model/com-amazonaws-lightsail-lightsail-20161128.json). The model does not enumerate the catalog's rows, so for the catalog's CONTENTS this is recorded behavior only: the recorded 200 answer to a default GetBundles (no `appCategory`, no `includeInactive`, no `pageToken`) reports at least 100 bundles in one page, and the entries past index 43 are plan FAMILIES beyond the general-purpose sizes — the memory-optimized plans, whose ids carry the family token (`m_large_1_0`, `m_xlarge_1_0`, `m_2xlarge_1_0`, `m_4xlarge_1_0`, `m_8xlarge_1_0`, `m_12xlarge_1_0`, `m_16xlarge_1_0`, ...) and whose hardware is that family's and not the same-named general-purpose size's (recorded `m_large_1_0`: instanceType `large`, cpuCount 2, ramSizeInGb 16, diskSizeInGb 160, power 3000, price 74.0, transferPerMonthInGb 5120, publicIpv4AddressCount 1, supportedPlatforms [LINUX_UNIX], isActive true — i.e. the same power and SSD as the general-purpose `large` plan but twice its RAM and its own price). The requirement is that the default page reports the whole published plan catalog, every family included, not one family of it.

Emulator: answers 200 with exactly 44 bundles and nothing after index 43: `crate::state::BUNDLE_SPECS` holds the 11 general-purpose sizes (nano..16xlarge) and `BUNDLE_COLUMNS` reports each size once per published column (Linux/Unix, Windows, and the IPv6-only column of each) — 11 x 4 = 44 rows — and no other plan family exists in the table at all. The replay reports `$.bundles[44] .. $.bundles[99]` (and on) as `missing (expected {...}, got None)`. The replay reports NO divergence on `$.nextPageToken`, so this is not a paging offset or a page-size difference that a second page would make up: the emulator ends the catalog after 44 rows and claims no further page.

Why: a client that enumerates instance plans to choose one — the console-style flow of listing plans and creating an instance on the chosen plan — never sees the memory-optimized plans against the emulator, so code that selects a RAM-per-vCPU profile picks a plan that does not match its sizing rule, or finds no plan at all and aborts. A client that already knows the plan it wants and calls CreateInstances with `bundleId: m_large_1_0` is refused against the emulator for a plan AWS publishes, which makes an unchanged deployment script fail only here. Any cost or capacity report built from this call is computed over fewer than half the published plans.

Fix: the published plan catalog this operation reads — the `BundleSpec` table and the column expansion in `crate::state` (state/bundles.rs) that `GetBundles`' handler reports through `crate::state::bundles(..)`/`bundle_page(..)`; the handler itself and the serializer are right. The same table also backs the bundle a CreateInstances/GetInstance answer resolves and reports, so the added families must resolve there too, not only list here.

Traces: lightsail-catalog-get-bundles#task#1 (GetBundles, expected 200 / actual 200, divergence_key `service=lightsail|GetBundles|status=200->200|keys=$.bundles[44],...,$.bundles[99]`).

=== Cause 2 of 3: GetBlueprints publishes invented Marketplace product-view ids and the wrong licence FORM, so productUrl/licenseUrl point at another listing (fixture lightsail-catalog-and-operations-lifecycle, step task#4) ===

AWS: Smithy: `GetBlueprints` -> `GetBlueprintsResult.blueprints` (`BlueprintList`); `Blueprint$productUrl` is documented "The product URL to learn more about the image or blueprint" and `Blueprint$licenseUrl` "The end-user license agreement URL for the image or blueprint" (smithy_query operation=GetBlueprints / shape=Blueprint). Neither member carries a `@pattern` and the model publishes no index of listings, so the VALUES are recorded behavior only: each entry's `productUrl` is the AWS page for THAT image — for a Marketplace-offered image its own listing, `https://aws.amazon.com/marketplace/pp/<product-view-or-product-code>` — and its `licenseUrl` is where AWS publishes the agreement accepted by launching THAT image, which the recording shows taking a form chosen per image: the listing page itself (identical to `productUrl`, no fragment), the listing's usage section (`productUrl` + `#pdp-usage`), or an agreement document on the licence host. The fixed part of each value is the host and the path shape; the variable part is the per-entry listing id / agreement path, which is the catalog's published value for that blueprint and must not be derived from a neighbour's or chosen.

Emulator: answers 200 with the same entry order and the same number of entries for this request, but from `$.blueprints[9]` on the two URL members name a DIFFERENT listing than AWS and/or the wrong licence form. Replay examples: `$.blueprints[9].licenseUrl` expected `https://aws.amazon.com/marketplace/pp/prodview-goqtn6wuqa2sc`, got `https://aws.amazon.com/marketplace/pp/prodview-goqtn6wuqa2sc#pdp-usage` (right listing, wrong FORM — the emulator appends the usage fragment where AWS publishes the terms as the listing page itself); `$.blueprints[12].productUrl` expected `https://aws.amazon.com/marketplace/pp/prodview-sxzreyutwngxy`, got `https://aws.amazon.com/marketplace/pp/prodview-m2wvqz7ktbxd5`; `$.blueprints[13].productUrl` expected `https://aws.amazon.com/marketplace/pp/B01N4R3GJI` (an older listing named by product code), got `https://aws.amazon.com/marketplace/pp/prodview-vz3mqkt8wbxd6`; `$.blueprints[16].productUrl` expected `https://aws.amazon.com/lightsail/features/#topic-0` (an AWS product page of its own, not a Marketplace listing), got `https://aws.amazon.com/marketplace/pp/prodview-z9qmkt3wvbxd8`. The source states the choice outright: `BlueprintSpec::product_page`'s own documentation says the path is "the recorded one on the rows a recorded answer names it for, and CHOSEN on the rest" (services/service-com-amazonaws-lightsail-lightsail-20161128/src/state/blueprints.rs lines ~188-190), and `BlueprintLicense`/`MARKETPLACE_USAGE_TERMS_FRAGMENT` (same file, ~231-299) pick the form per row — so every chosen row diverges, and the fixture asserts those rows.

Why: these members exist so a client can show or follow the page for the image it is about to launch, and the emulator's values are well-formed AWS URLs that resolve to a DIFFERENT product's listing (or to a listing where AWS publishes a product page). A console or portal built on this call links users to the wrong product and the wrong end-user licence agreement, and a compliance step that records which agreement a launch accepted records another product's terms. Nothing fails loudly: the call is a 200 and the URL is syntactically fine, so the error is only visible to whoever opens the link.

Fix: the published blueprint catalog's URL columns in `crate::state` (state/blueprints.rs): `BlueprintSpec::product_page` must carry each entry's own published path rather than a chosen `prodview-...` id, and `BlueprintSpec::license` must carry the form AWS publishes for that entry (listing page, listing usage section, or agreement document). `GetBlueprints`' handler and serializer are not involved.

Traces: lightsail-catalog-and-operations-lifecycle#task#4 (GetBlueprints, expected 200 / actual 200, divergence_key `service=lightsail|GetBlueprints|status=200->200|keys=$.blueprints[9].licenseUrl,$.blueprints[12..32].productUrl/licenseUrl`).

=== Cause 3 of 3: the blueprint catalog is short several published entries, and the Windows Server licence document is shared family-wide instead of per release (fixture lightsail-catalog-get-blueprints, step task#1) ===

AWS: same model citation as cause 2; the catalog's rows are recorded behavior only. The recorded 200 answer to this GetBlueprints request reports entries through index 38 — six more than the emulator serves — and reports each Windows Server release's `licenseUrl` as that RELEASE's own agreement document under the licence host: recorded `windows_server_2019` carries `https://d7umqicpi7263.cloudfront.net/eula/product/ef297a90-3ad0-4674-83b4-7f0ec07c39bb/8dcaef51-8e20-41b3-a622-57864d247f86.txt`. The fixed part of such a value is the licence host and the `/eula/product/<product>/<agreement>.txt` path shape; the variable parts are the product and agreement ids, which are the ones published for THAT image.

Emulator: answers 200 with a list that diverges from `$.blueprints[2]` on and ENDS before the entries AWS reports — the replay names `$.blueprints[33]` .. `$.blueprints[38]` among the diverging keys alongside the whole-entry mismatches at `$.blueprints[2]`..`$.blueprints[32]`, with no divergence reported on `$.nextPageToken`, so the catalog table simply holds fewer published images than AWS publishes (it is not a page boundary). Where an entry IS present it still diverges: `$.blueprints[2]` (`windows_server_2019`) matches AWS field for field except `licenseUrl`, where the emulator reports `https://d7umqicpi7263.cloudfront.net/eula/product/b0e459f5-2c38-4939-ad2a-d9784ecab690/0e7c7387-f24a-45b5-9206-e5ab5ff5fd9c.txt` — the constant `WINDOWS_SERVER_LICENSE_AGREEMENT` (state/blueprints.rs ~309-310), which the code deliberately shares across "more than one Windows Server entry" as "the Windows Server FAMILY's" one document. AWS publishes a distinct agreement document for this release, so every release that is not the one the constant was taken from reports another release's agreement.

Why: a client enumerating blueprints to build a launch menu is missing published images outright against the emulator — the image a deployment names may not be listed, and a CreateInstances call for it is refused for an image AWS offers — and every Windows Server release but one links the wrong licence document, so a launch records acceptance of an agreement that is not the image's.

Fix: the published blueprint catalog in `crate::state` (state/blueprints.rs): add the published entries the list is missing (keeping the catalog's own order, since the entry order is what the fixture indexes), and make the Windows Server licence a PER-ENTRY `BlueprintLicense::Agreement` path rather than one family-wide constant.

Traces: lightsail-catalog-get-blueprints#task#1 (GetBlueprints, expected 200 / actual 200, divergence_key `service=lightsail|GetBlueprints|status=200->200|keys=$.blueprints[2],...,$.blueprints[38]`).

=== Not covered by existing open issues ===

None of the open lightsail issues covers any of these three causes: `20261126T025250Z-lightsail-createkeypair-returns-publickeybase64-without-the-ssh-rsa.md` owns the CreateKeyPair `publicKeyBase64` rendering (the fourth divergence of this replay, skipped here as owned), `20261022T050650Z-lightsail-createkeypair-stub.md` is about CreateKeyPair's 501, the two container-service issues are about a public endpoint answering 501, and `20261028T134325Z-lightsail-getrelationaldatabaselogevents-validation.md` is about window coercion in GetRelationalDatabaseLogEvents. No open issue carries a replay key for GetBlueprints or GetBundles.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 34 steps, 30 passed, 4 mismatched (score 88.2%), emulator up (`/__admin/health` 200). Three of the four mismatches are the causes below (the fourth, CreateKeyPair, is owned by an existing issue).

Cause 1 — fixture `lightsail-catalog-get-bundles`, step `task#1`, GetBundles, verdict mismatch, expected 200 / actual 200:
  expected `$.bundles[44]` = {bundleId `m_large_1_0`, cpuCount 2, diskSizeInGb 160, instanceType `large`, isActive true, name `Large`, power 3000, price 74.0, publicIpv4AddressCount 1, ramSizeInGb 16.0, supportedPlatforms [`LINUX_UNIX`], transferPerMonthInGb 5120}; actual `None`.
  expected `$.bundles[45]` = `m_xlarge_1_0` (4 vCPU / 32 GB / 320 GB / power 4000 / 144.0); actual `None`. Likewise `$.bundles[46]` `m_2xlarge_1_0`, `[47]` `m_4xlarge_1_0`, `[48]` `m_8xlarge_1_0`, `[49]` `m_12xlarge_1_0`, `[50]` `m_16xlarge_...` and on through `$.bundles[99]`: all `missing (… got None)`.
  `$.nextPageToken` did NOT diverge, so the emulator reports its 44-row list as complete.
  Code: `BUNDLE_SPECS` in services/service-com-amazonaws-lightsail-lightsail-20161128/src/state/bundles.rs lines 169-410 holds 11 general-purpose sizes; `BundleColumn`/`BUNDLE_COLUMNS` (same file, lines 436-489) expands each size over 4 published columns = 44 rows; `ops/get_bundles.rs` lines 196-210 reports exactly that table.

Cause 2 — fixture `lightsail-catalog-and-operations-lifecycle`, step `task#4`, GetBlueprints, verdict mismatch, expected 200 / actual 200 (all `host_not_own`, i.e. real AWS URLs compared literally):
  `$.blueprints[9].licenseUrl`: expected `https://aws.amazon.com/marketplace/pp/prodview-goqtn6wuqa2sc`, got `…prodview-goqtn6wuqa2sc#pdp-usage`.
  `$.blueprints[12].productUrl`: expected `…/marketplace/pp/prodview-sxzreyutwngxy`, got `…/marketplace/pp/prodview-m2wvqz7ktbxd5`; `[12].licenseUrl` expected `…prodview-sxzreyutwngxy#pdp-usage`, got `…prodview-m2wvqz7ktbxd5#pdp-usage`.
  `$.blueprints[13].productUrl`/`.licenseUrl`: expected `…/marketplace/pp/B01N4R3GJI` for BOTH, got `…prodview-vz3mqkt8wbxd6` and `…prodview-vz3mqkt8wbxd6#pdp-usage`.
  `$.blueprints[14]`/`[15]`: expected `prodview-ykmb6re2rcouy` / `prodview-k66o7o642dfve`, got `prodview-q8wmzkt2vbxd4` / `prodview-t5zqmkw9vbxd7`.
  `$.blueprints[16].productUrl`: expected `https://aws.amazon.com/lightsail/features/#topic-0`, got `…/marketplace/pp/prodview-z9qmkt3wvbxd8`; `[16].licenseUrl` expected an agreement document on the licence host, got the same chosen listing + `#pdp-usage`.
  The same two members diverge on every entry from `[9]` through `[32]` (divergence_key key list).
  Code: services/service-com-amazonaws-lightsail-lightsail-20161128/src/state/blueprints.rs lines 173-191 (`product_page`, documented as recorded on some rows and "CHOSEN on the rest"), 231-254 (`BlueprintLicense` forms) and 299 (`MARKETPLACE_USAGE_TERMS_FRAGMENT`).

Cause 3 — fixture `lightsail-catalog-get-blueprints`, step `task#1`, GetBlueprints, verdict mismatch, expected 200 / actual 200:
  `$.blueprints[2]`: `missing` — expected the `windows_server_2019` entry with `licenseUrl` `https://d7umqicpi7263.cloudfront.net/eula/product/ef297a90-3ad0-4674-83b4-7f0ec07c39bb/8dcaef51-8e20-41b3-a622-57864d247f86.txt`; got the same entry with every other member equal and `licenseUrl` `https://d7umqicpi7263.cloudfront.net/eula/product/b0e459f5-2c38-4939-ad2a-d9784ecab690/0e7c7387-f24a-45b5-9206-e5ab5ff5fd9c.txt`.
  `$.blueprints[3]`..`$.blueprints[32]`: entry-level `missing`, driven by the same two URL members as cause 2.
  `$.blueprints[33]`..`$.blueprints[38]`: named in the divergence key list with no counterpart in the emulator's shorter list, and `$.nextPageToken` did not diverge.
  Code: state/blueprints.rs lines 309-310 (`WINDOWS_SERVER_LICENSE_AGREEMENT`, shared across Windows Server releases) and the `BLUEPRINTS` table it belongs to.

No cause involves an HTTP status or error-code difference: all three are 200 -> 200 body divergences, so no error message is at issue.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture `lightsail-catalog-get-bundles`, step `task#1` — or call `POST /ls/api/2016-11-28/GetBundles` (X-Amz-Target GetBundles) with an empty body `{}`. Count `bundles`: the emulator returns 44 entries and no `nextPageToken`; AWS returns the whole published catalog (100+ entries). Grep the response for a bundleId beginning `m_` (e.g. `m_large_1_0`): none is present.
3. Cause 2: replay fixture `lightsail-catalog-and-operations-lifecycle`, step `task#4` — or call `POST /ls/api/2016-11-28/GetBlueprints` with the same request the fixture records. Read `blueprints[9].licenseUrl` (it carries a `#pdp-usage` fragment AWS does not), and `blueprints[12..16].productUrl` (they name `prodview-…` ids other than the ones AWS publishes, including one entry whose AWS productUrl is not a Marketplace listing at all).
4. Cause 3: replay fixture `lightsail-catalog-get-blueprints`, step `task#1` — or call `POST /ls/api/2016-11-28/GetBlueprints` with an empty body `{}`. The list ends before the entries AWS reports at indices 33-38 while reporting no `nextPageToken`, and `blueprints[2]` (`windows_server_2019`) carries the family-wide Windows Server EULA path instead of that release's own agreement document.
5. In each case the status is 200 and the shape is valid, so the divergence is only visible by comparing the catalog contents against the recorded answer.

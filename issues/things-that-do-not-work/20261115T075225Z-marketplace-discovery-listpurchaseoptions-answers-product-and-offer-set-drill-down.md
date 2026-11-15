Issue-Source: trace-sweep-divergence
Issue-Trace: marketplace-discovery-purchase-options-filter-by-product-and-seller
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: marketplace-discovery-purchase-options-filter-by-product-and-seller marketplace-discovery-purchase-options-offer-drilldown marketplace-discovery-purchase-options-offerset-drilldown
Issue-Shape-Id: com.amazonaws.marketplacediscovery#AWSMarketplaceDiscovery
Issue-Key: marketplace-discovery|ListPurchaseOptions|2xx->2xx|$.purchaseOptions[*]
Issue-Key: marketplace-discovery|ListPurchaseOptions|2xx->2xx|$.purchaseOptions
Issue-Commit: 9b7bbc0d973267fb8051031d549f5a98fe90d52b

# ListPurchaseOptions answers product and offer-set drill-down queries with an empty purchaseOptions list

> _Note: this issue was identified against the code base at commit `9b7bbc0d973267fb8051031d549f5a98fe90d52b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListPurchaseOptions

## Description
ListPurchaseOptions returns HTTP 200 with an EMPTY `purchaseOptions` list for the filtered drill-down queries a discovery client issues after it has located a product/offer, where AWS returns at least one PurchaseOptionSummary. 2 causes, both on marketplace-discovery/ListPurchaseOptions.

Cause 1 of 2 — a PRODUCT_ID filter combined with a SELLER_OF_RECORD_PROFILE_ID filter matches nothing (fixture marketplace-discovery-purchase-options-filter-by-product-and-seller, step task#1; same cluster also covers marketplace-discovery-purchase-options-offer-drilldown#task#1).
  AWS: Smithy ListPurchaseOptions (POST /2026-02-05/listPurchaseOptions, success 200, @paginated items=purchaseOptions, output purchaseOptions: PurchaseOptionSummaryList @length max 100). Operation doc: "Returns the purchase options (offers and offer sets) available to the buyer. You can filter results by product, seller, purchase option type, visibility scope, and availability status." PurchaseOptionFilter$filterValues doc: "PRODUCT_ID - One or more product identifiers to filter by", "SELLER_OF_RECORD_PROFILE_ID - One or more seller profile identifiers to filter by", and "OR logic combines multiple values within the same filter" (multiple filters AND). For a product that HAS an offer whose seller of record is the named seller, AWS returns that offer as one PurchaseOptionSummary carrying the shape's required members purchaseOptionId, catalog, purchaseOptionType (OFFER for a single-product offer), sellerOfRecord and associatedEntities (the product + offer pair), plus any badges. The recorded response for this step is one such OFFER summary whose associatedEntities names the filtered product and whose sellerOfRecord/manufacturer is the filtered seller profile; the recording's concrete id, product name and seller display name are that run's values, not part of the requirement.
  Emulator: the response body has no element at $.purchaseOptions[0] at all - replay reports "$.purchaseOptions[0]: missing (expected {...purchaseOptionType: OFFER, associatedEntities: [...]...}, got None)" against expected_status 200 / actual_status 200. The list is served empty rather than absent-with-error, so nothing in the AND-join of the two filters selected a row.
  Why: a client that drills from a located product (or from a seller's catalog) into its purchase options concludes the product is not purchasable - it has no offer and no offer set - and the whole discovery-to-procurement flow dead-ends. Because the status is 200 and the modeled list is simply empty, the client cannot distinguish "nothing matched" from "the backing data is missing", so there is no retry or error path that recovers.
  Fix: ListPurchaseOptions' candidate selection - the projection the handler pages over (services/service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery/src/state.rs, ComAmazonawsMarketplacediscoveryAwsmarketplacediscoveryState::purchase_options) and the per-filter AND-join it is narrowed with (src/ops/list_purchase_options.rs, matches_every_filter / the handler body). Hypothesis: the entry a PRODUCT_ID+SELLER_OF_RECORD_PROFILE_ID pair must match is keyed on facts the drill-down's own inputs never carry together (entry.product_ids from the offer vs entry.seller_profile_id taken only from the offer's sellerOfRecord, and offers seeded with visibility_scope PUBLIC), so the conjunction is unsatisfiable for the ids the preceding discovery steps hand back; the modeled behavior is that an offer reachable for that product and that seller of record is returned.
  Traces: marketplace-discovery-purchase-options-filter-by-product-and-seller#task#1 (representative), marketplace-discovery-purchase-options-offer-drilldown#task#1.

Cause 2 of 2 — the offer-set drill-down's ListPurchaseOptions call returns [] (fixture marketplace-discovery-purchase-options-offerset-drilldown, step task#2).
  AWS: same operation and same output contract as above. PurchaseOptionType doc: "OFFER for a single-product offer or OFFERSET for a bundled offer set"; PurchaseOptionSummary requires purchaseOptionId, catalog, purchaseOptionType, sellerOfRecord and associatedEntities, with associatedEntities documented as "The products, offers, and offer sets associated with this purchase option". The recording for this step returns a non-empty one-element list (an OFFER summary whose associatedEntities names the product and offer and whose sellerOfRecord matches the offer set's seller of record); the specific purchaseOptionId/sellerProfileId/productId in the recording are that run's values.
  Emulator: "$.purchaseOptions: value_mismatch (expected [ {...} ], got [])" - a literally empty array, status 200 vs expected 200.
  Why: after a client has drilled into an offer set it can no longer enumerate the purchase options that offer set spans, so a bundled purchase is invisible to it; combined with Cause 1 the operation reports an empty catalog for every filtered path the corpus exercises, which also makes its @paginated contract untestable (no first page, so no nextToken).
  Fix: same component as Cause 1 - the offer-set branch of the purchase_options projection in src/state.rs (the OFFERSET entry built from offer_set.offer_ids, which skips any offer id the store does not hold) plus the filter join in src/ops/list_purchase_options.rs. The requirement is that purchase options reachable from a located offer set are returned, not that the seeded rows reproduce the recording's ids.
  Traces: marketplace-discovery-purchase-options-offerset-drilldown#task#2.

Not covered by the open marketplace-discovery issues: 20261029T201942Z (GetListing/GetProduct/ListFulfillmentOptions serve synthesized placeholder records), 20261108T055939Z (GetListing/GetProduct answer from a seeded catalog row), 20261109T122558Z (SearchListings placeholder logo host; GetListing/GetProduct detail restate a seeded row), 20261109T035256Z (SearchListings nextToken is base64 of a plaintext cursor), 20261110T144106Z (drill-down detail views synthesize a template product) and 20261110T042438Z (no IAM authorization) all concern other operations or other members; none names ListPurchaseOptions or carries these replay keys.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator (emulator_up=true, score_pct=53.6).

Cause 1 — fixture marketplace-discovery-purchase-options-filter-by-product-and-seller, step task#1, operation ListPurchaseOptions, verdict mismatch, expected_status 200, actual_status 200, affected_count 2 (also marketplace-discovery-purchase-options-offer-drilldown#task#1):
  detail: "$.purchaseOptions[0]: missing (expected {'associatedEntities': [{'offer': {'offerId': 'eyemnlrxh5or5zqsm99ih472r', 'sellerOfRecord': {'displayName': 'VMware Tanzu CloudHealth', 'sellerProfileId': '{{po_seller_id}}'}}, 'product': {'manufacturer': {...}, 'productId': 'fc9ecb41-af3a-4e4f-ae3d-36fb2f0405d3', 'productName': 'CloudHealth'}}], 'badges': [{'badgeType': 'AUTO_RENEW', 'displayName': 'Auto-renew'}], 'catalog': 'AWSMarketplace', 'purchaseOptionId': 'eyemnlrxh5or5zqsm99ih472r', 'purchaseOptionType': 'OFFER', 'sellerOfRecord': {...}}, got None)"
  Expected: >=1 PurchaseOptionSummary with purchaseOptionType OFFER and associatedEntities naming the filtered product/offer. Actual: no element at index 0 (empty list).

Cause 2 — fixture marketplace-discovery-purchase-options-offerset-drilldown, step task#2, operation ListPurchaseOptions, verdict mismatch, expected_status 200, actual_status 200:
  detail: "$.purchaseOptions: value_mismatch (expected [{'associatedEntities': [...], 'badges': [{'badgeType': 'AUTO_RENEW', ...}], 'catalog': 'AWSMarketplace', 'purchaseOptionId': 'eyemnlrxh5or5zqsm99ih472r', 'purchaseOptionType': 'OFFER', 'sellerOfRecord': {...}}], got [])"
  Expected: a one-element purchaseOptions list. Actual: [].

Neither cluster carries a stub_path (the operation IS implemented: src/ops/list_purchase_options.rs handle() filters state.purchase_options(...) and comments that "An empty page is an empty list, not an error") and neither carries an owned_by entry, so neither is covered by an existing issue.

Model evidence: smithy_query(query="operation", operation="ListPurchaseOptions") - output {purchaseOptions: PurchaseOptionSummaryList (@length max 100), nextToken}, http POST /2026-02-05/listPurchaseOptions code 200, paginated{items: purchaseOptions}; smithy_query(query="shape", shape="PurchaseOptionSummary") - purchaseOptionId, catalog, purchaseOptionType, sellerOfRecord, associatedEntities all @required; smithy_query(query="shape", shape="PurchaseOptionFilter") - filterValues doc for PRODUCT_ID / SELLER_OF_RECORD_PROFILE_ID / PURCHASE_OPTION_TYPE / VISIBILITY_SCOPE / AVAILABILITY_STATUS.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it (marketplace-discovery is bootstrapped; pending is empty).
2. Replay the wire corpus simparity-wire-aws-v1.
3. Cause 1: replay fixture marketplace-discovery-purchase-options-filter-by-product-and-seller, step task#1 — a ListPurchaseOptions POST to /2026-02-05/listPurchaseOptions carrying a PRODUCT_ID filter and a SELLER_OF_RECORD_PROFILE_ID filter for a product/seller the preceding discovery steps located. Observe HTTP 200 with purchaseOptions empty ($.purchaseOptions[0] missing) instead of at least one PurchaseOptionSummary with purchaseOptionType OFFER whose associatedEntities names that product and offer. The same request pattern reproduces on marketplace-discovery-purchase-options-offer-drilldown#task#1.
4. Cause 2: replay fixture marketplace-discovery-purchase-options-offerset-drilldown, step task#2 — the ListPurchaseOptions call made after drilling into an offer set. Observe HTTP 200 with $.purchaseOptions == [] instead of the offer-set's purchase option summary.
5. Both are 200-vs-200 body divergences, so assert on the purchaseOptions list being non-empty and on the required members of each summary (purchaseOptionId, catalog, purchaseOptionType, sellerOfRecord, associatedEntities), not on the recording's concrete ids, names or timestamps.

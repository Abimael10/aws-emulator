Issue-Source: trace-sweep-divergence
Issue-Trace: marketplace-discovery-search-listings-and-drill-into-listing
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: marketplace-discovery-search-listings-and-drill-into-listing
Issue-Shape-Id: com.amazonaws.marketplacediscovery#AWSMarketplaceDiscovery
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.associatedEntities[*]
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.categories[*].displayName
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.highlights[*]
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.listingName
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.logoThumbnailUrl
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.longDescription
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.promotionalMedia[*]
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.publisher.displayName
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.publisher.sellerProfileId
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.purchaseIndicators
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.resources[*].contentType
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.resources[*].displayName
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.resources[*].resourceType
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.resources[*].value
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.sellerEngagements[*]
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.shortDescription
Issue-Key: marketplace-discovery|GetListing|2xx->2xx|$.useCases[*]
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.categories[*]
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.highlights[*]
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.logoThumbnailUrl
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.longDescription
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.manufacturer.displayName
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.manufacturer.sellerProfileId
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.productName
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.purchaseIndicators
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.resources[*].contentType
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.resources[*].displayName
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.resources[*].resourceType
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.resources[*].value
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.sellerEngagements[*]
Issue-Key: marketplace-discovery|GetProduct|2xx->2xx|$.shortDescription
Issue-Key: marketplace-discovery|ListFulfillmentOptions|2xx->2xx|$.fulfillmentOptions[*]
Issue-Related: 20261119T070657Z-marketplace-discovery-listfulfillmentoptions-announces-a-saas-fulfillmenturl-on-an-address.md
Issue-Commit: 638af639c204b2c523dfe8732e7714679b6875fc

# Listing, product and fulfillment detail reads restate generated seed data instead of the requested record

> _Note: this issue was identified against the code base at commit `638af639c204b2c523dfe8732e7714679b6875fc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetListing

## Description
This issue bundles 3 causes, all in marketplace-discovery, all in the same drill-down flow (SearchListings -> GetListing -> GetProduct -> ListFulfillmentOptions). Each cause is one replay cluster and each needs its own re-replay to be confirmed closed.

NOTE ON OVERLAP - read before closing: two open issues describe parts of this flow, 20261109T122558Z-marketplace-discovery-getlisting-and-getproduct-detail-responses-restate-a-seeded.md (GetListing/GetProduct content) and 20261119T070657Z-marketplace-discovery-listfulfillmentoptions-announces-a-saas-fulfillmenturl-on-an-address.md (the announced SaaS URL). The current replay reports ALL THREE clusters below with owned_by EMPTY - no filed issue carries their replay key - and the emulator's behavior has drifted since those issues were written (category identifiers are now opaque uuid-shaped values rather than slugs, and the logo / fulfillment URLs are now announced on a loopback authority rather than on example.com). Neither of those issues is a substitute for closing the causes below; treat this issue as the one carrying the replay keys for these three steps.

Cause 1 of 3: GetListing answers 200 for the requested listingId but every content member describes a generated seed row rather than the record that id names.
AWS: Smithy GetListing (POST /2026-02-05/getListing, success http 200; input listingId ListingId @required @length 1..255 @pattern ^[\w\-]+$) returns associatedEntities (ListingAssociatedEntityList @required @length ..100, pairs of ProductInformation and OfferInformation), badges @required, catalog @required, categories (CategoryList @required @length ..30), fulfillmentOptionSummaries @required, highlights (@required @length ..10), listingName (@required, non-blank), logoThumbnailUrl (URL @required @length 1..2048 @pattern ^https?://.*$), longDescription (@required @length ..32768), pricingModels, pricingUnits, promotionalMedia, publisher (SellerInformation), resources, reviewSummary (optional), sellerEngagements, shortDescription, useCases; errors ResourceNotFoundException 404 / AccessDeniedException 403 / ValidationException 400 / ThrottlingException 429 / InternalServerException 500. The model does not fix the CONTENT, so for content this is recorded behavior only: the response restates the listing the id names - its own listingName, short and long description, its seller's own highlight sentences, its publisher, its taxonomy categories (categoryId an opaque taxonomy identifier, displayName the human-readable category name, in the listing's stored order), its resources and seller engagements, and an associatedEntities[0] naming that listing's real product AND its real offer with their real seller of record.
Emulator: 200, but the body describes a different, generated record. $.associatedEntities[0] resolves the SAME productId to a different product and offer (productName and offerName generated from the seed row's name, manufacturer/sellerOfRecord a template seller profile). $.listingName, $.shortDescription, $.longDescription, $.highlights[0..2], $.useCases[0], $.resources[0..3] (contentType, displayName, resourceType, value), $.sellerEngagements[0..1], $.publisher.sellerProfileId/.displayName, $.purchaseIndicators and $.promotionalMedia[0] are all seeded text for an unrelated row, and $.categories[0..3].displayName name a different taxonomy than the record's. $.logoThumbnailUrl is additionally announced on an emulator-local loopback authority that is not the authority the request reached the emulator on, so the one address in the response a client is meant to dereference identifies no resource.
Why: GetListing is the detail view the whole discovery flow funnels into, so a client that looked a listing up by id renders someone else's record - different name, different publisher, different categories and a different associated offer - while the status says the lookup succeeded. A client that feeds a returned categoryId back as a CATEGORY search filter filters on a taxonomy value the listing is not in, and the logo address it renders is dead.
Fix: the seeded catalog and the projections GetListing restates it through, in the marketplace-discovery crate's state (services/service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery/src/state.rs): the per-row seeder, the listing projection that fills name/descriptions/highlights/resources/seller engagements/promotional media from that row, the associatedEntities resolution for the requested product, and the announced-address resolution behind logoThumbnailUrl. The handler must restate the stored record for the requested listingId, and must announce fetchable addresses on the endpoint the answering request arrived on.
Traces: marketplace-discovery-search-listings-and-drill-into-listing#task#2 (GetListing, expected 200 / got 200, 30 diverging keys).

Cause 2 of 3: GetProduct repeats the substitution for the product resolved from that listing.
AWS: Smithy GetProduct (POST /2026-02-05/getProduct, success http 200; input productId ProductId @required @length 1..255 @pattern ^[\w\-]+$) returns productId, catalog, productName, manufacturer (SellerInformation @required), deployedOnAws (DeployedOnAwsStatus @required), shortDescription, longDescription, logoThumbnailUrl (URL @required @pattern ^https?://.*$), fulfillmentOptionSummaries, categories (@length ..30), highlights (@length ..10), promotionalMedia, resources, sellerEngagements, listingId; same five modelled errors. Recorded behavior for content: the product restates its own stored record - its own productName and manufacturer, its own short/long description, its taxonomy categories as whole elements {categoryId opaque identifier, displayName the category name} in stored order, the seller's own three highlight sentences, and its own resources and seller engagements.
Emulator: 200 with the same substitution as cause 1 - $.categories[0..3] differ whole-element (both categoryId and displayName name a different taxonomy), $.highlights[0..2] are the seed row's sentences, and $.productName, $.manufacturer.sellerProfileId/.displayName, $.shortDescription, $.longDescription, $.logoThumbnailUrl, $.resources[0..3].*, $.sellerEngagements[0..1] and $.purchaseIndicators are the same generated template text.
Why: a client that drills product detail after a listing gets a second, independently wrong copy of the same record, so cross-checking listing against product cannot detect the substitution, and a categoryId read off the product is not one the listing's own categories contain.
Fix: same component as cause 1 - the seeder and the category / highlight / description projections GetProduct shares with GetListing in that crate's state; GetProduct must restate the stored record for the requested productId.
Traces: marketplace-discovery-search-listings-and-drill-into-listing#task#3 (GetProduct, expected 200 / got 200, 27 diverging keys).

Cause 3 of 3: ListFulfillmentOptions announces the SaaS fulfillmentUrl on an authority the answering request did not arrive on.
AWS: Smithy ListFulfillmentOptions (POST /2026-02-05/listFulfillmentOptions, success http 200, @paginated items fulfillmentOptions; input productId @required) returns fulfillmentOptions (FulfillmentOptionsList @required); the SaaS union member SaasFulfillmentOption declares fulfillmentOptionId @required, fulfillmentOptionType @required, fulfillmentOptionDisplayName @required, fulfillmentUrl (String, documented "The URL of the seller's software registration landing page"), availableFromTime, launchUrl (URL @length 1..2048 @pattern ^https?://.*$) and quickLaunch @required. The model does not fix the host and on AWS these pages live on the seller's own hosts, so the requirement is not a particular host: it is that fulfillmentUrl names a page the client can actually dereference. Recorded behavior only for the host (the recording's value is a seller-hosted registration page).
Emulator: 200, and the option's fulfillmentUrl is published as https://<authority>/_aws/marketplace-discovery-saas-fulfillment/<fulfillmentOptionId>/register whose authority is NOT the one the replayed request reached the emulator at - in this run the announced authority is a loopback host:port different from the data listener that accepted the request, and the replay classifies the member `resource_not_identified`: the published address identifies no resource. The emulator does serve that path at its real front door, so the announcement, not the route, is wrong.
Why: fulfillmentUrl is the member a buyer-facing client turns into a link or a redirect after subscribing. Following the announced registration page yields a refused connection or no resource, so the subscribe-then-register flow cannot be exercised end to end even though the emulator serves that page where it is actually reached.
Fix: the announced-address resolution this operation's projection uses - the SaaS fulfillment addressing in that crate's state (src/state/saas_fulfillment.rs: saas_page_url / announced_page_base) and the endpoint/authority ordering it consults - must yield the authority and scheme the answering request arrived on, which the fulfillment-option record then stamps into fulfillmentUrl (and launchUrl).
Traces: marketplace-discovery-search-listings-and-drill-into-listing#task#4 (ListFulfillmentOptions, expected 200 / got 200; keys $.fulfillmentOptions[0] and $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl, the latter verdict `resource_not_identified`).

Explicitly NOT claimed as requirements anywhere in this issue: the recorded literal ids, names and URLs (listing / product / offer / fulfillment-option ids, sellerProfileId values, CloudFront media paths, the seller-hosted registration page) and any clock-derived timestamp such as availableFromTime. Those differ per environment and per run. The defects are that the content is a generated template rather than the record the requested id names, and that the addresses the response publishes for a client to fetch are announced on an authority the emulator is not reached on.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator up: /__admin/health answered 200 and the :46279 data listener accepts; score 53.6%, 10 of 28 steps mismatched). All three steps belong to fixture marketplace-discovery-search-listings-and-drill-into-listing, which searches (task#1) and then looks the resulting listing, product and fulfillment options up by id. All three returned 200 where 200 was expected, so each divergence is entirely in the body, and all three clusters are reported with owned_by empty.

Cause 1 - #task#2, GetListing, 30 diverging keys ($.associatedEntities[0], $.categories[0..3].displayName, $.highlights[0..2], $.listingName, $.logoThumbnailUrl, $.longDescription, $.promotionalMedia[0], $.publisher.displayName, $.publisher.sellerProfileId, $.purchaseIndicators, $.resources[0..3].*, $.sellerEngagements[0..1], $.shortDescription, $.useCases[0]). Samples, expected vs actual:
- $.associatedEntities[0]: expected {offer: {offerId, offerName 'Offer created on <timestamp>', sellerOfRecord {displayName, sellerProfileId}}, product: {manufacturer {displayName, sellerProfileId}, productId 'p-0227-structured-data-ml-service', productName 'Data'}}; got the same productId resolved to {product: {productName 'Borealis Structured Data Service', manufacturer {sellerProfileId 'sp-borealis', displayName 'Borealis Data'}}, offer: {offerId 'o-0227-structured-data-ml-service', offerName 'Borealis Structured Data Service - Contract', sellerOfRecord {sellerProfileId 'sp-borealis', displayName 'Borealis Data'}}}.
- $.listingName: expected 'Data', got 'Borealis Structured Data Service'.
- $.categories[0].displayName: expected 'Data Integration', got 'Structured Data'; [1] expected 'Data Analytics', got 'Machine Learning'; [2] expected 'Infrastructure Software', got 'Retail'; [3] expected 'Data Preparation', got 'Industries'.
- $.highlights[0]: expected 'IBM i software front end and back end modernization', got 'Trains gradient-boosted models over tabular columns'; [1] and [2] diverge the same way.
- $.longDescription, $.shortDescription, $.useCases[0], $.resources[0..3], $.sellerEngagements[0..1], $.promotionalMedia[0], $.publisher.* and $.purchaseIndicators all carry the seed row's text; $.logoThumbnailUrl is an emulator-local https://127.0.0.1:<port>/_aws/marketplace-discovery-listing-media/<id>/logo.png on an authority other than the serving one.

Cause 2 - #task#3, GetProduct, 27 diverging keys. Samples:
- $.categories[0]: expected {categoryId 'f42ec728-09b3-46d2-956a-8a0cde3f8ee6', displayName 'Data Integration'}, got {categoryId '645edc21-2120-59fd-a2c4-451dfca0161e', displayName 'Structured Data'}; [1] expected 'Data Analytics', got 'Machine Learning'; [2] expected 'Infrastructure Software', got 'Retail'; [3] expected 'Data Preparation', got 'Industries' - whole-element divergences (the identifier is now uuid-shaped, which is right in FORM; the element it names is the wrong category).
- $.highlights[0..2]: expected the seller's three highlight sentences, got the seed row's three ('Trains gradient-boosted models over tabular columns', 'Feature importance is reported beside every prediction', 'Model drift is flagged against a held-out sample').
- $.productName, $.manufacturer.displayName/.sellerProfileId, $.shortDescription, $.longDescription, $.logoThumbnailUrl, $.resources[0..3].*, $.sellerEngagements[0..1], $.purchaseIndicators: all template text for the unrelated row.

Cause 3 - #task#4, ListFulfillmentOptions, keys $.fulfillmentOptions[0] and $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl:
- $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl: verdict `resource_not_identified`. Expected a seller-hosted registration page; got 'https://127.0.0.1:35899/_aws/marketplace-discovery-saas-fulfillment/fo-0227-structured-data-ml-service-saas/register', whose authority is not the authority this request reached the emulator at (the run's data listener is :46279). The part that IS a requirement is that the announced authority and scheme are the ones the answering request arrived on; the path /_aws/marketplace-discovery-saas-fulfillment/<fulfillmentOptionId>/register is the emulator's own and correct.
- $.fulfillmentOptions[0]: reported as differing as a whole for the same element; its fulfillmentOptionId and the numeric availableFromTime are per-environment / per-run values and are NOT asserted here.

Code read: services/service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery/src/state.rs - the module header at :3-29 states the service owns a seeded catalog built on Default and that every handler computes its response from it; the breadth seed rows at :4040-4339 are the source of the 'Borealis'/'Aurora'/'Cirrus' names, highlights and descriptions seen above, BREADTH_SELLERS at :4342-4348 of the 'sp-borealis' publisher, and the per-row seeder at :4810-4884 of the o-/os- offers these details restate; the announced addresses come from src/state/saas_fulfillment.rs (saas_page_url / announced_page_base) and src/state/listing_logo.rs (logo_url).

Model reference: smithy_query query="operation" operation="GetListing,GetProduct,ListFulfillmentOptions" and query="shape" shape="SaasFulfillmentOption" on services/service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery/smithy-model/com-amazonaws-marketplacediscovery-awsmarketplacediscovery.json.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout; note the authority (host:port) its data listener accepts requests on.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Run fixture marketplace-discovery-search-listings-and-drill-into-listing. Step task#1 POSTs /2026-02-05/searchListings and yields a listing id and a product id.
4. Cause 1: step task#2 POSTs /2026-02-05/getListing with that listingId. Observe 200 whose body describes a generated seed row instead of the listing - a different listingName, publisher, short/long description, highlights, resources, seller engagements, promotional media and use cases, categories whose displayName values name a different taxonomy, associatedEntities[0] resolving the same productId to a different product and offer, and a logoThumbnailUrl announced on a loopback authority that is not the one used in step 3.
5. Cause 2: step task#3 POSTs /2026-02-05/getProduct with the productId from that listing. Observe 200 with the same substituted categories (whole-element), the seed row's highlights, and template productName/manufacturer/descriptions/logoThumbnailUrl/resources/sellerEngagements.
6. Cause 3: step task#4 POSTs /2026-02-05/listFulfillmentOptions with that productId. Read $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl: its authority is not the authority the request was sent to. GET the announced URL - it does not resolve; GET the same path on the authority the API was reached at and the registration page is served, showing the announcement, not the route, is wrong.
7. Confirm the fix by re-replaying all three bound steps: marketplace-discovery-search-listings-and-drill-into-listing#task#2, #task#3 and #task#4.

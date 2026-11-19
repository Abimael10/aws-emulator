Issue-Source: trace-sweep-divergence
Issue-Trace: marketplace-discovery-search-listings-and-drill-into-listing
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: marketplace-discovery-search-listings-and-drill-into-listing
Issue-Shape-Id: com.amazonaws.marketplacediscovery#AWSMarketplaceDiscovery
Issue-Key: marketplace-discovery|ListFulfillmentOptions|2xx->2xx|$.fulfillmentOptions[*].saasFulfillmentOption.fulfillmentUrl
Issue-Related: 20261029T201942Z-marketplace-discovery-getlisting-getproduct-and-listfulfillmentoptions-serve-synthesized-placeholder-records.md
Issue-Commit: 09c23e7edd3271306ce8757d61ba011150226101

# ListFulfillmentOptions announces a SaaS fulfillmentUrl on an address the emulator is not reached on

> _Note: this issue was identified against the code base at commit `09c23e7edd3271306ce8757d61ba011150226101`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListFulfillmentOptions

## Description
One cause, in marketplace-discovery.

Cause 1 of 1: the SaaS fulfillment option's `fulfillmentUrl` is published on an authority that is not the one the request reached the emulator at, so the address the emulator announces is an address nothing answers at.

AWS: Smithy: ListFulfillmentOptions (POST /2026-02-05/listFulfillmentOptions, http code 200) returns `fulfillmentOptions` (FulfillmentOptionsList, required); the SaaS union member SaasFulfillmentOption declares `fulfillmentUrl` (String, optional) documented as "The URL of the seller's software registration landing page", alongside `launchUrl` (URL, @length 1..2048, @pattern ^https?://.*$) documented as "The URL that a buyer uses to launch the seller's SaaS product. This URL is distinct from fulfillmentUrl". The model does not fix the host, and on AWS these pages live on the seller's own hosts, so the WORDING of this requirement is not a particular host: it is that `fulfillmentUrl` names a page a buyer can actually dereference. Recorded behavior only for the host itself (the recording's value is a seller-hosted page), so the recorded host, the generated `fulfillmentOptionId`, and the numeric `availableFromTime` are explicitly NOT claimed as requirements here — they differ per environment and per run.

Emulator: 200, and the option's `fulfillmentUrl` is announced as an emulator-local URL of the form `https://<authority>/_aws/marketplace-discovery-saas-fulfillment/<fulfillmentOptionId>/register` whose authority is NOT the one the replayed request reached the emulator at. In this replay the emulator's data listener is the one that accepts the ListFulfillmentOptions request, yet the announced authority is a different loopback port, and the replay checker classifies the member `resource_not_identified`: the published address identifies no resource. The emulator's own design states this is wrong — src/state/saas_fulfillment.rs documents that "what it publishes is an address of its OWN: the endpoint a request reached it at ... A URL published on any other host is one the client can render but never dereference" — so the defect is that the announced authority drifts from the serving one, not that it is local.

Hypothesis: `announced_page_base` resolves the authority through `reached_emulator_endpoint_of_parts(ctx.parts())` else `served_endpoint_authority(base)` else `url_authority(base)`, and hard-codes the `https://` scheme; when the request's reached-endpoint parts do not resolve, the fallback supplies a port the process is not serving the data plane on (and/or a scheme that listener does not present), which is exactly the "any other host" case the module warns about.

Why: `fulfillmentUrl` is the one member a buyer-facing client turns into a link or a redirect after a subscribe. A client that follows the announced registration page gets a connection that is refused or a resource that does not exist, so the subscribe-then-register flow cannot be exercised end to end against the emulator even though the emulator does serve that page at its real front door. It also makes the emulator's own round trip inconsistent: the route that answers `/_aws/marketplace-discovery-saas-fulfillment/<id>/register` can never be reached through the URL the API handed out.

Fix: the announced-address resolution this operation's projection uses — `announced_page_base` in the service's SaaS fulfillment addressing, and the endpoint/authority ordering it consults in the shared data-plane addressing module — must yield the authority and scheme the answering request actually arrived on, which `FulfillmentOptionRecord::announced` then stamps into `fulfillmentUrl`/`launchUrl`.

Related but NOT covering this: open issue 20261029T201942Z-marketplace-discovery-getlisting-getproduct-and-listfulfillmentoptions-serve-synthesized-placeholder-records.md covers the same step's SEEDED content (quickLaunch, the optional usageInstructions/launchUrl members, the absent recorded `deploymentTemplates` member, and the then-template `https://example.com/<slug>/register` URL). It does not carry this replay key and says nothing about the announced authority diverging from the serving one, which is what the current replay flags.

Traces: marketplace-discovery-search-listings-and-drill-into-listing#task#4 (ListFulfillmentOptions, expected 200 / got 200; keys $.fulfillmentOptions[0] and $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl, the latter verdict `resource_not_identified`). Code read: src/state/saas_fulfillment.rs:106-134 (saas_page_url / announced_page_base) and src/state.rs:343-365 (FulfillmentOptionRecord::announced), in services/service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator up: /__admin/health answered 200 and the :37623 data listener accepts).

Fixture marketplace-discovery-search-listings-and-drill-into-listing, step task#4, ListFulfillmentOptions: expected 200, got 200.

Diverging keys reported by the replay:
- $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl: verdict `resource_not_identified` — the emulator answered with `https://127.0.0.1:39027/_aws/marketplace-discovery-saas-fulfillment/fo-0227-structured-data-ml-service-saas/register`, i.e. authority 127.0.0.1:39027, while the request that produced this response reached the emulator on the :37623 data listener. The fixed part of the announced address that IS a requirement is that its authority and scheme are the ones the answering request arrived on; the path `/_aws/marketplace-discovery-saas-fulfillment/<fulfillmentOptionId>/register` is the emulator's own and correct. The recorded value (a seller-hosted page) and the recorded/emulated `fulfillmentOptionId` and `availableFromTime` are per-environment/per-run values and are not asserted.
- $.fulfillmentOptions[0]: reported as differing as a whole for the same element.

The emulator does serve that page: src/state/saas_fulfillment.rs mints the path (saas_page_path), routes it back (saas_page_route) and renders it from the stored record (saas_fulfillment_page) — so the only thing standing between a client and that page is the authority stamped into the announced URL.

Model reference: smithy_query query="operation" operation="ListFulfillmentOptions" and query="shape" shape="SaasFulfillmentOption,URL" on services/service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery/smithy-model/com-amazonaws-marketplacediscovery-awsmarketplacediscovery.json.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout; note the port its data listener accepts requests on.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Run fixture marketplace-discovery-search-listings-and-drill-into-listing: step task#1 searches, task#2 gets the listing, task#3 gets the product id.
4. Step task#4 POSTs /2026-02-05/listFulfillmentOptions with that productId to the data listener.
5. Read $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl: its authority is not the authority the request in step 4 was sent to.
6. GET that announced URL: it does not resolve to the registration page. GET the same path on the authority the API was actually reached at and the page is served — showing the announcement, not the route, is wrong.

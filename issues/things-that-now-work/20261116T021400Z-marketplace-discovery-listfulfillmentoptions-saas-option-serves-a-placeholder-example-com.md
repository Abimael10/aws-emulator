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
Issue-Commit: 6657c571a48698e48ad30cd78f16738e8c5e885f

# ListFulfillmentOptions SaaS option serves a placeholder example.com fulfillmentUrl and a synthesized usageInstructions

> _Note: this issue was identified against the code base at commit `6657c571a48698e48ad30cd78f16738e8c5e885f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListFulfillmentOptions

## Description
This issue carries 1 cause, in marketplace-discovery.

Cause 1 of 1: ListFulfillmentOptions answers 200 but the SaaS fulfillment option it returns is a template built from the product's slug rather than a restatement of the stored fulfillment-option record: its fulfillmentUrl names a host the emulator does not serve, and it carries an optional member (usageInstructions) the record does not have while omitting a member the recording does carry.

AWS: Smithy: ListFulfillmentOptions (POST /2026-02-05/listFulfillmentOptions, output member fulfillmentOptions: FulfillmentOptionsList, required; errors ResourceNotFoundException 404, ValidationException 400, AccessDeniedException 403, ThrottlingException 429, InternalServerException 500). The FulfillmentOption union's saasFulfillmentOption member targets SaasFulfillmentOption, which REQUIRES fulfillmentOptionId, fulfillmentOptionType, fulfillmentOptionDisplayName and quickLaunch, and makes fulfillmentUrl, usageInstructions, availableFromTime and launchUrl OPTIONAL. fulfillmentUrl is documented as "The URL of the seller's software registration landing page" and usageInstructions as "Instructions on how to access and use this SaaS product" — both are properties of the seller's record, not values the service composes. The model does not fix the content, so for the content: recorded behavior only — the recorded SaaS option reports a real, resolvable seller-owned registration URL for fulfillmentUrl, emits NO usageInstructions at all when the record carries none, and carries a deploymentTemplates member (an empty list here) that the on-disk SaasFulfillmentOption does not declare, so deploymentTemplates is recorded behavior only and a fixer should treat only the modelled members above as a requirement. Status for this call is 200 in both the recording and the model's http binding, and no error code is involved.

Emulator: 200, and the SaaS option's structural members (fulfillmentOptionType SAAS, fulfillmentOptionDisplayName "SaaS", quickLaunch DISABLED) agree — but (a) fulfillmentUrl is a composed placeholder of the form https://example.com/<product-slug>/register, which the replay flags host_not_own: example.com is not a host this emulator serves or owns, so the URL cannot be dereferenced by anything that receives it; (b) usageInstructions is present and is a generated sentence of the form "Subscribe, then open the <product display name> console.", where the record (and the recording) carries no usageInstructions member at all; (c) deploymentTemplates is absent where the recording emits it.

Why: fulfillmentUrl is the value a buyer-facing client turns into a link or follows to register for the SaaS product. A client that renders or follows it gets an address the emulator never answers on, so the post-subscribe registration leg of the flow cannot be exercised at all, and a test asserting the host is reachable fails for a reason that has nothing to do with the client. The spurious usageInstructions is worse than missing text: presence of an optional member is what a client branches on to decide whether to show a usage panel, so the emulator makes clients take a code path AWS would not take for this product, and a client that asserts the instructions came from the seller's record sees emulator-authored prose instead.

Fix: ListFulfillmentOptions' projection of the seeded fulfillment options — specifically the SAAS arm of the breadth-row fulfillment-option builder in the marketplace-discovery crate's state, which composes both the registration URL and the usage-instructions sentence from the row's slug and display name. The SaaS option must take fulfillmentUrl from the stored record (and, if the emulator must serve one, from an address it actually answers on rather than example.com), and must emit usageInstructions only when the record carries one. Note the already-open issue 20261029T201942Z-marketplace-discovery-getlisting-getproduct-and-listfulfillmentoptions-serve-synthesized-placeholder-records.md describes this same operation and lists an Issue-Key for $.fulfillmentOptions[*], but its replay pointer (Issue-Trace/Issue-Step marketplace-discovery-search-listings-and-drill-into-listing#task#2, Issue-Trace-Ids naming the fixture with no step) does not bind this step, which is why replay reports this cluster as unowned: the quickLaunch ENABLED and launchUrl halves of that issue's cause 3 are gone, but the placeholder fulfillmentUrl and the synthesized usageInstructions remain and are what this issue gates.

Traces: marketplace-discovery-search-listings-and-drill-into-listing#task#4 (ListFulfillmentOptions, expected 200 / got 200, diverging keys $.fulfillmentOptions[0] and $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl, the latter classified host_not_own). File/line citation: services/service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery/src/state.rs:3650-3670 (fulfillment_option_for_type, the "SAAS" arm: saas_option(id, &format!("https://example.com/{slug}/register"), None, "DISABLED", &format!("Subscribe, then open the {name} console."), available_from)).

Explicitly NOT claimed as requirements: the generated fulfillmentOptionId literal (fo-<slug>-saas vs the recording's fo-<base32>) and the numeric availableFromTime, which the emulator derives from its own clock — those are per-environment, per-run values. The defects claimed are the unservable placeholder HOST in fulfillmentUrl, the presence of an optional member the record does not carry, and the content being composed from a template rather than restated from the record.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, /__admin/health 200). Fixture marketplace-discovery-search-listings-and-drill-into-listing, step task#4, operation ListFulfillmentOptions: expected status 200, actual status 200, verdict mismatch, owned_by empty and stub_path empty (net-new, not covered by any open issue's replay key).

Diverging keys reported: $.fulfillmentOptions[0] and $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl (host_not_own).

Expected $.fulfillmentOptions[0]:
{'saasFulfillmentOption': {'availableFromTime': 1719446400.0, 'deploymentTemplates': [], 'fulfillmentOptionDisplayName': 'SaaS', 'fulfillmentOptionId': 'fo-nmr4osr2damuo', 'fulfillmentOptionType': 'SAAS', 'fulfillmentUrl': <seller-owned registration URL>, 'quickLaunch': 'DISABLED'}}

Actual $.fulfillmentOptions[0]:
{'saasFulfillmentOption': {'fulfillmentOptionId': 'fo-0227-structured-data-ml-service-saas', 'fulfillmentOptionType': 'SAAS', 'fulfillmentOptionDisplayName': 'SaaS', 'fulfillmentUrl': 'https://example.com/structured-data-ml-service/register', 'usageInstructions': 'Subscribe, then open the Borealis Structured Data Service console.', 'availableFromTime': 1792203148, 'quickLaunch': 'DISABLED'}}

Reading the diff member by member: fulfillmentOptionType, fulfillmentOptionDisplayName and quickLaunch agree. fulfillmentUrl diverges in its HOST — example.com, which the replay classifies host_not_own because the emulator does not serve it; the fixed part of the emulator's value is the literal 'https://example.com/' prefix and the '/register' suffix, and the variable part is the product slug taken from the seeded row. usageInstructions is EXTRA: its fixed text is 'Subscribe, then open the ' / ' console.' and its variable part is the product display name, so the whole member is emulator-composed and the recording has no such member. deploymentTemplates is ABSENT where the recording emits [] (recorded behavior only — the on-disk SaasFulfillmentOption does not declare it). fulfillmentOptionId and availableFromTime differ only as per-environment generated values and are not claimed.

Model reference: smithy_query query=operation operation=ListFulfillmentOptions and query=shape shape=SaasFulfillmentOption,FulfillmentOption on services/service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery/smithy-model/com-amazonaws-marketplacediscovery-awsmarketplacediscovery.json.

Code read: services/service-com-amazonaws-marketplacediscovery-awsmarketplacediscovery/src/state.rs:3650-3670 — fulfillment_option_for_type's "SAAS" arm formats both the example.com registration URL and the usage-instructions sentence.

## Steps to Reproduce
1. git clone the repo (branch under test) and build/boot the emulator from that checkout.
2. Replay the wire corpus for trace set simparity-wire-aws-v1.
3. Run fixture marketplace-discovery-search-listings-and-drill-into-listing. Step task#1 (SearchListings) yields a listing id; step task#2 (GetListing) yields the product id; step task#3 (GetProduct) confirms it.
4. Step task#4 POSTs /2026-02-05/listFulfillmentOptions with that productId (p-0227-structured-data-ml-service).
5. Observe the 200 response: $.fulfillmentOptions[0].saasFulfillmentOption.fulfillmentUrl is 'https://example.com/<slug>/register' — an example.com host the emulator does not serve (replay verdict host_not_own) — and the option additionally carries a usageInstructions member ('Subscribe, then open the <name> console.') that the recorded record does not have, while the recording's deploymentTemplates member is absent.
6. Compare against the recorded step: the SaaS option's fulfillmentUrl is a resolvable seller-owned registration URL with no usageInstructions member present.

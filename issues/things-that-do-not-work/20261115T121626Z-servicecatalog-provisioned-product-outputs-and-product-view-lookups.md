Issue-Source: trace-sweep-divergence
Issue-Trace: servicecatalog-provision-product-returns-template-outputs
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: servicecatalog-provision-product-returns-template-outputs servicecatalog-product-view-and-copy-product servicecatalog-provisioned-product-plan-and-record-not-found
Issue-Shape-Id: com.amazonaws.servicecatalog#AWS242ServiceCatalogService
Issue-Key: servicecatalog|GetProvisionedProductOutputs|2xx->4xx|$.Outputs
Issue-Key: servicecatalog|GetProvisionedProductOutputs|2xx->4xx|status
Issue-Key: servicecatalog|DescribeProductView|4xx->4xx|$.Message
Issue-Key: servicecatalog|GetProvisionedProductOutputs|4xx->4xx|$.Message
Issue-Commit: f4e3930dabb7420d5dbe8abed2b59ac3a4a8acd2

# servicecatalog: provisioned-product outputs and product-view lookups diverge from AWS status and messages

> _Note: this issue was identified against the code base at commit `f4e3930dabb7420d5dbe8abed2b59ac3a4a8acd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetProvisionedProductOutputs

## Description
Three distinct Service Catalog read-path defects, all in the provisioned-product / product-view lookup path: a successful launch's template outputs are not retrievable at all, and two not-found errors are returned with emulator-invented message text instead of AWS's wording. This bundle has 3 causes; all three must be closed.

Cause 1 of 3: GetProvisionedProductOutputs returns 400 for a provisioned product that exists and has template outputs (fixture servicecatalog-provision-product-returns-template-outputs, step task#11).
AWS: Smithy: GetProvisionedProductOutputs input accepts ProvisionedProductId (Id, pattern ^[a-zA-Z0-9_\-]*$) or ProvisionedProductName; output GetProvisionedProductOutputsOutput { Outputs: RecordOutputs, NextPageToken }; modeled errors InvalidParametersException (400) and ResourceNotFoundException (400). For a provisioned product that has settled successfully from a template declaring Outputs, AWS answers HTTP 200 and returns one RecordOutput per template output, each carrying OutputKey, OutputValue and Description taken from the launched template and the resolved parameter values (the recording shows the template's output echoing the launch's Env parameter). ResourceNotFoundException is reserved for an identifier that names no provisioned product.
Emulator: answers HTTP 400 with no Outputs member at all for a provisioned product the same fixture had just provisioned successfully, i.e. it treats a live provisioned product as absent (or never materialises its stack outputs).
Why: a client that provisions a product and then reads its outputs to obtain the launched resources' identifiers gets a hard client error instead of the output set, so every post-launch wiring step (looking up a created queue/bucket/role name) fails. The error is also mis-classified: a 400 not-found tells the client to stop retrying and that its identifier is wrong, when the identifier is correct.
Fix: the GetProvisionedProductOutputs handler and the provisioned-product state it reads — the launch must record the resolved template Outputs on the provisioned product (ProvisionProduct's state transition) and the handler must resolve a live provisioned product by id/name and serialize them into Outputs; the modeled 400 path must only fire for an unknown identifier.
Traces: servicecatalog-provision-product-returns-template-outputs#task#11 (expected 200 with Outputs, got 400 with none).

Cause 2 of 3: DescribeProductView not-found message uses emulator wording (fixture servicecatalog-product-view-and-copy-product, step task#7).
AWS: Smithy: DescribeProductView.Id is required (length 1-100, pattern ^[a-zA-Z0-9_\-]*$); modeled errors InvalidParametersException (400) and ResourceNotFoundException (400). For an Id that names no product view AWS returns HTTP 400 with the recorded error code and the message whose fixed text is "Product not found for ProductViewId: " followed by the Id value taken verbatim from the request. Only that trailing identifier is variable; the prefix is fixed text. (Recorded behavior supplies the exact wording; the model documents only the error class.)
Emulator: returns HTTP 400 with the correct error code but the message "<id> does not exist or access was denied" — AWS's wording is not produced, and the invented form also leads with the identifier rather than the fixed prefix.
Why: clients and SDK-based tooling match on Service Catalog's message text to distinguish a genuinely missing product view from an authorization failure; the emulator's wording asserts an access-denial that did not happen and breaks any assertion or log-scrape keyed on AWS's text.
Fix: the DescribeProductView handler's not-found error construction (message template), not its status or error code, which already match. Note the separately open issue 20261104T032856Z-servicecatalog-describeproduct-not-found-access-denied-error-message-uses.md covers the same wording defect in DescribeProduct; it does not cover DescribeProductView, which has its own message template.
Traces: servicecatalog-product-view-and-copy-product#task#7.

Cause 3 of 3: GetProvisionedProductOutputs not-found message uses emulator wording (fixture servicecatalog-provisioned-product-plan-and-record-not-found, step task#6).
AWS: Smithy: GetProvisionedProductOutputs modeled errors InvalidParametersException (400), ResourceNotFoundException (400). For a provisioned-product identifier that names nothing, AWS surfaces the underlying CloudFormation stack lookup failure: HTTP 400 with the recorded error code and a message whose fixed text is "No stack named " + <the provisioned-product identifier from the request> + " exists." — the identifier is the only variable part; the prefix, the suffix " exists." and the trailing period are fixed text. (Recorded behavior only; the model documents the error class, not the string.)
Emulator: returns HTTP 400 with the correct error code but the message "Provisioned product not found: <id>", which is neither AWS's wording nor AWS's shape (it never mentions the stack and drops the terminating period).
Why: a client cannot distinguish AWS's stack-level not-found from an emulator-specific string, so retry/branching logic and test assertions keyed on AWS's message fail even though the status and code look right.
Fix: the GetProvisionedProductOutputs handler's not-found error construction (message template). This is the same class of defect as the already-open 20261113T121347Z-servicecatalog-describeprovisionedproductplan-not-found-message-uses-emulator-wording-instead.md (DescribeProvisionedProductPlan), which owns only that operation's template and does not cover this one.
Traces: servicecatalog-provisioned-product-plan-and-record-not-found#task#6.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 128 steps, 106 passed, score 82.8%).

Cause 1 — fixture servicecatalog-provision-product-returns-template-outputs, step task#11, operation GetProvisionedProductOutputs, verdict mismatch:
  expected status 200, actual status 400
  $.Outputs: missing — expected a RecordOutputs list (one entry per template output, carrying Description/OutputKey/OutputValue; the recording's entry echoes the launch's Env parameter), got None.

Cause 2 — fixture servicecatalog-product-view-and-copy-product, step task#7, operation DescribeProductView, verdict mismatch (status 400 expected and returned; error code matched):
  $.Message value_mismatch — expected "Product not found for ProductViewId: <requested Id>", got "<requested Id> does not exist or access was denied".

Cause 3 — fixture servicecatalog-provisioned-product-plan-and-record-not-found, step task#6, operation GetProvisionedProductOutputs, verdict mismatch (status 400 expected and returned; error code matched):
  $.Message value_mismatch — expected "No stack named <requested provisioned-product id> exists.", got "Provisioned product not found: <requested provisioned-product id>".

Smithy model consulted: services/service-com-amazonaws-servicecatalog-aws242servicecatalogservice — DescribeProductView (Id required, errors InvalidParametersException/ResourceNotFoundException, both 400) and GetProvisionedProductOutputs (output Outputs: RecordOutputs + NextPageToken, same two 400 errors, paginated by PageToken/PageSize).

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
3. Cause 1: replay fixture servicecatalog-provision-product-returns-template-outputs through step task#11 — create a product whose template declares an output, provision it with a parameter, wait for the launch to settle, then call GetProvisionedProductOutputs on the resulting provisioned product. Observe HTTP 400 and no Outputs where AWS returns 200 with the template's outputs.
4. Cause 2: replay fixture servicecatalog-product-view-and-copy-product step task#7 — call DescribeProductView with an Id that names no product view (e.g. a well-formed but unknown prod-* id). Observe the 400 body's Message is the emulator's "<id> does not exist or access was denied" instead of "Product not found for ProductViewId: <id>".
5. Cause 3: replay fixture servicecatalog-provisioned-product-plan-and-record-not-found step task#6 — call GetProvisionedProductOutputs with a provisioned-product id that names nothing (e.g. an unknown pp-* id). Observe the 400 body's Message is "Provisioned product not found: <id>" instead of "No stack named <id> exists.".

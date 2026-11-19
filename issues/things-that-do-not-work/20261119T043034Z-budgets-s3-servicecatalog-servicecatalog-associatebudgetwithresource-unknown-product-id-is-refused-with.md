Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: budgets__s3__servicecatalog.associate-budget-with-unknown-product-rejected
Issue-Key: budgets__s3__servicecatalog.associate-budget-with-unknown-product-rejected|run#26|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Commit: 94d2aaccc78d8a13524b41a95baab02d39e9af8f

# servicecatalog AssociateBudgetWithResource: unknown product id is refused with a generic 'Resource not found: <id>' message instead of AWS's 'Product <id> not found.'

> _Note: this issue was identified against the code base at commit `94d2aaccc78d8a13524b41a95baab02d39e9af8f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateBudgetWithResource

## Description
One net-new root cause in the emulated AWS Service Catalog service, reached through the budgets/servicecatalog association flow.

Cause 1 of 1: AssociateBudgetWithResource's ResourceNotFoundException message is a generic, emulator-invented string rather than AWS's product-specific text.

AWS: Smithy (services/service-com-amazonaws-servicecatalog-aws242servicecatalogservice/smithy-model/com-amazonaws-servicecatalog-aws242servicecatalogservice.json): AssociateBudgetWithResource input BudgetName (required, @length 1..100) and ResourceId (target Id, required, @length 1..100, @pattern ^[a-zA-Z0-9_\-]*$, documented as "Either a portfolio-id or a product-id"); modeled errors DuplicateResourceException | InvalidParametersException | LimitExceededException | ResourceNotFoundException, all errorKind client, httpError 400. ResourceNotFoundException carries a single Message member (ErrorMessage). For a ResourceId that names no product, AWS refuses the call with ResourceNotFoundException whose message template is the fixed text "Product " + <the ResourceId from the request> + " not found." — the fixed parts are "Product " and " not found."; the only variable part is the ResourceId echoed from the request (the recorded run happened to send a prod-sim2real-missing-... id; that literal fills the template, it is not part of it).

Emulator: the status (400) and the error code (ResourceNotFoundException) are right, but the body carries Message 'Resource not found: <ResourceId>' — a generic kind-agnostic string built by crate::state::not_found_message(crate::state::KIND_RESOURCE, &input.resource_id), so the message never names the resource kind AWS names.

Why: a client that distinguishes "the id names no product" from other not-found conditions by the message content, logs it, or surfaces it to a user, sees text AWS never emits; recorded-expectation tests and anything matching AWS's "Product ... not found." phrasing fail even though the request was correctly refused.

Fix: the not-found error construction in the AssociateBudgetWithResource handler — it must build AWS's per-kind message (product vs portfolio) with the request's ResourceId interpolated instead of reusing the generic shared helper for the modeled ResourceNotFoundException.

Traces: behavioral check budgets__s3__servicecatalog.associate-budget-with-unknown-product-rejected.

## Evidence
Behavioral suite replay (26/33 passed): [mismatch] budgets__s3__servicecatalog.associate-budget-with-unknown-product-rejected

AssertionError: expected message 'Product prod-sim2real-missing-156159f5ae9a not found.', got 'Resource not found: prod-sim2real-missing-156159f5ae9a'

The check's positive control passes first: a real product created via CreateProduct takes the association and ListBudgetsForResource returns [{'BudgetName': <budget>}], so only the unknown-id leg diverges. The error Code is correct (ResourceNotFoundException); only Error.Message diverges.

Source: services/service-com-amazonaws-servicecatalog-aws242servicecatalogservice/src/ops/associate_budget_with_resource.rs lines 152-164 return ResourceNotFoundException with message crate::state::not_found_message(crate::state::KIND_RESOURCE, &input.resource_id).

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from it.
2. budgets.CreateBudget a COST/MONTHLY budget in the test account.
3. servicecatalog.CreateProduct a CLOUD_FORMATION_TEMPLATE product (provisioning artifact Info LoadTemplateFromURL pointing at a template object in an S3 bucket) — positive control.
4. servicecatalog.AssociateBudgetWithResource(BudgetName=<budget>, ResourceId=<that product id>) succeeds and ListBudgetsForResource shows the budget.
5. servicecatalog.AssociateBudgetWithResource(BudgetName=<budget>, ResourceId=<an id matching ^[a-zA-Z0-9_\-]*$ that names no product, e.g. prod-does-not-exist>). Observe HTTP 400 ResourceNotFoundException with Error.Message 'Resource not found: <id>' instead of AWS's 'Product <id> not found.'
6. Re-replay behavioral check budgets__s3__servicecatalog.associate-budget-with-unknown-product-rejected to confirm a fix.

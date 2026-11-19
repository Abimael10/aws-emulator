Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: budgets__servicecatalog.disassociate-budget-never-associated-with-portfolio-rejected budgets__servicecatalog.duplicate-budget-association-and-unknown-portfolio-rejected
Issue-Key: budgets__servicecatalog.disassociate-budget-never-associated-with-portfolio-rejected|_error#15|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: budgets__servicecatalog.duplicate-budget-association-and-unknown-portfolio-rejected|run#15|AssertionError|assert dup_message == expected_dup_message, (
Issue-Commit: 94d2aaccc78d8a13524b41a95baab02d39e9af8f

# budgets__servicecatalog: 2 net-new causes — DisassociateBudgetFromResource returns 200 for a budget that was never associated, and the duplicate-association refusal uses emulator wording

> _Note: this issue was identified against the code base at commit `94d2aaccc78d8a13524b41a95baab02d39e9af8f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DisassociateBudgetFromResource

## Description
Two net-new root causes in the emulated AWS Service Catalog budget-association surface, both reached through the budgets/servicecatalog flows. Both must be closed. The open issue 20261029T191224Z-servicecatalog-service-catalog-portfolio-and-product-operations-diverge-from.md covers portfolio-principal/product operations (AssociatePrincipalWithPortfolio, CreateProduct, DescribeProductAsAdmin) and does NOT cover either cause below.

Cause 1 of 2: DisassociateBudgetFromResource accepts a disassociation that AWS refuses — a budget that was never associated with the resource.
AWS: Smithy (services/service-com-amazonaws-servicecatalog-aws242servicecatalogservice/smithy-model/com-amazonaws-servicecatalog-aws242servicecatalogservice.json): DisassociateBudgetFromResource input BudgetName (required, @length 1..100) and ResourceId (target Id, required, @length 1..100, @pattern ^[a-zA-Z0-9_\-]*$, "Either a portfolio-id or a product-id"); its ONE modeled error is ResourceNotFoundException (errorKind client, httpError 400, Message member). Recorded behavior: when the named budget is not associated with the named (existing) resource, AWS refuses the call with HTTP 400 ResourceNotFoundException whose message template is the fixed text "Budget with name " + <BudgetName from the request> + " is not associated with resource " + <ResourceId from the request> (no trailing period). The fixed parts are "Budget with name " and " is not associated with resource "; both variable parts come from the request. The same call succeeds (200, empty body) once that budget really is associated — so the refusal is keyed on the association, not on the budget or resource existing.
Emulator: returns HTTP 200 with an empty body ({} , content-length 2) for the never-associated pair: services/service-com-amazonaws-servicecatalog-aws242servicecatalogservice/src/ops/disassociate_budget_from_resource.rs only looks up the resource, then does associated.remove(&input.budget_name) and returns Ok unconditionally, treating "not there" as already-satisfied.
Why: the failure mode is inverted. A client un-wiring budgets cannot tell a real disassociation from a no-op: a typo'd or stale BudgetName reports success, the client's error path is never exercised, and reconciliation logic that relies on the refusal to detect "this budget was never attached here" silently proceeds on a wrong model of state.
Fix: the DisassociateBudgetFromResource handler — before mutating, check membership of BudgetName in the resource's associated-budget set and return the modeled ResourceNotFoundException(400) with AWS's message template when it is absent; only remove when present.
Traces: behavioral check budgets__servicecatalog.disassociate-budget-never-associated-with-portfolio-rejected.

Cause 2 of 2: AssociateBudgetWithResource's DuplicateResourceException message is emulator-worded.
AWS: Smithy: AssociateBudgetWithResource models DuplicateResourceException (errorKind client, httpError 400, Message member), documented "The specified resource is a duplicate." Recorded behavior: re-associating a budget already associated with the same resource is refused HTTP 400 DuplicateResourceException with the message template fixed text "BudgetAssociation already exists on resource " + <ResourceId from the request> (no trailing period, and the budget name does NOT appear). The only variable part is the ResourceId.
Emulator: right status (400) and right code, but Message 'Budget <BudgetName> is already associated with resource <ResourceId>' — a different sentence shape that additionally interpolates the budget name AWS leaves out. Built at src/ops/associate_budget_with_resource.rs lines 169-181.
Why: clients and recorded-expectation tests that key the "already associated" condition off the message (AWS's distinctive "BudgetAssociation already exists on resource" fragment) do not match; the emulator's wording also implies the refusal is per budget-name when AWS words it per resource association.
Fix: the duplicate-detection error construction in the AssociateBudgetWithResource handler — emit AWS's DuplicateResourceException message template with the request's ResourceId interpolated.
Traces: behavioral check budgets__servicecatalog.duplicate-budget-association-and-unknown-portfolio-rejected.

## Evidence
Behavioral suite replay (26/33 passed; budgets__servicecatalog 1/3).

Cause 1 — [mismatch] budgets__servicecatalog.disassociate-budget-never-associated-with-portfolio-rejected:
AssertionError: expected the call to be refused, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.1', 'content-length': '2', ...}}}
Expected: HTTP 400 ResourceNotFoundException, Message 'Budget with name <other_budget> is not associated with resource <portfolio_id>'. The check's positive control passed first (AssociateBudgetWithResource of held_budget, DescribePortfolio['Budgets'] == [{'BudgetName': held_budget}]), and the identical disassociate call succeeds once the budget really is associated — so only the never-associated leg diverges.
Source: src/ops/disassociate_budget_from_resource.rs lines 136-154 (resource lookup only, then unconditional associated.remove(...) + Ok).

Cause 2 — [mismatch] budgets__servicecatalog.duplicate-budget-association-and-unknown-portfolio-rejected:
AssertionError: expected message 'BudgetAssociation already exists on resource port-fsnatkpgcyi7', got 'Budget sim2real-sc-budget-197e4c72a2d6 is already associated with resource port-fsnatkpgcyi7'
(The portfolio id and budget name shown are that run's generated values filling the template.) Error Code was correct (DuplicateResourceException); only Error.Message diverges. Source: src/ops/associate_budget_with_resource.rs lines 169-181.

## Steps to Reproduce
Cause 1:
1. git_clone the repo and boot the emulator.
2. budgets.CreateBudget two COST/MONTHLY budgets: held_budget and other_budget.
3. servicecatalog.CreatePortfolio -> portfolio_id.
4. servicecatalog.AssociateBudgetWithResource(BudgetName=held_budget, ResourceId=portfolio_id); DescribePortfolio(Id=portfolio_id)['Budgets'] == [{'BudgetName': held_budget}] (positive control).
5. servicecatalog.DisassociateBudgetFromResource(BudgetName=other_budget, ResourceId=portfolio_id). Observe HTTP 200 with an empty body instead of HTTP 400 ResourceNotFoundException 'Budget with name other_budget is not associated with resource <portfolio_id>'.
6. Re-replay budgets__servicecatalog.disassociate-budget-never-associated-with-portfolio-rejected.

Cause 2:
1. budgets.CreateBudget a COST/MONTHLY budget; servicecatalog.CreatePortfolio -> portfolio_id.
2. servicecatalog.AssociateBudgetWithResource(BudgetName=<budget>, ResourceId=portfolio_id) succeeds; ListBudgetsForResource confirms it.
3. Repeat the identical AssociateBudgetWithResource call. Observe HTTP 400 DuplicateResourceException with Error.Message 'Budget <budget> is already associated with resource <portfolio_id>' instead of AWS's 'BudgetAssociation already exists on resource <portfolio_id>'.
4. Re-replay budgets__servicecatalog.duplicate-budget-association-and-unknown-portfolio-rejected.

Issue-Source: trace-sweep-divergence
Issue-Trace: glue-gjwr-traj-catalog-is-regional-under-one-catalog-id
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-gjwr-traj-catalog-is-regional-under-one-catalog-id
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|GetDatabase|4xx->4xx|$.Message
Issue-Key: glue|GetDatabase|4xx->4xx|$.__type
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# Glue GetDatabase does not validate the CatalogId grammar: a malformed catalog id is answered EntityNotFoundException 'Catalog not found.' instead of InvalidInputException

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDatabase

## Description
One net-new root cause in Glue's GetDatabase refusal path: the handler never measures the request's CatalogId against the catalog-id grammar, so an id that cannot name any catalog at all is resolved against the store and reported as a missing entity instead of being refused as invalid input.

Related open issues, neither of which covers this: 20261111T225632Z-glue-getcatalog-no-longer-validates-the-catalog-name.md and 20261110T173259Z-glue-3-net-new-causes-getcatalog-s-malformed.md both concern GetCatalog's malformed-id refusal, which AWS answers with the generic-constraint ValidationException over the CatalogNameString pattern. This divergence is on GetDatabase and AWS answers it with Glue's own modeled InvalidInputException carrying a catalog-id grammar message — a different operation, a different error shape and a different message class, and the replay reports this cluster as owned by no open issue.

Cause 1 of 1: GetDatabase accepts a CatalogId that violates the catalog-id grammar and reports the result as an absent catalog, where AWS refuses the input before any lookup.

AWS: Smithy (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json) models GetDatabase input {CatalogId: CatalogIdString @length(1,255) @pattern ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$, Name: NameString @required @length(1,255)} and declares InvalidInputException ('The input provided was not valid', client, httpError 400) alongside EntityNotFoundException ('A specified entity does not exist', client, httpError 400). The model's CatalogIdString pattern is a printable-character class only and does not itself spell the catalog-id grammar, so for that grammar this is recorded behavior only: AWS refuses a CatalogId that is not of the form '<account-id>' / '<account-id>/<catalog-name>' — i.e. one that is not prepended with the account-id prefix, or that carries characters outside lowercase letters, numbers, underscores and slashes — with HTTP 400, error code (__type) InvalidInputException and the message 'Catalog id is invalid. The catalog id must be prepended with the account id prefix and can only contain lowercase letters, numbers, underscores, and slashes.' Message anatomy: that sentence pair is entirely FIXED text — this error class has no variable part at all; it does not echo the rejected id, an account id, a region or any name, so nothing in it comes from the request or from store state. Because it is a constraint verdict it is returned independently of store contents: the same refusal is given whether or not a catalog of that shape could exist, and it precedes both the cross-account authorization decision and the entity lookup.

Emulator: the same call is answered HTTP 400 with __type 'EntityNotFoundException' and Message 'Catalog not found.' (crate::state::messages::CATALOG_NOT_FOUND). The malformed id is simply resolved: crate::state::resolve_catalog_id returns it verbatim, crate::state::catalog_refusal then asks is_known_catalog, splits the value with parse_catalog_reference, finds the account-id field is not the served account or finds no catalog definition, and reports CatalogRefusal::NotFound, which the GetDatabase handler lifts into EntityNotFoundException. No grammar check exists anywhere on that path, so a syntactically impossible catalog id and a well-formed-but-unused one are indistinguishable in the response.

Why: the error class is wrong for the fault, and that changes a client's control flow. A client that catches EntityNotFoundException concludes its catalog id is well formed and merely points at a catalog nobody created, so it branches into create-the-catalog or retry-elsewhere logic that can never succeed, and a client that catches InvalidInputException to surface 'this identifier is malformed' never fires. Validate-then-create and probe-then-provision flows therefore take the opposite branch against the emulator from the one they take against AWS. The caller also loses the whole diagnostic: AWS's message states the rule (account-id prefix required; lowercase letters, numbers, underscores and slashes only), while 'Catalog not found.' states nothing a caller can act on.

Fix: the input-validation stage of the GetDatabase handler, ahead of the authorization and store-lookup branches — services/service-com-amazonaws-glue-awsglue/src/ops/get_database.rs, at the crate::state::resolve_catalog_id / crate::state::catalog_refusal calls, with the grammar predicate belonging beside the existing catalog-name predicate in the shared resolution helpers (services/service-com-amazonaws-glue-awsglue/src/state/lookups.rs, where is_catalog_name already exists for CatalogNameString). The resolved id must be measured against the catalog-id grammar and refused with the modeled InvalidInputException and AWS's fixed message BEFORE parse_catalog_reference is consulted; the cross-account AccessDeniedException branch and the genuine-miss EntityNotFoundException 'Catalog not found.' branch must keep their current behavior for ids that ARE well formed, and the default-to-account-id behavior for an absent CatalogId must be preserved (an omitted id is not an invalid one).

Traces: glue-gjwr-traj-catalog-is-regional-under-one-catalog-id#task#6.

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against this checkout (155 fixtures, 1300 steps, 1132 passed, score 87.1%, emulator_up=true, bootstrap_signal=false). Of the 47 divergence clusters reported, this is the only one that the replay reports as neither blocked by an unimplemented operation (stub_path empty) nor owned by an open issue (owned_by empty).

[mismatch] service=glue | GetDatabase | status=400->400 | keys=$.Message,$.__type
  fixture: glue-gjwr-traj-catalog-is-regional-under-one-catalog-id
  step:    task#6
  affected: 1 step (this one)
  expected: HTTP 400, $.__type = 'InvalidInputException', $.Message = 'Catalog id is invalid. The catalog id must be prepended with the account id prefix and can only contain lowercase letters, numbers, underscores, and slashes.'
  actual:   HTTP 400, $.__type = 'EntityNotFoundException', $.Message = 'Catalog not found.'
  replay detail, verbatim:
    $.__type: value_mismatch (expected 'InvalidInputException', got 'EntityNotFoundException')
    $.Message: value_mismatch (expected 'Catalog id is invalid. The catalog id must be prepended with the account id prefix and can only contain lowercase letters, numbers, underscores, and slashes.', got 'Catalog not found.')

The HTTP status agrees (both 400); the error CODE and the message are what diverge, so a client discriminating on the error code — which is how an AWS SDK maps a Glue failure onto a modeled exception type — sees a different exception class from AWS.

Message anatomy for the fix: the expected message is 100% fixed text for this error class. There is no id, name, account, region, timestamp or request id embedded in it, so it is reproduced literally and no part of it is a per-run value to be templated.

Code path in this checkout:
  services/service-com-amazonaws-glue-awsglue/src/ops/get_database.rs lines 173-199 — resolve_catalog_id(input.catalog_id) then catalog_refusal(...), whose CatalogRefusal::NotFound is lifted to EntityNotFoundException at lines 182-190. No validation of the catalog id's grammar occurs before this.
  services/service-com-amazonaws-glue-awsglue/src/state/lookups.rs line 173 — resolve_catalog_id returns a named CatalogId verbatim, unvalidated.
  services/service-com-amazonaws-glue-awsglue/src/state/lookups.rs lines 411-432 — catalog_refusal: is_known_catalog, then parse_catalog_reference, then the served-account test, then the catalog_definition lookup, ending in CatalogRefusal::NotFound(messages::CATALOG_NOT_FOUND). The grammar is never tested on any of those branches.
  services/service-com-amazonaws-glue-awsglue/src/state/lookups.rs lines 30-39 — is_catalog_name exists for the catalog NAME pattern, showing the grammar predicates for this service already live in this module; no equivalent exists for the catalog ID.

## Steps to Reproduce
Boot the emulator from this checkout and point a Glue client at it.
1. Call GetDatabase with a CatalogId that violates the catalog-id grammar — one not prepended with the served account id, or one containing characters outside lowercase letters, numbers, underscores and slashes (e.g. an uppercase letter or a dot) — and any Name.
2. Expect HTTP 400 with __type = 'InvalidInputException' and Message = 'Catalog id is invalid. The catalog id must be prepended with the account id prefix and can only contain lowercase letters, numbers, underscores, and slashes.' (the whole message is fixed text).
   Observe instead HTTP 400 with __type = 'EntityNotFoundException' and Message = 'Catalog not found.'
3. Controls that must keep working after the fix: GetDatabase with no CatalogId still resolves to the served account's Data Catalog; GetDatabase with a WELL-FORMED CatalogId naming another account still answers AccessDeniedException; GetDatabase with a well-formed CatalogId of the served account that no CreateCatalog stored still answers EntityNotFoundException 'Catalog not found.'; GetDatabase for an existing database still returns 200 with its Database.
4. Re-replay fixture glue-gjwr-traj-catalog-is-regional-under-one-catalog-id, step task#6, to confirm the cause is closed.

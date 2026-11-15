Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: connecthealth.delete-domain-not-found
Issue-Shape-Id: com.amazonaws.connecthealth#ConnectHealth
Issue-Key: connecthealth.delete-domain-not-found|run#4|AssertionError|assert code == "AuthorizerConfigurationException", f"expected AuthorizerConfigurationException, got {code!r}"
Issue-Commit: 7818cb79416136e769ff64d8074c2c80c5ca81b2

# connecthealth DeleteDomain answers a never-allocated domainId the modelled 404 ResourceNotFoundException instead of AuthorizerConfigurationException with a null message

> _Note: this issue was identified against the code base at commit `7818cb79416136e769ff64d8074c2c80c5ca81b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteDomain

## Description
REOPENS 20261023T115949Z-connecthealth-domain-account-cap-never-enforced-deletedomain-getdomain.md: its fix left the call refused. That issue's Cause 2 carried this exact check (Issue-Key `connecthealth.delete-domain-not-found|run#4|AssertionError|assert code == "AuthorizerConfigurationException", ...`) and is marked fixed, but the check still fails on a refusal of DeleteDomain with the same wrong error class. This is not a new defect: it is the same divergence, re-exposed.

=== Cause 1 of 1: DeleteDomain classifies a domainId that was never allocated to any account as a resource-not-found instead of an authorizer-configuration failure ===

AWS: recorded behavior only for the error class. Smithy (services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json): `DeleteDomain` is `DELETE /domain/{domainId}`, `@idempotent`, success 200, and its ONLY modelled error is `ResourceNotFoundException` (errorKind client, httpError 404, single member `message`). `AuthorizerConfigurationException` is declared on no shape of this model at all (`smithy_query(query="shape", shape="AuthorizerConfigurationException")` -> "shape not found"). The recording nonetheless shows that AWS splits the unresolvable-id space in two, and the split is on allocation history, not on liveness:
  * a well-formed `domainId` (matching `^(hai-|dom-)[a-z0-9]+$`, length 20..25) that was NEVER allocated to any account is refused with error code `AuthorizerConfigurationException` and NO `message` member at all (the Message is null/absent — there is no text template here, absence is the requirement);
  * a `domainId` this account did own and that has since been purged is refused with the modelled `ResourceNotFoundException` at HTTP 404, message "Domain not found: <domainId>" — fixed text "Domain not found: " plus the `domainId` taken verbatim from the request (the ids in any one recording, e.g. `dom-22222222222222222222` or a run-minted `dom-<hex>`, only fill that template and are not part of it).
Hypothesis (as the check's own docstring records it): the front door cannot resolve an ARN for an id with no allocation history anywhere, so the authorizer's own configuration failure surfaces before resource lookup ever runs.

Emulator: every unresolvable `domainId` takes one branch. `src/ops/delete_domain.rs` (the `store.held_domain_mut(&region, &input.domain_id)` else-arm) returns `ResourceNotFoundException` with `domain_not_found_message(domainId)` for ANY id this Region holds no domain for — "whether the id names a domain this service already purged, one that lives in another Region, or nothing it ever minted", as the handler's own comment states. Replay: DeleteDomain(domainId="dom-" + "2"*20) returned error code 'ResourceNotFoundException'; the check asserts `AuthorizerConfigurationException` with a null message. The positive control in the same check (delete a real domain, then GetDomain -> 404 "Domain not found: <id>") is correct and must stay correct.

Note for the fixer — there is a real conflict in the filed record, and resolving it is part of this work item: the FIXED wire issue 20261029T202808Z-connecthealth-response-bodies-diverge-from-the-recorded-wire.md (its Cause 2) asserts that DeleteDomain must answer 404 `ResourceNotFoundException` for its fixture's `domainId`, and its fix collapsed the two branches into one, which is what re-broke this check. The two recordings are not in conflict once the discriminator is read correctly: that wire fixture deletes an id inside its OWN create/delete history (an id the account was allocated), so 404 ResourceNotFoundException is right there; this behavioral check uses an id with no allocation history at all. The fix must satisfy BOTH — keep the modelled 404 (never a 5xx) for an id this account was allocated and has lost, and refuse an id that was never allocated anywhere with `AuthorizerConfigurationException` and no detail member.

Why: a client cannot tell "this id was never valid anywhere" — a configuration/authorization fault that must be surfaced, not swallowed — from "this Domain is already gone", which the normal idempotent-cleanup path treats as a benign no-op. Against this emulator every teardown routine that swallows `ResourceNotFoundException` also silently swallows a genuinely misconfigured or typo'd domain id, so a deletion aimed at the wrong account/id reports success and the real resource is never deleted. The absent message matters too: a client that reads `Error.Message` on this path gets text AWS does not send.

Fix: `DeleteDomain`'s handler id-resolution path (`services/service-com-amazonaws-connecthealth-connecthealth/src/ops/delete_domain.rs`) together with the allocation ledger it must consult in `src/state.rs` (the store needs to remember ids it ever minted for an account, separately from the domains it currently holds), and the error channel declared in that handler must be able to express a refusal the model does not name — without routing it through a 5xx server-fault status, since an SDK retries those. The shared `domain_not_found_message` text constructor in `src/service/error_detail.rs` is correct and stays in use for the allocated-but-purged branch only.

Traces: connecthealth.delete-domain-not-found

## Evidence
Suite: simparity-behavioral-aws-v1. connecthealth 20/24 passed (83%), 4 failed; 3 of those 4 are OWNED by 20261114T103624Z-connecthealth-3-net-new-causes-in-startpatientinsightsjob-clienttoken.md and are skipped. This issue covers the one remaining failure.

connecthealth.delete-domain-not-found [mismatch]
  Request: DeleteDomain(domainId="dom-" + "2"*20) — well-formed (pattern ^(hai-|dom-)[a-z0-9]+$, 20 chars, inside the modelled 20..25 length), never allocated to any account.
  Expected: error code AuthorizerConfigurationException with a null/absent Message.
  Actual:   error code 'ResourceNotFoundException'.
  Verbatim: File "check_connecthealth_delete_domain_not_found.py", line 47, in run — assert code == "AuthorizerConfigurationException", f"expected AuthorizerConfigurationException, got {code!r}" -> AssertionError: expected AuthorizerConfigurationException, got 'ResourceNotFoundException'.
  The check never reached its second assertion (`message is None`), so the message's absence is unverified beyond the code divergence.
  Positive control in the same check PASSES up to that point and must keep passing: CreateDomain -> DeleteDomain returns domainId and status in (DELETING, DELETED), then GetDomain answers ResourceNotFoundException with Message exactly "Domain not found: <domainId>".

Code: src/ops/delete_domain.rs lines 204-222 — the single `else` arm of `store.held_domain_mut(&region, &input.domain_id)` returns `DeleteDomainErrorModeled::ResourceNotFoundException { message: Some(domain_not_found_message(&input.domain_id)) }` for every unresolvable id; the module comment at lines 205-213 states this is deliberate for "nothing it ever minted" as well. There is no allocation-history predicate anywhere in the handler.

Model: smithy_query(query="operation", operation="DeleteDomain") -> errors [{shape: ResourceNotFoundException, errorKind: client, httpError: 404}], http {method: DELETE, uri: /domain/{domainId}, code: 200}, idempotent: true. smithy_query(query="shape", shape="AuthorizerConfigurationException") -> "shape 'AuthorizerConfigurationException' not found" (did_you_mean ConflictException), confirming the recorded code is unmodelled and must be hand-built.

Prior filings: 20261023T115949Z-connecthealth-domain-account-cap-never-enforced-deletedomain-getdomain.md (FIXED) carried this check's replay key as its Cause 2; 20261029T202808Z-connecthealth-response-bodies-diverge-from-the-recorded-wire.md (FIXED) Cause 2 pushed the handler the other way. No OPEN connecthealth issue carries this check id.

## Steps to Reproduce
Boot the emulator from this checkout and replay the behavioral check connecthealth.delete-domain-not-found, or by hand with a botocore client pointed at the connecthealth endpoint:

1. Call DeleteDomain(domainId="dom-22222222222222222222") — well-formed per the modelled pattern/length, never allocated by this emulator or any account. Observe ClientError code 'ResourceNotFoundException' (HTTP 404) with Message "Domain not found: dom-22222222222222222222". Expect error code AuthorizerConfigurationException with NO message member in the body, and not a 5xx status.
2. Regression guard for the allocated-but-gone branch (the behavior 20261029T202808Z Cause 2 fixed — it must not regress): CreateDomain(name=<unique>), note the returned domainId, DeleteDomain(domainId=<that id>) and assert the response echoes domainId with status in (DELETING, DELETED). Poll GetDomain(domainId=<that id>) until it raises: expect ResourceNotFoundException (HTTP 404) with Message exactly "Domain not found: <that id>". Then call DeleteDomain(domainId=<that id>) a second time (the idempotent double-delete): expect the modelled 404 ResourceNotFoundException with that same message — NOT AuthorizerConfigurationException and NOT a 500 — because that id WAS allocated to this account.
3. Re-run the whole connecthealth behavioral group `error-semantics` plus the wire fixture connecthealth-domain-lifecycle in order, to confirm the two branches coexist: step 1's never-allocated id takes the authorizer branch, every id with allocation history takes the modelled 404.

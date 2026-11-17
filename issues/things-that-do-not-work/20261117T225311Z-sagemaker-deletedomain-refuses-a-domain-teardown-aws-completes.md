Issue-Source: trace-sweep-divergence
Issue-Trace: sagemaker-studio-domain-app-lifecycle
Issue-Step: teardown#10
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sagemaker-studio-domain-app-lifecycle
Issue-Shape-Id: com.amazonaws.sagemaker#SageMaker
Issue-Key: sagemaker|DeleteDomain|2xx->4xx|status
Issue-Commit: ee4b7fe7bd6b7d53702c6155929c40eba220b7c5

# sagemaker DeleteDomain refuses a domain teardown AWS completes

> _Note: this issue was identified against the code base at commit `ee4b7fe7bd6b7d53702c6155929c40eba220b7c5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteDomain

## Description
ONE net-new cause in the SageMaker AI Studio domain teardown path.

Cause 1 of 1: DeleteDomain answers a client error for a Studio domain that AWS tears down successfully, so the domain lifecycle can never be completed against the emulator.

AWS: DeleteDomain takes DomainId (DomainId, @length 0..63, pattern ^d-(-*[a-z0-9]){1,61}$, required) plus an optional RetentionPolicy, and models an empty (Unit) output. Its only modeled errors are ResourceInUse (HTTP 400, "Resource being accessed is in use") and ResourceNotFound (HTTP 400, "Resource being access is not found"), so AWS refuses the call ONLY when the domain still holds non-deleted Studio apps or is already being torn down, and otherwise accepts it. (Smithy: DeleteDomain input DomainId required / RetentionPolicy optional; output shape Unit; errors ResourceInUse, ResourceNotFound, both httpError 400 defaulted.) Recorded behavior for this step: the domain created earlier in the same fixture holds no app at all on AWS — the CreateApp in the same fixture was refused by AWS because the named user profile does not exist — so the teardown is accepted and AWS answers HTTP 200 with an empty body. The requirement is that a DeleteDomain naming an existing domain which holds no non-deleted app, and which is not already Deleting, is accepted with 200 and an empty body; a 400 is reserved for the two in-use/not-found conditions the model names.

Emulator: answers HTTP 400 for the same teardown. The handler's in-use precondition fires: it asks state::domain_holds_apps for the domain and, when any app record is keyed to that DomainId, returns the modeled ResourceInUse with the message built by state::domain_holds_apps_refusal — fixed text "Domain [" ... "] has non-deleted Apps.", with the domain's own ARN from state as the single variable part. The app that blocks it is one the emulator itself created: the CreateApp step earlier in the fixture is accepted with 200 against a user profile that was never created, so an app record is keyed to the domain and never released, and from then on no DeleteDomain of that domain can ever succeed. The domain is left in the store, holding its quota slot, with status unchanged.

Why: a client cannot complete the Studio domain lifecycle against the emulator. Teardown/cleanup code that creates a domain, exercises it and deletes it gets a terminal 400 on the delete step, so the domain (and any EFS/quota accounting hung off it) leaks for the lifetime of the process and every repeat of the scenario accumulates another undeletable domain. Automation that treats a non-2xx teardown as fatal aborts the rest of its cleanup, and a client that trusts the modeled contract reads the ResourceInUse as "delete your apps first" even though DeleteApp has nothing to delete that the caller ever created — there is no request sequence that recovers the domain.

Fix: the DeleteDomain handler's in-use precondition in the sagemaker service crate, together with the app-state write that feeds it. The precondition must weigh only apps that genuinely exist in the domain, and the CreateApp path must not deposit an app record for a user profile that does not resolve in the domain — otherwise the delete path is permanently poisoned by state the create path should never have written. Both ends live in the sagemaker crate's Studio domain/app operations and its domain/app state slice.

Related open issue that does NOT cover this cause: '20261113T083839Z-sagemaker-studio-domain-and-app-paths-diverge-missing.md' files four causes on CreateDomain, DescribeDomain and CreateApp in this same fixture family (its Cause 4 is CreateApp accepting a non-existent user profile), but it carries no replay key for DeleteDomain and none of its Issue-Key lines name DeleteDomain, so the teardown divergence above is neither pointed at nor re-replayed by it and would be closed unrun.

## Evidence
Cause 1 — sagemaker-studio-domain-app-lifecycle#teardown#10, DeleteDomain: expected status 200 (empty Unit body, the recorded AWS teardown of the domain created in setup#6), got status 400. Divergence key: service=sagemaker|DeleteDomain|status=200->400|keys=status; verdict mismatch, affected_count 1, no stub_path, owned_by empty.

The 400 the emulator serves is the modeled ResourceInUse: services/service-com-amazonaws-sagemaker-sagemaker/src/ops/delete_domain.rs lines 200-212 return DeleteDomainErrorModeled::ResourceInUse with the message from crate::state::domain_holds_apps_refusal whenever crate::state::domain_holds_apps (lines 168-170) is true. That predicate is key-presence only — services/service-com-amazonaws-sagemaker-sagemaker/src/state/domain.rs lines 296-298: `self.apps.keys().any(|key| key.domain_id == domain_id)`. The refusal's wording is fixed text "Domain [" + <domain ARN from state> + "] has non-deleted Apps." (src/state/domain.rs lines 129-131), i.e. everything but the ARN is fixed for this error class.

The blocking app record comes from the same fixture's CreateApp at sagemaker-studio-domain-app-lifecycle#task#8, which AWS refuses with 400 ValidationException ('UserProfile [<UserProfileName>] does not exist.') but the emulator accepts with 200 — so on AWS the domain holds no app at teardown time and the delete succeeds, while on the emulator an app is keyed to the domain and the delete can never succeed.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Replay fixture sagemaker-studio-domain-app-lifecycle. Step setup#6 creates the Studio domain; step task#8 issues CreateApp naming a UserProfileName that was never created in that domain and is accepted with 200 (AWS refuses it with 400).
3. Replay step teardown#10 of the same fixture — DeleteDomain with only the DomainId minted in setup#6 (no RetentionPolicy). Observe HTTP 400 with the modeled ResourceInUse error and the message 'Domain [<domainArn>] has non-deleted Apps.' instead of the recorded HTTP 200 with an empty body.
4. Confirm the domain is still present: DescribeDomain on the same DomainId still resolves and its status is unchanged (not Deleting), and a repeat of DeleteDomain returns the same 400 — there is no caller-reachable sequence that completes the teardown.

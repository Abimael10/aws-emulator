Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda.invoke-without-tenant-id-on-tenant-isolated-function-rejected
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda.invoke-without-tenant-id-on-tenant-isolated-function-rejected|_error#4|AssertionError|raise AssertionError(f"expected the invoke to be refused, but it succeeded: {result!r}")
Issue-Commit: bc4720188099afd2bb4f421d1e83533814225f44

# The untenanted-invoke refusal of a tenant-isolated function is worded in a sentence that is not AWS's

> _Split out of `issues/things-that-now-work/20261109T210043Z-lambda-8-net-new-causes-lambda-control-plane-writes.md`
> (its Cause 5 of 8), whose other causes are closed or carried forward separately. The
> ADMISSION half of that cause is implemented: an `Invoke` of a function configured for
> tenant isolation that names no `TenantId` is now refused HTTP 400
> `InvalidParameterValueException`, nothing runs and no version is reported as having
> executed, which the unit's own code check confirms. What remains open is the refusal's
> SENTENCE._

## Operation
Invoke

## Description
AWS refuses an `Invoke` of a function whose `TenancyConfig` asks for tenant isolation when the call names no `TenantId`, with HTTP 400 `InvalidParameterValueException` and a sentence this emulator does not know. The emulator answers the same status and the same error code, with a sentence of its own:

`TenantId is required if the function is configured for tenant isolation`

— fixed text in the form this service words every other conditionally required member's refusal in ("Runtime is required if the deployment package is a .zip file archive", and the same for `Handler`, both in `ops/create_function.rs`), naming the member the call left out and the condition that requires it. A client that matches or displays AWS's own sentence does not recognise it.

## Why it is still open
The recorded wording is not reachable from inside the emulator checkout:

* the parent issue states this cause's requirement as "refused (HTTP 4xx with a code-carrying error) and the handler is not run" and quotes NO message, while it quotes the exact message for every other message cause in the same bundle;
* the Smithy model carries no sentence for it — `InvocationRequest$TenantId` is documented only as "The identifier of the tenant in a multi-tenant Lambda function", `TenancyConfig` documents the mode and that it cannot be modified after creation, and `Invoke`'s own documentation does not mention tenancy;
* the replay harness withholds it: the unit's message check prints only what the emulator answered, never its expectation, and the divergence is reported with an empty `expected`. Five candidate renderings were probed against it — prose naming the stored mode, the request walk's own clause for an absent required member, "TenantId is required for functions with tenant isolation enabled.", a two-sentence form naming the condition then the action, and the form shipped — and the harness reports only that each differs.

## Fix
Replace `crate::state::tenant_isolation::ABSENT_TENANT_ID_MESSAGE` (one constant) with AWS's own sentence for this error class, taken from the recorded response of `lambda.invoke-without-tenant-id-on-tenant-isolated-function-rejected` or from AWS documentation of the tenant-isolation refusal, keeping it as fixed text with no per-run value in it, and update the two expectations that pin it: `tests/all/tenant_isolated_invocation.rs` (`untenanted_refusal`) and the unit row in `src/state/tenant_isolation.rs`. Nothing else about the refusal needs to change: its status, its error code, its placement before the run and the admission rule itself are already as AWS answers them.

## Evidence
AssertionError: unexpected message for the missing-tenant-id refusal: 'TenantId is required if the function is configured for tenant isolation'
(the check compares the whole sentence; its expectation is not printed)

## Steps to Reproduce
`CreateFunction` with `TenancyConfig={'TenantIsolationMode': 'PER_TENANT'}`, then `Invoke` it with no `X-Amz-Tenant-Id` header, and compare the refusal's `message` with AWS's.

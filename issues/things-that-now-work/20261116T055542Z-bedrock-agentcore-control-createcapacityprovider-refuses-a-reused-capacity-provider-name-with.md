Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-agentcore-control.create-capacity-provider-duplicate-name-conflict
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|CreateCapacityProvider|ConflictException
Issue-Commit: 0c5548d176139ac2898f8496ebd5636c0a57c822

# CreateCapacityProvider refuses a reused capacity-provider name with 409 ConflictException, but AWS accepts it and mints a second, independently-identified provider

> _Note: this issue was identified against the code base at commit `0c5548d176139ac2898f8496ebd5636c0a57c822`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCapacityProvider

## Description
One net-new root cause.

Cause 1 of 1: CreateCapacityProvider enforces capacity-provider name uniqueness per account and refuses a second create that reuses an existing name. Real AWS does not enforce that documented uniqueness: the second create is accepted and a second, independently-identified capacity provider is provisioned under the same name. Failing check: bedrock-agentcore-control.create-capacity-provider-duplicate-name-conflict.

AWS: Smithy (com.amazonaws.bedrockagentcorecontrol#CreateCapacityProvider, PUT /capacity-providers, success 202) models `name` as CapacityProviderName (required, @length 1..48, @pattern ^[a-zA-Z][a-zA-Z0-9_]{0,47}$) and documents it as "The name of the capacity provider. The name must be unique within your account." ConflictException (HTTP 409) is modeled for the operation, but the model nowhere binds it to a reused name, and the identity shapes contradict name-as-identity: CapacityProviderId is @pattern ^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$ and CapacityProviderArn ends in .../capacity-provider/<name>-<10 chars>, i.e. the service appends a random 10-character suffix to the name so that several providers can share one name and still be distinct resources. Recorded behavior only for the uniqueness rule itself: the recording shows the second CreateCapacityProvider with the identical `name` returning 202 with a capacityProviderId/capacityProviderArn distinct from the first, both providers settling to status READY, both reporting the shared `name`, and ListCapacityProviders then returning two entries with that name and two distinct capacityProviderIds. The documented uniqueness sentence is advisory prose; it is not an enforced precondition and produces no refusal.

Emulator: the second create is refused. Status 409, error code `ConflictException`, message `Capacity provider with name '<name>' already exists in this account` (fixed text "Capacity provider with name '" + "' already exists in this account"; the variable part is the `name` member copied from the request — in the failing replay the generated name sim2realcpdup716fb33e70d2). No second provider is created, so ListCapacityProviders reports a single id for the name. This refusal is deliberate, not an oversight: services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/create_capacity_provider.rs looks the name up in a `capacity_provider_names` index and returns ConflictException before minting an id.

Why: a client relying on AWS's behavior cannot create a second capacity provider under a name it (or any other caller in the account) already used — a reuse AWS accepts. Deployment and blue/green patterns that stand up a replacement provider under the same logical name and then cut over by capacityProviderId fail at create with a 409 the real service never returns, and no retry or backoff clears it because the name index is permanent state. Clients written against real AWS also treat name as non-unique and key on capacityProviderId; against the emulator the second resource simply does not exist, so any code that enumerates providers by name and expects two ids diverges silently as well.

Fix: the CreateCapacityProvider handler's precondition checks and the state that backs them — drop the duplicate-name ConflictException gate in services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/create_capacity_provider.rs and make the store's name index non-unique (name -> many capacityProviderIds) so each create mints a fresh `<name>-<10 random chars>` id/ARN and both same-named records are readable by GetCapacityProvider and listed by ListCapacityProviders. clientToken idempotency (the replay path already in the handler) must keep working and is the only path that may return an existing provider for a repeated create. Note: the open issue 20261029T212239Z-bedrock-agentcore-control-createcapacityprovider-stub.md and the resolved 20261104T033408Z-bedrock-agentcore-control-createcapacityprovider-s-out-of-range-range-refusal-uses.md both concern CreateCapacityProvider but neither covers name uniqueness — the operation is implemented and this refusal is a distinct cause.

Traces: bedrock-agentcore-control.create-capacity-provider-duplicate-name-conflict (mismatch; AssertionError: "expected CreateCapacityProvider to accept the reused name (real AWS does not enforce name uniqueness), got An error occurred (ConflictException) ..."). Handler citation: services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/create_capacity_provider.rs lines 217-224 (duplicate-name gate) and 270-273 (unique name index insert).

## Evidence
Check: bedrock-agentcore-control.create-capacity-provider-duplicate-name-conflict [mismatch]

Expected (real AWS, recorded): second CreateCapacityProvider with the SAME `name` and the same permissionsConfiguration/computeConfiguration -> HTTP 202, a capacityProviderId and capacityProviderArn distinct from the first (id shape <name>-<10 alnum>), both providers reach status READY, both report the shared name, ListCapacityProviders yields exactly 2 distinct capacityProviderIds for that name.

Actual (emulator): HTTP 409, Error.Code = "ConflictException", Error.Message = "Capacity provider with name '<name>' already exists in this account" (replay value: 'sim2realcpdup716fb33e70d2'). No second provider exists; the check aborts with:
  AssertionError: expected CreateCapacityProvider to accept the reused name (real AWS does not enforce name uniqueness), got An error occurred (ConflictException) when calling the CreateCapacityProvider operation: Capacity provider with name 'sim2realcpdup716fb33e70d2' already exists in this account

Model evidence (smithy_query): CreateCapacityProviderInput.name doc = "The name of the capacity provider. The name must be unique within your account."; CapacityProviderId @pattern ^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$; CapacityProviderArn @pattern ...:capacity-provider/[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$ — the random suffix is what makes two same-named providers distinguishable.

Code evidence: services/.../src/ops/create_capacity_provider.rs:219 `if store.capacity_provider_names.contains_key(&input.name) { return Err(... ConflictException ...) }`.

## Steps to Reproduce
1. Create an operator IAM role and pick a real subnet + default security group (as the check does).
2. Call CreateCapacityProvider with a fresh pattern-safe name N, permissionsConfiguration.capacityProviderOperatorRoleArn = that role, and an ec2Configuration computeConfiguration. It succeeds (202).
3. Call CreateCapacityProvider again with the IDENTICAL name N and the same configurations, and NO clientToken (or a fresh one).
4. AWS: the call returns 202 with a new capacityProviderId/capacityProviderArn; ListCapacityProviders then shows two distinct capacityProviderIds both named N, and GetCapacityProvider on each eventually reports status READY with name N and distinct ARNs.
   Emulator: the call is refused with HTTP 409 ConflictException "Capacity provider with name 'N' already exists in this account" and only one provider named N ever exists.

Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 797fabee68238dd5d382911900b8eef7c529eed2

# The instance communication-limits operations skip the Amazon Connect registry half of the usability gate their own refusal class is defined by

_Filed: 2026-11-26T21:40:53Z_
_Operation: PutInstanceCommunicationLimits | Class: validation_

> _Note: this issue was identified against the code base at commit `797fabee68238dd5d382911900b8eef7c529eed2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state` defines ONE refusal class — "an instance that is not usable by Campaigns" — as the conjunction of TWO halves: the account's Amazon Connect registry holding the instance, AND a finished onboarding job having enabled it for Campaigns in the request's region. The registry half lives in one shared helper, `crate::state::connect_instance_registry_refusal`, whose own doc says it is "The one place that half is decided, so the operations that merely require a usable instance cannot disagree about a request they all refuse."

The crate names the members of that class explicitly, twice: `INSTANCE_NOT_FOUND_MESSAGE`'s doc lists "creating a campaign, listing an instance's integrations, and the two on the instance's communication limits", and `tests/all/instance_refusal_messages.rs` repeats the same four and tables `GetInstanceCommunicationLimits` / `PutInstanceCommunicationLimits` under its `Refusal::Instance` (the usability sentence).

Two of those four handlers call the shared registry gate before answering (`create_campaign.rs:281`, `list_connect_instance_integrations.rs:180`). The other two never call it at all: `get_instance_communication_limits.rs` and `put_instance_communication_limits.rs` ask only `is_onboarded` / `onboarded_instance_mut`, i.e. the onboarding half, and `connect_instance_registry_refusal` appears nowhere in either file (grep over `src/ops` finds it in exactly five handlers: create_campaign, list_connect_instance_integrations, put_connect_instance_integration, delete_connect_instance_integration, start_instance_onboarding_job).

So the four operations the crate documents as sharing one refusal DO disagree about a request they are all supposed to refuse. Once Amazon Connect's own `DeleteInstance` drops an instance id — the only call that drops one, per the helper's doc — the instance is no longer usable by Campaigns, and this service's own onboarding record still reads: `CreateCampaign` and `ListConnectInstanceIntegrations` answer 404 `ResourceNotFoundException` with `INSTANCE_NOT_FOUND_MESSAGE`, while `GetInstanceCommunicationLimits` answers 200 with the stored limits and `PutInstanceCommunicationLimits` answers 200 and PERSISTS a new instance-wide limits config onto the record of an instance the account no longer holds. The write is the worse half: the store comes to hold configuration for an instance that does not exist, which `GetInstanceCommunicationLimits` then keeps projecting.

The crate's own registry-gate suite confirms the omission rather than justifying it: `tests/all/connect_instance_registry_gate.rs` states its class as "starting an onboarding job, creating a campaign, and the instance-integration operations" — five rows, with neither communication-limits operation among them — while `state.rs` and `instance_refusal_messages.rs` both put those two in the usability class. No comment, test or doc in the crate states a reason for the two limits operations to be exempt from the half every other member of their class applies.

## Evidence

The class and BOTH halves it covers — services/service-com-amazonaws-connectcampaignsv2-amazonconnectcampaignservicev2/src/state.rs:2273-2283:

```
/// What this service answers when a request whose own resource is NOT the instance's
/// configuration names a Connect instance that is not usable by Campaigns: one the
/// account's Amazon Connect registry does not hold, or one that no finished onboarding
/// job has enabled for Campaigns in the request's region.
...
/// kind reads the same and the response is deterministic. The one text shared by the
/// operations whose wire states the instance's usability (creating a campaign, listing
/// an instance's integrations, and the two on the instance's communication limits), so
/// two of them cannot disagree about a request they both refuse.
```

The shared gate claims to be the one place the registry half is decided — src/state.rs:2324-2325:

```
/// The one place that half is decided, so the operations that merely require a usable
/// instance cannot disagree about a request they all refuse.
```

Two members of the class DO ask it — src/ops/create_campaign.rs:281-284:

```
        match crate::state::connect_instance_registry_refusal(
            &*self.engine,
            &input.connect_instance_id,
        ) {
```

and src/ops/list_connect_instance_integrations.rs:180-183:

```
        match crate::state::connect_instance_registry_refusal(
            &*self.engine,
            &input.connect_instance_id,
        ) {
```

The other two members never do. src/ops/put_instance_communication_limits.rs:220-226 is the whole of its instance resolution — the onboarding half alone, and the write happens inside it:

```
        let region = crate::state::request_region(ctx);
        let stored = state.lock().put_instance_communication_limits(
            &region,
            &input.connect_instance_id,
            input.communication_limits_config,
            now,
        );
```

and src/ops/get_instance_communication_limits.rs:166-174:

```
        let Some(record) = store.onboarded_instance(&region, &input.connect_instance_id, now)
        else {
            return Err(
                GetInstanceCommunicationLimitsErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        // The usability sentence, the one this service's wire states for
                        // the instance limits — the write of the same member answers it
                        // too, so the two cannot disagree.
                        message: crate::state::INSTANCE_NOT_FOUND_MESSAGE.to_string(),
```

`onboarded_instance`/`is_onboarded` is only the onboarding half, by its own doc — src/state.rs:2325-2329:

```
/// instance cannot disagree about a request they all refuse. The other half — whether
/// a finished onboarding job has enabled the instance for Campaigns in the request's
/// region — is this service's own store's answer
/// ([`ComAmazonawsConnectcampaignsv2Amazonconnectcampaignservicev2State::is_onboarded`]),
/// and both refuse with the same text.
```

Test coverage, both sides. The refusal-message suite puts the two limits operations in the usability class — tests/all/instance_refusal_messages.rs:15-18:

```
//! * the operations whose wire reports the instance's USABILITY by Campaigns answer the
//!   service's fixed sentence about it, which carries nothing from the request
//!   (`CreateCampaign`, the integration LISTING, and the two on the instance's
//!   communication limits);
```

with rows at tests/all/instance_refusal_messages.rs:133-150 giving both `refusal: Refusal::Instance`. Its "WHY the instance cannot be served" rows (tests/all/instance_refusal_messages.rs:32-34) are "unknown to Amazon Connect, registered but never onboarded, onboarding still in progress, onboarded only in another region, or its configuration deleted since" — in every one of those the onboarding half already fails, which is why the gap is invisible there.

The registry-gate suite, which drives the registry-only case (instance onboarded, then removed from the registry), omits both — tests/all/connect_instance_registry_gate.rs:99-102:

```
/// The class is every operation that needs a USABLE instance: starting an onboarding
/// job, creating a campaign, and the instance-integration operations. The status and the
/// code are the class's; which sentence each one states is a fact of its own wire, and
/// `instance_refusal_messages.rs` tables all of them.
```

and its five rows (tests/all/connect_instance_registry_gate.rs:107-162) are StartInstanceOnboardingJob, CreateCampaign, PutConnectInstanceIntegration, DeleteConnectInstanceIntegration, ListConnectInstanceIntegrations — neither communication-limits operation.

## Steps to Reproduce

1. Register a Connect instance through the Amazon Connect emulator (`CreateInstance`) so the account's registry holds `instance-1`.
2. `PUT /v2/connect-instance/instance-1/onboarding` with `{"encryptionConfig": {"enabled": false}}` and wait out `ONBOARDING_JOB_DURATION` so the job reaches SUCCEEDED.
3. `PUT /v2/connect-instance/instance-1/communication-limits` with a valid `communicationLimitsConfig` — 200, as expected while both halves of usability hold.
4. Drop the instance from the registry with Amazon Connect's `DeleteInstance` (the only call that drops an instance id, per src/state.rs:2317-2318).
5. `PUT /v2/campaigns` naming `connectInstanceId: instance-1`: 404 `ResourceNotFoundException`, message `Connect Instance does not exist or is not enabled to use Campaigns.` — the registry half refusing, via create_campaign.rs:281.
6. `GET /v2/connect-instance/instance-1/integrations`: 404 with the same sentence, via list_connect_instance_integrations.rs:180.
7. `GET /v2/connect-instance/instance-1/communication-limits`: 200 with the stored `communicationLimitsConfig` — the same instance, the same usability class, the opposite answer.
8. `PUT /v2/connect-instance/instance-1/communication-limits` with a different `communicationLimitsConfig`: 200, and the new config is persisted onto the record of an instance the account no longer holds; step 7 repeated now reports it.

Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 6b352a3056b64f5f10a2602a471a448f4bf0b8c1

# The stored-record → reported-provider projection is written twice (state helper and the Get handler's inline copy)

_Filed: 2026-11-18T18:07:08Z_
_Operation: GetRegisteredSubscriptionProvider | Class: maintainability_

> _Note: this issue was identified against the code base at commit `6b352a3056b64f5f10a2602a471a448f4bf0b8c1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The mapping from a stored `SubscriptionProviderRecord` onto the six members a registration is REPORTED with exists twice in this crate:

1. `crate::state::registered_subscription_provider`, the helper `ListRegisteredSubscriptionProviders` projects every page element through; and
2. an inline copy of the same six-member mapping in `GetRegisteredSubscriptionProvider::handle`.

The two output shapes are member-identical (`GetRegisteredSubscriptionProviderOutput` and `RegisteredSubscriptionProvider` declare the same six members with the same types), so the projection is one rule, not two. Having it written twice means a change to how any one member is derived from the record — a `SubscriptionProviderStatusMessage` the service starts reporting, a retrieval time that is formatted rather than cloned, a member the record gains — lands in one copy and not the other, and the two reporting operations then answer the same stored registration differently. That is exactly the invariant this crate's own suite pins (`provider_status_transition.rs`: "every other reporting operation then agrees — the status is a property of the stored record, not of the operation that reported it"), and the state module says the shared lookup/projection helpers live there precisely so no handler grows its own copy.

The fix is mechanical and response-preserving: let `GetRegisteredSubscriptionProvider` build the one projection (`crate::state::registered_subscription_provider(record)`) and move its members into its own output shape, so the record→members rule is stated once. No response body and no existing test changes.

## Evidence

Copy 1 — services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/state.rs:871-884:
```
/// The `RegisteredSubscriptionProvider` the model describes, built entirely from the
/// stored record — every member is what registration persisted.
pub fn registered_subscription_provider(
    record: &SubscriptionProviderRecord,
) -> RegisteredSubscriptionProvider {
    RegisteredSubscriptionProvider {
        subscription_provider_arn: Some(record.arn.clone()),
        subscription_provider_source: Some(record.source.clone()),
        secret_arn: Some(record.secret_arn.clone()),
        subscription_provider_status: Some(record.status.clone()),
        subscription_provider_status_message: record.status_message.clone(),
        last_successful_data_retrieval_time: record.last_successful_data_retrieval_time.clone(),
    }
}
```
(its only caller: `src/ops/list_registered_subscription_providers.rs:198` — `.map(crate::state::registered_subscription_provider)`)

Copy 2 — src/ops/get_registered_subscription_provider.rs:193-202:
```
        Ok(GetRegisteredSubscriptionProviderOutput {
            subscription_provider_arn: Some(record.arn.clone()),
            subscription_provider_source: Some(record.source.clone()),
            secret_arn: Some(record.secret_arn.clone()),
            subscription_provider_status: Some(record.status.clone()),
            subscription_provider_status_message: record.status_message.clone(),
            last_successful_data_retrieval_time: record
                .last_successful_data_retrieval_time
                .clone(),
        })
```

The two targets are member-identical, so one projection serves both:
- src/ops/types/get_registered_subscription_provider_types.rs:22-29 — `pub struct GetRegisteredSubscriptionProviderOutput { subscription_provider_arn, subscription_provider_source, secret_arn, subscription_provider_status, subscription_provider_status_message, last_successful_data_retrieval_time }` (all `Option<String>`)
- src/ops/types/list_registered_subscription_providers_types.rs:31-38 — `pub struct RegisteredSubscriptionProvider { … }` with the same six `Option<String>` members

The rule the duplication can break, stated by the crate itself:
- src/state.rs:5-9 — "One store holds every resource this service's operations share … The lookup / list / filter / pagination helpers live here too, so the operation handlers in `ops/` all resolve a resource the same way instead of each growing its own copy."
- tests/all/provider_status_transition.rs:16-18 — "once the span has passed, WHICHEVER operation reports a status first materializes the transition to `ACTIVE`, and every other reporting operation then agrees — the status is a property of the stored record, not of the operation that reported it"

Existing coverage that must stay green and unchanged: tests/all/provider_status_transition.rs (Get / List / repeated-registration rows), tests/all/subscription_providers.rs, tests/all/subscription_provider_registration.rs, tests/all/error_taxonomy.rs (the `GetRegisteredSubscriptionProvider` rows).

## Steps to Reproduce

1. Open services/service-com-amazonaws-licensemanagerlinuxsubscriptions-licensemanagerlinuxsubscriptions/src/state.rs and read `registered_subscription_provider` at lines 871-884: six members projected off `SubscriptionProviderRecord`.
2. Open src/ops/get_registered_subscription_provider.rs and read the handler's `Ok(GetRegisteredSubscriptionProviderOutput { … })` at lines 193-202: the same six members projected off the same record, by hand.
3. Compare src/ops/types/get_registered_subscription_provider_types.rs:22-29 with src/ops/types/list_registered_subscription_providers_types.rs:31-38 — the two shapes declare an identical member set, so the two projections are copies of one rule rather than two different mappings.
4. Confirm the helper has exactly one caller (`grep -n registered_subscription_provider src/ops/`): list_registered_subscription_providers.rs:198 — the Get path does not go through it, so a change to the helper reaches only ListRegisteredSubscriptionProviders.

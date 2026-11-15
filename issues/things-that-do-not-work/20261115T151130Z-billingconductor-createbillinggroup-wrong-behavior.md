Issue-Severity: HIGH
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1023678a2f4d4b5afbe27b0b7b8a4a94baea46e4

# Billing-group ids are minted from a base-36 alphabet, so most groups carry an ARN the tagging surface can never address and the Tags the create stored are unreachable

_Filed: 2026-11-15T15:11:30Z_
_Operation: CreateBillingGroup | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1023678a2f4d4b5afbe27b0b7b8a4a94baea46e4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateBillingGroup` mints the group's resource id with `mint_resource_id(12)`, which renders the shared id counter in the base-36 alphabet `0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ`. It then PERSISTS the caller's `Tags` on that group record.

The tagging surface of this same crate cannot address such a group. The modelled tagging `Arn` spells a billing group `billinggroup/?[0-9]{12}` — DIGITS only — and this crate's own `src/state/tagging.rs` quotes that spelling as the one the surface addresses. A request whose `ResourceArn` breaks the modelled pattern is refused 400 `ValidationException` by `PreHandlerChecks` before any handler runs (`src/service.rs:417`), so for any group whose minted id contains a letter there is NO spelling of its ARN that `TagResource`, `UntagResource` or `ListTagsForResource` will accept. The tags `CreateBillingGroup` wrote onto the record are then unreadable and unremovable for the life of the group — exactly the outcome `src/state.rs:4174-4179` says the shared tag dispatch exists to prevent ("a kind left unresolved would make the tags its create stored unreadable").

The id counter is shared by every resource kind (`mint_resource_id` increments one `id_counter`), so the 10th resource minted in a store already gets the id `00000000000A`. The crate's own tests do not pin the broken behaviour — they ORDER their setup to avoid it, and say so: the billing group is created "early so its minted id is still all digits".

The three other taggable kinds are unaffected: the tagging pattern spells them `[a-zA-Z0-9]{10}`.

## Evidence

The mint and the tag persistence, `src/ops/create_billing_group.rs:424-427` and `:441`:

        // 12 characters, within the `[a-zA-Z0-9]{10,12}` the `BillingGroupArn`
        // pattern allows.
        let id = state.mint_resource_id(12);
        let arn = crate::state::billing_group_arn(&id);
    ...
            tags: input.tags.unwrap_or_default().into_iter().collect(),

The alphabet it mints from, `src/state.rs:38-39`:

    /// The alphabet the modeled resource-id patterns allow (`[a-zA-Z0-9]`).
    const ID_ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

and `src/state.rs:3060-3069`, which renders the SHARED counter in it:

    pub fn mint_resource_id(&mut self, width: usize) -> String {
        self.id_counter += 1;
        let mut value = self.id_counter;
        let mut id = vec![b'0'; width];
        for slot in id.iter_mut().rev() {
            let alphabet_len = u64::try_from(ID_ALPHABET.len()).unwrap_or(36);
            *slot = ID_ALPHABET[usize::try_from(value % alphabet_len).unwrap_or(0)];

The model's tagging `Arn` (smithy_query query="shape" shape="Arn"):

    "pattern": "^arn:aws(-cn)?:billingconductor::[0-9]{12}:billinggroup/?[0-9]{12}$|^arn:aws(-cn)?:billingconductor::[0-9]{12}:pricingplan/[a-zA-Z0-9]{10}$|..."

which the crate itself restates, `src/state/tagging.rs:168-172`:

    /// The resource kinds this service's tagging surface addresses, as the resource
    /// segment of the modelled tagging `Arn` spells them
    /// (`smithy_query(query="operation",operation="TagResource")`:
    /// `billinggroup/?[0-9]{12}`, `pricingplan/[a-zA-Z0-9]{10}`,

The refusal happens before the handler, `src/service.rs:417-426`:

            if let Some(exception) = modelled_refusal::<Op>(protocol, req) {
                ...
                match protocol.serialize_error(
                    &exception,
                    &crate::ops::types::SCHEMA_VALIDATION_EXCEPTION,

The rule this breaks, `src/state.rs:4174-4179`:

    /// Tags live ON the resource record (the same map `CreateBillingGroup` /
    /// `CreatePricingPlan` / `CreatePricingRule` / `CreateCustomLineItem` persisted their
    /// `Tags` into), so there is one copy of a resource's tags and a delete reclaims them
    /// with the resource. EVERY taggable kind this service persists is resolved by the one
    /// dispatch both halves of the tagging surface share (`resource_tags_of!`): a kind
    /// left unresolved would make the tags its create stored unreadable.

Test coverage avoids the case instead of covering it — `tests/all/shared_tag_dispatch.rs:47-48`:

    // The billing group is created early so its minted id is still all digits: the
    // tagging `Arn` pattern spells a billing group `billinggroup/?[0-9]{12}`.

and `tests/all/tagging_surface.rs:384-385`:

    // Created before the billing group so the group's minted id is still all digits:
    // the tagging `Arn` pattern spells a billing group `billinggroup/?[0-9]{12}`.

No test tags a billing group minted after the ninth resource.

## Steps to Reproduce

1. Start the emulator with the emulated account designated the payer account (as the suites do: `engine.state::<BillingConductorState>().lock().payer_account_id = Some(emulator_account_id())`).
2. POST /create-pricing-plan nine times (any names) so the shared id counter reaches 9.
3. POST /create-billing-group with `{Name:"G", AccountGrouping:{LinkedAccountIds:[]}, ComputationPreference:{PricingPlanArn:"arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan"}, Tags:{"stage":"test"}}` -> HTTP 200 with `Arn = arn:aws:billingconductor::<account>:billinggroup/00000000000A`; the tags are stored on the record.
4. GET /tags/arn%3Aaws%3Abillingconductor%3A%3A<account>%3Abillinggroup%2F00000000000A -> HTTP 400 ValidationException (the `ResourceArn` breaks the modelled `billinggroup/?[0-9]{12}` pattern), not the stored `{"stage":"test"}`.
5. POST /tags/<same encoded ARN> with `{"Tags":{"team":"finance"}}` and DELETE /tags/<same encoded ARN>?tagKeys=stage -> HTTP 400 as well, so the tags this create persisted can never be read, extended or removed.
6. Compare with the same call made for the FIRST billing group created in a fresh store (id `000000000001`), which the tagging surface serves — the only difference is the id the service minted.

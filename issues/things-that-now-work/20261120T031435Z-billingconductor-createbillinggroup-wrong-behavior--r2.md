Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 48b4999bcfc6fe53ee977f3df6a77f3d1baf018d

# AccountGrouping.AutoAssociate is persisted and gated but never associates a newly joined account, contrary to the rule the crate's own comment states for the flag

_Filed: 2026-11-20T03:14:35Z_
_Operation: CreateBillingGroup | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `48b4999bcfc6fe53ee977f3df6a77f3d1baf018d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`AccountGrouping.AutoAssociate` is stored by `CreateBillingGroup` (and `UpdateBillingGroup`), is reported back by the listings, bounds how many groups may hold it, and blocks a delete of a holder — but nothing in the crate ever acts on it. The crate's own doc comment on the one reader of the flag says it means the group "automatically associates the accounts newly joining the consolidated billing family" and then enumerates every rule that turns on it: "the state-wide cap on holders, the refusal to delete a holder". Neither of those associates anything.

The consequence is visible on the read side. The crate DOES read the consolidated billing family's roster from the Organizations store (`organization_linked_accounts`), and the one state method that answers "which billing group was this linked account in" hands back `None` for every family account no billing group has ever held — without consulting any group's `auto_associate`. So with a billing group that has `AutoAssociate: true`, an account that joins the family afterwards is reported by `ListAccountAssociations` with no `BillingGroupArn` (i.e. `UNMONITORED`), is not counted in that group's `Size`, and contributes nothing to its cost report, although the flag the group holds says it is associated automatically.

This is not a missing helper: `CreateBillingGroup` already associates a group's accounts through `state.associate_account(&account_id, &arn)`, and the roster is already read at exactly the point where a newly joined account surfaces.

## Evidence

The modelled behaviour of the member, stated by the model itself — `smithy_query(query="doc",shape="AccountGrouping",member="AutoAssociate")`:

    "Specifies if this billing group will automatically associate newly added Amazon Web Services accounts that join your consolidated billing family."

The crate restates that rule, and then names the only two rules that read the flag — `src/state.rs:1727-1734`:

    /// True when this group automatically associates the accounts newly joining the
    /// consolidated billing family — `AccountGrouping.AutoAssociate`, where an absent
    /// flag is the default `false`. One reading of the flag, so every rule that turns
    /// on it (the state-wide cap on holders, the refusal to delete a holder) decides
    /// from the same answer.
    pub fn auto_associates(&self) -> bool {
        self.auto_associate.unwrap_or(false)
    }

The only two call sites are exactly those gates — `src/ops/create_billing_group.rs:392-394` (`let holders = state.other_auto_associate_billing_groups("");`) and `src/ops/delete_billing_group.rs:196` (`if group.auto_associates() {`). `src/state.rs:4245-4251` (`other_auto_associate_billing_groups`) only counts holders.

The roster reader that decides an account's group makes no mention of the flag, and states the outcome plainly — `src/state.rs:3367-3400`:

    /// Every linked account this service can report, in account-id order, each with
    /// the billing group it was associated with IN `billing_period` (`None` when it
    /// was in none then).
    ...
    /// billing group then, so one that joined a group later is reported for an earlier
    /// period with no group at all, and a family account no group has ever held is reported
    /// with none too.
    pub fn linked_accounts(
    ...
            .map(|(account_id, _)| (account_id, None))

while the helper that WOULD make the membership exists and is already used by the create for the accounts the request itself names — `src/ops/create_billing_group.rs:455-457`:

        for account_id in linked_account_ids {
            state.associate_account(&account_id, &arn);
        }

Test coverage: `tests/all/auto_associate_billing_group_cap.rs:6-21` pins ONLY the cap ("at most one stored group may have it enabled at a time"), `tests/all/delete_auto_associate_billing_group.rs:7` only the delete refusal, and `tests/all/update_billing_group.rs:400-410,543-545` only that the flag is persisted and reported back. No test asserts that a family account stays out of the electing group, so nothing pins the current inertness.

## Steps to Reproduce

1. Start the emulator with the emulated account designated the payer account (as the suites do: `engine.state::<BillingConductorState>().lock().payer_account_id = Some(emulator_account_id())`) and a consolidated billing family roster holding account 111111111111.
2. POST /create-pricing-plan, then POST /create-billing-group with `{"Name":"Auto","AccountGrouping":{"LinkedAccountIds":[],"AutoAssociate":true},"ComputationPreference":{"PricingPlanArn":"arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan"}}` -> 200 with an Arn.
3. Add account 222222222222 to the Organizations roster (the family the emulator reports), i.e. an account that joins the consolidated billing family AFTER the electing group was created.
4. POST /list-account-associations with `{}`.
5. Observed: the element for 222222222222 carries no `BillingGroupArn` (reported as unassociated), and POST /list-billing-groups reports `Size: 0` for the electing group. Expected per the member's own documentation and `src/state.rs:1727-1731`: the newly joined account is associated to the group that holds `AutoAssociate: true`.

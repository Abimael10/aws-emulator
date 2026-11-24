Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 816efa24d8fe12d231f3da407667caac95adf4e8

# Every custom line item version reports the ITEM's creation instant as CreationTime, where the member is modelled as the time the VERSION was created

_Filed: 2026-11-21T12:30:12Z_
_Operation: ListCustomLineItemVersions | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `816efa24d8fe12d231f3da407667caac95adf4e8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListCustomLineItemVersions` renders each reported generation's `CreationTime` from `CustomLineItemVersion::creation_time_millis`, which the state module holds as the custom line item's own creation instant and copies unchanged into every generation. So an item written three times reports the SAME `CreationTime` on all three versions — the instant the item was created — and no version ever reports the instant that version came into being.

The member's own Smithy documentation states the opposite: `CustomLineItemVersionListElement$CreationTime` is "The time when the custom line item version was created." The element already carries the instant each generation took effect separately (`StartTime`, filled from `version.start_time_millis`, which `put_custom_line_item` sets to the instant of the write that opened the generation), so the crate HAS the per-version instant and chooses to report the item's instead.

The divergence is per-member and has no other effect: the cursor key of this listing is the generation's positional index (`format!("{index:010}")`), the filter reads `spans`, and `StartBillingPeriod`/`EndBillingPeriod` are derived from the spans, so only the `CreationTime` a client reads is affected.

## Evidence

Smithy model — `smithy_query(query="doc", shape="CustomLineItemVersionListElement", member="CreationTime")`, verbatim:

    "<p>The time when the custom line item version was created.</p>"

and for the sibling member that the crate DOES fill per generation, `smithy_query(query="shape", shape="CustomLineItemVersionListElement")`:

    "StartTime": {"target": "Instant", "default": 0, "type": "long", "doc": "<p> The inclusive start time. </p>"}

The handler, `services/service-com-amazonaws-billingconductor-awsbillingconductor/src/ops/list_custom_line_item_versions.rs:206-212`:

        creation_time: version.creation_time_millis,
        last_modified_time: version.last_modified_time_millis,
        association_size: item.association_size(&start_billing_period),
        start_billing_period: Some(start_billing_period),
        end_billing_period,
        arn: Some(item.arn.clone()),
        start_time: version.start_time_millis,

What `version.creation_time_millis` holds — `services/service-com-amazonaws-billingconductor-awsbillingconductor/src/state.rs:2620-2621` (field doc on `CustomLineItemVersion`):

    /// The item's `CreationTime`, which every generation of it shares.
    pub creation_time_millis: i64,

and where it comes from — `src/state.rs:2486-2498` (`CustomLineItemRecord::current_version`), which every superseded generation is frozen from:

    pub fn current_version(&self) -> CustomLineItemVersion {
        CustomLineItemVersion {
            ...
            creation_time_millis: self.creation_time_millis,
            last_modified_time_millis: self.last_modified_time_millis,
            start_time_millis: self.version_start_time_millis,
        }
    }

The per-generation instant the crate already has, set by the write that opens a generation — `src/state.rs:3816-3818` (`put_custom_line_item`):

            if replaced.applies_anywhere() {
                kept.push(replaced);
                updated.version_start_time_millis = now;
            }

Test coverage that currently pins the item-wide value, as an assertion restating the behaviour rather than giving a reason to depart from the modelled sentence — `tests/all/list_custom_line_item_versions.rs:838-843`:

        assert_eq!(
            pluck(&reported[0], "CreationTime"),
            pluck(&reported[1], "CreationTime"),
            "case {case:?}: expected every version to share the item's creation time, got \
             {reported:?}"
        );

(`tests/all/update_custom_line_item.rs:417` and `:1308` read `CreationTime` off the item itself, not off a version, so they do not cover this member.)

## Steps to Reproduce

1. Start the emulator with the emulated account designated the payer account (as the test harnesses do: `engine.state::<BillingConductorState>().lock().payer_account_id = Some(emulator_account_id())`).
2. POST /create-billing-group with `ComputationPreference.PricingPlanArn = arn:aws:billingconductor::aws:pricingplan/BasicPricingPlan`; keep the group ARN G.
3. POST /create-custom-line-item with `Name="Item"`, `BillingGroupArn=G`, `ChargeDetails={Type:FEE, Flat:{ChargeValue:10}}` and NO BillingPeriodRange; keep the ARN C. Note the response time T0.
4. Wait so a second write lands at a later instant, then POST /update-custom-line-item with `{Arn:C, Description:"second generation", BillingPeriodRange:{InclusiveStartBillingPeriod:<current>}}` -> HTTP 200 (this freezes the first generation and opens a second at instant T1 > T0).
5. POST /list-custom-line-item-versions with `{Arn:C}` -> two `CustomLineItemVersions` elements.
6. Compare each element's `CreationTime` with its `StartTime`: both elements carry the SAME `CreationTime` (T0, the item's creation instant), while their `StartTime` values differ (T0 and T1). The newer version therefore reports a `CreationTime` that is not "the time when the custom line item version was created", which is what the member's modelled documentation states.

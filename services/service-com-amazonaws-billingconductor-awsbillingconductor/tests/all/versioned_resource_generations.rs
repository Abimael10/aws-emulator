// SPDX-License-Identifier: CC0-1.0

//! The billing-period versioning rule, driven once over EVERY resource that carries it,
//! end to end over the service's real `restJson1` wire through
//! [`MultiServiceRouter::dispatch`] — the same path the production router serves.
//!
//! RULE: a pricing plan, a pricing rule and a billing group are each read for a BILLING
//! PERIOD, so each keeps the generations of itself that later writes superseded and a read
//! reports the generation in force in the period asked about — the latest one whose own
//! start period is at or before it, and none at all for a period before the resource was
//! created. A write takes effect in the period it is made in: one landing in a period
//! after the current generation's own start period closes that generation, one landing in
//! that same period replaces it, and past the bounded number of superseded generations the
//! store keeps the OLDEST is dropped, which reads back as the oldest kept generation
//! answering for the periods before it — never as a resource that vanishes from a period
//! it existed in.
//!
//! That rule has ONE implementation for the three resources, so this suite drives it
//! through an operation of each: `UpdatePricingPlan` / `ListPricingPlans`,
//! `UpdatePricingRule` / `ListPricingRules`, `UpdateBillingGroup` / `ListBillingGroups`,
//! and `AssociatePricingRules`, which stores a plan the same way. The sibling suites
//! `tests/all/pricing_plan_billing_periods.rs`,
//! `tests/all/pricing_rule_billing_periods.rs` and
//! `tests/all/billing_group_billing_periods.rs` own each resource's own members in depth;
//! this one is about the rule the three share, asserted over all of them at once.
//!
//! The rows differ along what the rule generalises over: which versioned resource is
//! written (all three), where the period read falls among the generations (before the
//! resource existed, each generation's own period, the month after it, the current one and
//! a far-future one), and where it falls relative to the kept-history bound (a period
//! whose generation has been dropped, the oldest kept one, the newest superseded one and
//! the current one).
//!
//! The wire cannot write in a PAST billing period — a write happens now — so a row writes
//! through the real wire and then backdates the stored instants, which is the one thing
//! the test cannot ask the service for. That is the harness
//! `tests/all/pricing_rule_association_periods.rs` and
//! `tests/all/billing_group_billing_periods.rs` already carry, driven from here.
//!
//! The rule's other half is also shared, and this suite drives it over the same resources:
//! whether a write MODIFIED a resource at all is decided by comparing the values a read of
//! it reports — exactly the members one generation of it carries — so a request naming only
//! values the resource already reports moves no `LastModifiedTime` and opens no generation,
//! whichever resource it addresses and whichever of those members it names.

use serde_json::{json, Value};
use service_com_amazonaws_billingconductor_awsbillingconductor::state::{
    MAX_BILLING_GROUP_VERSIONS, MAX_PRICING_PLAN_VERSIONS, MAX_PRICING_RULE_VERSIONS,
};

use crate::billing_group_billing_periods::{
    assert_members, current_period, primary_account_id, Emulator as GroupEmulator,
};
use crate::pricing_rule_association_periods::{
    period_start_millis, shift_period, Emulator as PlanEmulator,
};

/// The resources whose reads are scoped to a billing period — the ones the shared rule
/// covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Versioned {
    PricingPlan,
    PricingRule,
    BillingGroup,
}

impl Versioned {
    /// Every versioned resource of this store, which is what each table below spans.
    const ALL: [Versioned; 3] = [Self::PricingPlan, Self::PricingRule, Self::BillingGroup];

    /// How many SUPERSEDED generations of one such resource the store keeps — the
    /// per-resource bound the shared write rule enforces, named from the store's own
    /// constants rather than restated here.
    fn kept_generations(self) -> usize {
        match self {
            Self::PricingPlan => MAX_PRICING_PLAN_VERSIONS,
            Self::PricingRule => MAX_PRICING_RULE_VERSIONS,
            Self::BillingGroup => MAX_BILLING_GROUP_VERSIONS,
        }
    }
}

/// The two harnesses this suite drives, so one row can write and read any of the three
/// versioned resources: a pricing plan and a pricing rule live in the association-period
/// harness, a billing group in the billing-group one.
struct World {
    /// Writes and backdates a pricing plan and a pricing rule.
    pricing: PlanEmulator,
    /// Writes and backdates a billing group.
    billing: GroupEmulator,
    /// The pricing plan a billing group of this world computes with — a group's
    /// computation preference has to name a plan that exists.
    group_plan: String,
}

impl World {
    async fn new() -> Self {
        let billing = GroupEmulator::new();
        let group_plan = billing.create_plan("GenerationsGroupPlan").await;
        Self {
            pricing: PlanEmulator::new(),
            billing,
            group_plan,
        }
    }

    /// Create the `index`-th resource of `kind` carrying `description`, and answer the ARN
    /// the service minted.
    async fn create(&self, kind: Versioned, index: usize, description: &str) -> String {
        let name = format!("Generations{index}");
        match kind {
            Versioned::PricingPlan => {
                let (status, answer) = self
                    .pricing
                    .post(
                        "/create-pricing-plan",
                        &json!({"Name": name, "Description": description}),
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "setup CreatePricingPlan {name:?}: expected 200, got {status} with {answer:?}"
                );
                answer["Arn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("setup CreatePricingPlan {name:?}: no Arn"))
                    .to_string()
            }
            Versioned::PricingRule => {
                crate::pricing_rule_billing_periods::create_rule(
                    &self.pricing,
                    &name,
                    &json!({"Name": name, "Scope": "GLOBAL", "Type": "MARKUP",
                            "ModifierPercentage": 10.0, "Description": description}),
                )
                .await
            }
            Versioned::BillingGroup => {
                self.billing
                    .create_group(
                        &name,
                        &self.group_plan,
                        &primary_account_id(index),
                        &json!({"Description": description}),
                    )
                    .await
            }
        }
    }

    /// Backdate the resource's creation — and the only generation it has yet — to
    /// `period`, the way a world where it already existed in a closed period is set up.
    fn created_in(&self, kind: Versioned, arn: &str, period: &str) {
        match kind {
            Versioned::PricingPlan => self.pricing.create_plan_in(arn, period),
            Versioned::PricingRule => self.pricing.create_rule_in(arn, period),
            Versioned::BillingGroup => self.billing.created_in(arn, period),
        }
    }

    /// Backdate the resource's CURRENT generation to `period`, so the next write through
    /// the wire lands in a LATER billing period and therefore closes it.
    fn generation_started_in(&self, kind: Versioned, arn: &str, period: &str) {
        match kind {
            Versioned::PricingPlan => self.pricing.restate_plan_generation_in(arn, period),
            Versioned::PricingRule => self.pricing.restate_generation_in(arn, period),
            Versioned::BillingGroup => self.billing.generation_started_in(arn, period),
        }
    }

    /// The write under test: each resource's own update operation, every one of which
    /// stores through the shared write rule. `Description` is the one member all three
    /// updates carry, so one row's write is the same change whichever resource it is made
    /// to.
    async fn describe(&self, case: &str, kind: Versioned, arn: &str, description: &str) {
        match kind {
            Versioned::PricingPlan => {
                crate::pricing_plan_billing_periods::update_plan(
                    &self.pricing,
                    case,
                    arn,
                    &json!({"Description": description}),
                )
                .await;
            }
            Versioned::PricingRule => {
                let (status, answer) = self
                    .pricing
                    .put(
                        "/update-pricing-rule",
                        &json!({"Arn": arn, "Description": description}),
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case}: setup UpdatePricingRule expected 200, got {status} with \
                     {answer:?}"
                );
            }
            Versioned::BillingGroup => {
                self.billing
                    .update_group(case, arn, &json!({"Description": description}))
                    .await;
            }
        }
    }

    /// The list element the resource's period-scoped listing reports for `arn` in `period`
    /// (`None` names no period at all), or `None` when the listing does not report it for
    /// that period at all.
    async fn element_in(
        &self,
        case: &str,
        kind: Versioned,
        arn: &str,
        period: Option<&str>,
    ) -> Option<Value> {
        match kind {
            Versioned::PricingPlan => {
                crate::pricing_plan_billing_periods::element_in(&self.pricing, case, arn, period)
                    .await
            }
            Versioned::PricingRule => {
                crate::pricing_rule_billing_periods::element_in(&self.pricing, case, arn, period)
                    .await
            }
            Versioned::BillingGroup => self.billing.element_in(case, arn, period).await,
        }
    }
}

#[tokio::test]
async fn every_versioned_resource_reports_the_generation_in_force_in_the_period_read() {
    let world = World::new().await;
    let now = current_period();
    // Three generations: the create, a write a year later, and a write made NOW. The first
    // two are backdated, because the wire can only write in the current period.
    let first = shift_period(&now, -24);
    let second = shift_period(&now, -12);

    for (index, kind) in Versioned::ALL.iter().copied().enumerate() {
        let case = format!("{kind:?}");
        let arn = world.create(kind, index, "first").await;
        world.created_in(kind, &arn, &first);
        world.describe(&case, kind, &arn, "second").await;
        world.generation_started_in(kind, &arn, &second);
        world.describe(&case, kind, &arn, "third").await;

        // Rows differ along where the period falls among the generations: before the
        // resource existed, the first generation's own period and the month after it, the
        // second generation's period and the month after it, the current period, and a
        // far-future one.
        let rows: Vec<(String, Option<Value>)> = vec![
            (shift_period(&first, -13), None),
            (shift_period(&first, -1), None),
            (
                first.clone(),
                Some(json!({"Description": "first",
                            "LastModifiedTime": period_start_millis(&first)})),
            ),
            (
                shift_period(&first, 1),
                Some(json!({"Description": "first",
                            "LastModifiedTime": period_start_millis(&first)})),
            ),
            (
                second.clone(),
                Some(json!({"Description": "second",
                            "LastModifiedTime": period_start_millis(&second)})),
            ),
            (
                shift_period(&second, 1),
                Some(json!({"Description": "second",
                            "LastModifiedTime": period_start_millis(&second)})),
            ),
            (now.clone(), Some(json!({"Description": "third"}))),
            (shift_period(&now, 13), Some(json!({"Description": "third"}))),
        ];

        for (period, expected) in rows {
            let case = format!("{case} read at {period:?}");
            let reported = world.element_in(&case, kind, &arn, Some(&period)).await;
            match (&expected, &reported) {
                (Some(expected), Some(element)) => {
                    assert_members(&case, element, expected);
                    // The members no write can change belong to every generation of the
                    // resource, so they are the same in every period.
                    assert_eq!(
                        element["Arn"].as_str(),
                        Some(arn.as_str()),
                        "case {case}: expected the resource's own ARN, got {element:?}"
                    );
                    assert_eq!(
                        element["CreationTime"].as_i64(),
                        Some(period_start_millis(&first)),
                        "case {case}: expected the creation instant in every period, got \
                         {element:?}"
                    );
                }
                (None, None) => {}
                _ => panic!(
                    "case {case}: a resource is reported exactly for the periods from its \
                     creation onward, expected {expected:?}, got {reported:?}"
                ),
            }
        }

        // A read that names no period asks about the current one, so it answers with the
        // newest generation.
        let case_now = format!("{case} read with no period");
        let element = world
            .element_in(&case_now, kind, &arn, None)
            .await
            .unwrap_or_else(|| panic!("case {case_now}: the resource exists now, so it is reported"));
        assert_members(&case_now, &element, &json!({"Description": "third"}));

        // The reads changed nothing a later caller sees: the earliest period still reports
        // the generation it reported before them.
        let case_again = format!("{case} re-read at {first:?}");
        let element = world
            .element_in(&case_again, kind, &arn, Some(&first))
            .await
            .unwrap_or_else(|| {
                panic!("case {case_again}: a read must not change what a later caller sees")
            });
        assert_members(
            &case_again,
            &element,
            &json!({"Description": "first", "LastModifiedTime": period_start_millis(&first)}),
        );
    }
}

#[tokio::test]
async fn the_kept_generations_of_every_versioned_resource_are_bounded_the_same_way() {
    let world = World::new().await;
    let now = current_period();

    for (index, kind) in Versioned::ALL.iter().copied().enumerate() {
        let case = format!("{kind:?}");
        let cap = kind.kept_generations();
        // THREE more period-crossing writes than the store keeps generations, so the
        // oldest three are dropped and the periods they covered fall back to the oldest
        // generation still kept.
        let total = cap + 3;
        let steps = i64::try_from(total).expect("the version bound fits in an i64");
        let creation = shift_period(&now, -steps);

        let arn = world.create(kind, index, "generation-0").await;
        world.created_in(kind, &arn, &creation);
        // Each write lands in the current period, which is later than the period its
        // predecessor's generation was backdated to, so every one of them closes a
        // generation; the backdating that follows it moves that new generation one month
        // on, leaving one generation per month from the creation to now.
        for step in 1..=total {
            let left = i64::try_from(total - step).expect("the version bound fits in an i64");
            world
                .describe(&case, kind, &arn, &format!("generation-{step}"))
                .await;
            world.generation_started_in(kind, &arn, &shift_period(&now, -left));
        }

        // The oldest generation still kept answers for every period whose own generation
        // the bound dropped — the resource never vanishes from a period it existed in.
        let oldest_kept = total - cap;
        let rows: Vec<(String, String)> = vec![
            (creation.clone(), format!("generation-{oldest_kept}")),
            (
                shift_period(&creation, 1),
                format!("generation-{oldest_kept}"),
            ),
            (
                shift_period(&now, -steps + 2),
                format!("generation-{oldest_kept}"),
            ),
            (
                shift_period(&now, -steps + 3),
                format!("generation-{oldest_kept}"),
            ),
            (
                shift_period(&now, -steps + 4),
                format!("generation-{}", oldest_kept + 1),
            ),
            (
                shift_period(&now, -1),
                format!("generation-{}", total - 1),
            ),
            (now.clone(), format!("generation-{total}")),
            (shift_period(&now, 13), format!("generation-{total}")),
        ];

        for (period, description) in rows {
            let case = format!("{case} with {total} generations read at {period:?}");
            let element = world
                .element_in(&case, kind, &arn, Some(&period))
                .await
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: the resource existed in that period, so a bounded \
                         history still reports it"
                    )
                });
            assert_members(&case, &element, &json!({"Description": description}));
        }
    }
}

#[tokio::test]
async fn an_association_write_keeps_the_pricing_plan_generation_it_closes() {
    // `AssociatePricingRules` changes only what a plan associates, and stores the plan
    // through the same write rule as every other versioned resource — so it closes the
    // generation it displaces rather than overwriting it.
    let world = World::new().await;
    let now = current_period();
    let first = shift_period(&now, -24);

    let rule = world.pricing.create_rule("AssociationGenerationsRule").await;
    let plan = world.create(Versioned::PricingPlan, 7, "first").await;
    world.created_in(Versioned::PricingPlan, &plan, &first);
    world.created_in(Versioned::PricingRule, &rule, &first);

    world.pricing.associate(&plan, std::slice::from_ref(&rule)).await;

    // The period the closed generation was in force in still reports the plan as it stood
    // then, with no association yet, and the current period reports the one the call made.
    for (period, size) in [(first.clone(), 0_i64), (now.clone(), 1)] {
        let case = format!("an association read at {period:?}");
        let element = crate::pricing_plan_billing_periods::element_in(
            &world.pricing,
            &case,
            &plan,
            Some(&period),
        )
        .await
        .unwrap_or_else(|| panic!("case {case}: the plan existed then, so it is reported"));
        assert_members(&case, &element, &json!({"Description": "first", "Size": size}));
    }

    // The association moved the plan's `LastModifiedTime` into the period it was made in,
    // which is what made the generation above one the write had to freeze.
    let case = format!("an association read at {now:?}");
    let element =
        crate::pricing_plan_billing_periods::element_in(&world.pricing, &case, &plan, Some(&now))
            .await
            .unwrap_or_else(|| panic!("case {case}: the plan exists now, so it is reported"));
    let modified_now = element["LastModifiedTime"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case}: no LastModifiedTime in {element:?}"));
    let generation_start = period_start_millis(&first);
    assert!(
        modified_now > generation_start,
        "case {case}: expected the association to move LastModifiedTime past \
         {generation_start}, got {modified_now}"
    );
}

/// The values a row of the billing-group member table names: the group's own name and the
/// two pricing plans of the world it is written in, so a row can restate exactly what the
/// group already reports.
struct Reported {
    /// The name the group was created with.
    name: String,
    /// The pricing plan the group computes with.
    plan: String,
    /// A second pricing plan, the one a changing write moves the group to.
    other_plan: String,
}

/// One row of the billing-group member table: one member class of the group's generation,
/// with the value the group already reports for it and a value it does not.
struct MemberRow {
    label: &'static str,
    /// The members `CreateBillingGroup` is given beyond the harness defaults, so the group
    /// starts out reporting the value the row restates.
    created: fn(&Reported) -> Value,
    /// The members the call under test names — the SAME values the group already reports.
    restated: fn(&Reported) -> Value,
    /// The same member class, with a value the group does not report.
    changed: fn(&Reported) -> Value,
}

/// The name of the `index`-th group of the member table.
fn member_group_name(index: usize) -> String {
    format!("NoOpMember{index}")
}

/// The member classes of a billing group's generation — the members
/// `UpdateBillingGroupInput` can change
/// (`smithy_query(query="shape",shape="UpdateBillingGroupInput")`: `Name`, `Description`,
/// `ComputationPreference.PricingPlanArn`, `Status`,
/// `AccountGrouping.AutoAssociate`), one row each: a name under a pattern, free text, an
/// ARN, an enum value and a boolean.
fn member_rows() -> Vec<MemberRow> {
    vec![
        MemberRow {
            label: "Name",
            created: |_| json!({}),
            restated: |reported| json!({"Name": reported.name}),
            changed: |_| json!({"Name": "NoOpMemberRenamed"}),
        },
        MemberRow {
            label: "Description",
            created: |_| json!({"Description": "as created"}),
            restated: |_| json!({"Description": "as created"}),
            changed: |_| json!({"Description": "moved on"}),
        },
        MemberRow {
            label: "ComputationPreference.PricingPlanArn",
            created: |_| json!({}),
            restated: |reported| {
                json!({"ComputationPreference": {"PricingPlanArn": reported.plan}})
            },
            changed: |reported| {
                json!({"ComputationPreference": {"PricingPlanArn": reported.other_plan}})
            },
        },
        MemberRow {
            label: "Status",
            created: |_| json!({}),
            restated: |_| json!({"Status": "ACTIVE"}),
            changed: |_| json!({"Status": "PENDING"}),
        },
        MemberRow {
            label: "AccountGrouping.AutoAssociate",
            created: |_| json!({"AccountGrouping": {"AutoAssociate": false}}),
            restated: |_| json!({"AccountGrouping": {"AutoAssociate": false}}),
            changed: |_| json!({"AccountGrouping": {"AutoAssociate": true}}),
        },
    ]
}

#[tokio::test]
async fn a_write_naming_only_the_values_a_resource_reports_opens_no_generation() {
    let world = World::new().await;
    let now = current_period();
    // Each resource and its only generation live a year back, so the write under test lands
    // in a LATER billing period than the generation it would displace — the condition under
    // which both consequences of the decision (the instant and the generation) are
    // observable at once.
    let created_in = shift_period(&now, -12);
    let created_at = period_start_millis(&created_in);

    // First over every versioned resource, through its own update operation: `Description`
    // is the one member all three carry, so one row is the same restatement whichever
    // resource it addresses.
    for (index, kind) in Versioned::ALL.iter().copied().enumerate() {
        let case = format!("{kind:?} restating the Description it reports");
        let arn = world.create(kind, index, "as created").await;
        world.created_in(kind, &arn, &created_in);
        world.describe(&case, kind, &arn, "as created").await;

        // Nothing moved: the period the generation started in and the current one are still
        // answered from that one generation, with the instant it started at.
        for period in [&created_in, &now] {
            let case = format!("{case} read at {period:?}");
            let element = world
                .element_in(&case, kind, &arn, Some(period))
                .await
                .unwrap_or_else(|| {
                    panic!("case {case}: the resource existed then, so it is reported")
                });
            assert_members(
                &case,
                &element,
                &json!({"Description": "as created", "LastModifiedTime": created_at}),
            );
        }

        // The control: a write naming a value the resource does NOT report moves the instant
        // and freezes the generation the reads above were answered from.
        let case = format!("{kind:?} naming a Description it does not report");
        world.describe(&case, kind, &arn, "moved on").await;
        let frozen = world
            .element_in(&case, kind, &arn, Some(&created_in))
            .await
            .unwrap_or_else(|| panic!("case {case}: the resource existed then, so it is reported"));
        assert_members(
            &case,
            &frozen,
            &json!({"Description": "as created", "LastModifiedTime": created_at}),
        );
        let current = world
            .element_in(&case, kind, &arn, Some(&now))
            .await
            .unwrap_or_else(|| panic!("case {case}: the resource exists now, so it is reported"));
        assert_members(&case, &current, &json!({"Description": "moved on"}));
        let modified = current["LastModifiedTime"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case}: no LastModifiedTime in {current:?}"));
        assert!(
            modified > created_at,
            "case {case}: expected a write that modified the resource to move \
             LastModifiedTime past {created_at}, got {modified}"
        );
    }

    // Then over EVERY member class a billing group's generation carries, because that is
    // the resource whose update names the longest member list: one restating write and one
    // changing write per member, through the real wire.
    let other_plan = world.billing.create_plan("NoOpMemberOtherPlan").await;
    for (index, row) in member_rows().into_iter().enumerate() {
        let reported = Reported {
            name: member_group_name(index),
            plan: world.group_plan.clone(),
            other_plan: other_plan.clone(),
        };
        // A group's primary account is one of its own linked accounts and an account
        // belongs to at most one group, so each row's group owns a different one; the
        // offset keeps them clear of the groups created above.
        let primary = primary_account_id(index + 32);
        let case = format!("a group restating the {} it reports", row.label);
        let group = world
            .billing
            .create_group(
                &reported.name,
                &reported.plan,
                &primary,
                &(row.created)(&reported),
            )
            .await;
        world.billing.created_in(&group, &created_in);
        let generations_before = world.billing.superseded_generations(&group);

        world
            .billing
            .update_group(&case, &group, &(row.restated)(&reported))
            .await;
        let generations_after = world.billing.superseded_generations(&group);
        assert_eq!(
            generations_after, generations_before,
            "case {case}: expected a write that modified nothing to freeze no generation, \
             got {generations_before} generations before and {generations_after} after"
        );
        let element = world
            .billing
            .element_in(&case, &group, Some(&now))
            .await
            .unwrap_or_else(|| panic!("case {case}: the group exists now, so it is reported"));
        assert_members(&case, &element, &json!({"LastModifiedTime": created_at}));

        // The control for the same member: a value the group does not report moves the
        // instant and spends a slot of the bounded generation history.
        let case = format!("a group naming a {} it does not report", row.label);
        world
            .billing
            .update_group(&case, &group, &(row.changed)(&reported))
            .await;
        let generations_after = world.billing.superseded_generations(&group);
        assert_eq!(
            generations_after,
            generations_before + 1,
            "case {case}: expected a write that modified the group in a later billing period \
             to freeze the generation it displaced, got {generations_before} generations \
             before and {generations_after} after"
        );
        let frozen = world
            .billing
            .element_in(&case, &group, Some(&created_in))
            .await
            .unwrap_or_else(|| panic!("case {case}: the group existed then, so it is reported"));
        assert_members(&case, &frozen, &json!({"LastModifiedTime": created_at}));
        let element = world
            .billing
            .element_in(&case, &group, Some(&now))
            .await
            .unwrap_or_else(|| panic!("case {case}: the group exists now, so it is reported"));
        let modified = element["LastModifiedTime"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case}: no LastModifiedTime in {element:?}"));
        assert!(
            modified > created_at,
            "case {case}: expected a write that modified the group to move LastModifiedTime \
             past {created_at}, got {modified}"
        );
    }
}

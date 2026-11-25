// SPDX-License-Identifier: CC0-1.0

//! The BILLING PERIOD dimension of a pricing rule / pricing plan association, end to
//! end over the service's real `restJson1` wire through
//! [`MultiServiceRouter::dispatch`] — the same path the production router serves.
//!
//! RULE: an association between a pricing rule and a pricing plan is in force from
//! the billing period it was made in onward, so every read scoped to a billing period
//! reports it for that period and every later one and for no earlier one. That covers
//! `ListPricingRulesAssociatedToPricingPlan` and its inverse index
//! `ListPricingPlansAssociatedWithPricingRule`, and the counts the two listings derive
//! from the same associations (`PricingPlanListElement.Size`,
//! `PricingRuleListElement.AssociatedPricingPlanCount`). A read that names no period
//! asks about the current one.
//!
//! Each of those two listings REFERENCES one resource — the plan it addresses, the rule
//! it addresses — and resolves it in the period being read, so a period in which this
//! account held no such resource is refused with the modelled 404 rather than answered
//! with an empty association set ([`assert_no_such_resource_in_period`]); only a period
//! the resource existed in reports "no association in force".
//!
//! Each table's rows differ along what the rule generalises over: the period an
//! association started in (the epoch month, a December, a January, a far-future
//! month, and both spellings of one month the modelled `BillingPeriod` pattern
//! `^\d{4}-(0?[1-9]|1[012])$` allows) and the period a read asks about (the month
//! before the start, the start itself, the month after, a year on either side, both
//! spellings of the start, and no period at all).
//!
//! The wire cannot make an association in a PAST period — an associating call happens
//! now — so a row makes its association through the real wire and then backdates the
//! stored start period, which is the one thing the test cannot ask the service for.
//! Backdating an association backdates the RULE it names too: an association cannot be
//! in force in a period before the rule exists, and a rule is reported only for the
//! periods from its creation onward.

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_billingconductor_awsbillingconductor::{
    ComAmazonawsBillingconductorAwsbillingconductorState as BillingConductorState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// A `BillingPeriod` as a count of months, so two periods compare and shift.
///
/// Shared with the suites that read this world from another angle
/// (`tests/all/pricing_rule_association_billing_periods.rs`,
/// `tests/all/pricing_rule_association_period_subsets.rs`), so one ordering rule serves
/// every period-scoped assertion in this crate.
pub(crate) fn period_months(value: &str) -> i64 {
    let (year, month) = value
        .split_once('-')
        .expect("a test billing period is spelled YYYY-MM");
    let year: i64 = year.parse().expect("a test billing period has a year");
    let month: i64 = month.parse().expect("a test billing period has a month");
    year * 12 + month - 1
}

/// The `BillingPeriod` a month count names, in the zero-padded spelling the modelled
/// pattern allows.
fn period_from_months(total: i64) -> String {
    format!(
        "{:04}-{:02}",
        total.div_euclid(12),
        total.rem_euclid(12) + 1
    )
}

/// The instant a billing period BEGINS, in millis since the epoch: midnight UTC on the
/// first day of that month.
///
/// The conversion is `aws_smithy_types::DateTime` — the same formatter the service
/// derives a billing period from an instant with — rather than hand-written calendar
/// arithmetic, applied to the canonical spelling of the month so either spelling of one
/// period answers the same instant.
pub(crate) fn period_start_millis(period: &str) -> i64 {
    let canonical = period_from_months(period_months(period));
    ::aws_smithy_types::DateTime::from_str(
        &format!("{canonical}-01T00:00:00Z"),
        ::aws_smithy_types::date_time::Format::DateTime,
    )
    .unwrap_or_else(|error| panic!("a test billing period starts at an instant: {error}"))
    .to_millis()
    .unwrap_or_else(|error| panic!("a test billing period start fits in millis: {error}"))
}

/// The period `months` after `period` (negative for before), crossing year
/// boundaries.
///
/// Shared with the suites that read a period-scoped world from another angle
/// (`tests/all/account_association_billing_periods.rs`), so one month arithmetic
/// serves every period-scoped assertion in this crate.
pub(crate) fn shift_period(period: &str, months: i64) -> String {
    period_from_months(period_months(period) + months)
}

/// The periods a suite probes around an association: well before it, just before it, the
/// period it started in, the one after, the current period, the one after that, and the
/// extremes of the modelled `BillingPeriod` pattern `^\d{4}-(0?[1-9]|1[012])$`.
///
/// Shared by the suites that hold the readers of an association to each other
/// (`tests/all/pricing_rule_delete_reader_agreement.rs`,
/// `tests/all/association_delete_gate_spans_billing_periods.rs`).
pub(crate) fn probe_periods(start: &str, now: &str) -> Vec<String> {
    vec![
        "1970-01".to_string(),
        shift_period(start, -13),
        shift_period(start, -1),
        start.to_string(),
        shift_period(start, 1),
        now.to_string(),
        shift_period(now, 1),
        "9999-12".to_string(),
    ]
}

/// `POST /delete-pricing-rule` — the write that can strand an association
/// (`smithy_query(query="operation",operation="DeletePricingRule")`).
pub(crate) const DELETE_RULE_PATH: &str = "/delete-pricing-rule";

/// `POST /delete-pricing-plan`, which takes a plan and the associations it holds out of
/// existence.
pub(crate) const DELETE_PLAN_PATH: &str = "/delete-pricing-plan";

/// `PUT /disassociate-pricing-rules`, which ends an association from the period the call
/// is made in onward.
pub(crate) const DISASSOCIATE_PATH: &str = "/disassociate-pricing-rules";

/// A live emulator holding only this service, with the engine kept so a row can
/// backdate an association the wire could only make now.
///
/// This is the harness the other association-period suites in this crate drive too, so
/// the dispatch and signing boilerplate exists once.
pub(crate) struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    /// A router over ONE shared engine, with the emulated account designated the
    /// organization's PAYER account — the world in which this payer-account-only
    /// control plane is reachable at all (see `tests/all/payer_account_gate.rs` for
    /// the denial every other caller gets).
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        engine
            .state::<BillingConductorState>()
            .lock()
            .payer_account_id = Some(::engine::emulator_account_id().to_string());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
    }

    /// POST a JSON body to one of this service's routes and return the status plus
    /// the parsed response body.
    pub(crate) async fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        self.send("POST", path, body).await
    }

    /// PUT a JSON body to one of this service's routes — the method the writes'
    /// modelled HTTP bindings name (`UpdatePricingRule`, `AssociatePricingRules`).
    pub(crate) async fn put(&self, path: &str, body: &Value) -> (u16, Value) {
        self.send("PUT", path, body).await
    }

    /// Send a JSON body to one of this service's routes with the method its modelled
    /// HTTP binding names (`smithy_query(query="operation", operation="...")`:
    /// `AssociatePricingRules` is a `PUT`, the listings are `POST`).
    async fn send(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://billingconductor.us-east-1.amazonaws.com{path}"
            ))
            .header("host", "billingconductor.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/us-east-1/billingconductor/aws4_request, \
                 SignedHeaders=host, Signature=00",
            )
            .body(Bytes::from(
                serde_json::to_vec(body).expect("serialize the request body"),
            ))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create a `GLOBAL`/`MARKUP` pricing rule and answer the ARN the service minted.
    pub(crate) async fn create_rule(&self, name: &str) -> String {
        let (status, body) = self
            .post(
                "/create-pricing-rule",
                &json!({"Name": name, "Scope": "GLOBAL", "Type": "MARKUP",
                        "ModifierPercentage": 10.0}),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup CreatePricingRule {name:?}: expected 200, got {status} with {body:?}"
        );
        body["Arn"]
            .as_str()
            .expect("setup CreatePricingRule: no Arn in the response")
            .to_string()
    }

    /// Create a pricing plan associating `rules`, and answer the ARN the service
    /// minted.
    pub(crate) async fn create_plan(&self, name: &str, rules: &[String]) -> String {
        let (status, body) = self
            .post(
                "/create-pricing-plan",
                &json!({"Name": name, "PricingRuleArns": rules}),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup CreatePricingPlan {name:?}: expected 200, got {status} with {body:?}"
        );
        body["Arn"]
            .as_str()
            .expect("setup CreatePricingPlan: no Arn in the response")
            .to_string()
    }

    /// `AssociatePricingRules`: add `rules` to the plan's associations.
    pub(crate) async fn associate(&self, plan: &str, rules: &[String]) {
        let (status, body) = self
            .send(
                "PUT",
                "/associate-pricing-rules",
                &json!({"Arn": plan, "PricingRuleArns": rules}),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup AssociatePricingRules on {plan:?}: expected 200, got {status} with {body:?}"
        );
    }

    /// Backdate every association the plan holds to `period`, as if the associating
    /// call had been made in that billing period — and with it the plan and the rules
    /// those associations name, since a resource has to exist in the period its
    /// association is in force in.
    fn backdate(&self, plan: &str, period: &str) {
        let rules: Vec<String> = {
            let state = self.engine.state::<BillingConductorState>();
            let mut state = state.lock();
            let record = state
                .pricing_plans
                .get_mut(plan)
                .expect("setup backdate: the plan was created above");
            for association in &mut record.pricing_rule_associations {
                association.inclusive_start_billing_period = period.to_string();
            }
            record
                .pricing_rule_associations
                .iter()
                .map(|association| association.arn.clone())
                .collect()
        };
        self.create_plan_in(plan, period);
        for rule in rules {
            self.create_rule_in(&rule, period);
        }
    }

    /// Backdate `plan_arn`'s creation to `period`, as if the creating call had been made
    /// in that billing period: the way a world where a plan already existed in a closed
    /// period is set up, which no request can ask for (a create happens now).
    pub(crate) fn create_plan_in(&self, plan_arn: &str, period: &str) {
        let at = period_start_millis(period);
        let state = self.engine.state::<BillingConductorState>();
        let mut state = state.lock();
        let record = state
            .pricing_plans
            .get_mut(plan_arn)
            .expect("setup create_plan_in: the plan was created above");
        record.creation_time_millis = at;
        record.last_modified_time_millis = at;
    }

    /// How many SUPERSEDED generations of `plan_arn` the store holds: the slots this plan
    /// has spent of the bounded version history a period-scoped read of a closed period
    /// is answered from. Counted from the store because no read reports the size of that
    /// history.
    pub(crate) fn superseded_plan_generations(&self, plan_arn: &str) -> usize {
        let state = self.engine.state::<BillingConductorState>();
        let state = state.lock();
        state
            .pricing_plans
            .get(plan_arn)
            .expect("superseded_plan_generations: the plan was created above")
            .superseded_versions
            .len()
    }

    /// Backdate the plan's CURRENT generation to `period`, as if the write that left the
    /// plan as it stands had been made in that billing period: how a world whose
    /// generations started in DIFFERENT periods is set up, which no request can ask for
    /// (a write happens now).
    pub(crate) fn restate_plan_generation_in(&self, plan_arn: &str, period: &str) {
        let at = period_start_millis(period);
        let state = self.engine.state::<BillingConductorState>();
        let mut state = state.lock();
        let record = state
            .pricing_plans
            .get_mut(plan_arn)
            .expect("setup restate_plan_generation_in: the plan was created above");
        record.last_modified_time_millis = at;
    }

    /// Backdate `rule_arn`'s creation to `period`, as if the creating call had been made
    /// in that billing period: the way a world where a rule already existed in a closed
    /// period is set up, which no request can ask for (a create happens now).
    pub(crate) fn create_rule_in(&self, rule_arn: &str, period: &str) {
        let at = period_start_millis(period);
        let state = self.engine.state::<BillingConductorState>();
        let mut state = state.lock();
        let record = state
            .pricing_rules
            .get_mut(rule_arn)
            .expect("setup create_rule_in: the rule was created above");
        record.creation_time_millis = at;
        record.last_modified_time_millis = at;
    }

    /// Backdate the DELETE of `rule_arn` to `period`, as if the deleting call had been
    /// made in that billing period: the wire can only delete now, and the billing periods
    /// a deleted rule is still reported for are the ones before the delete was made in
    /// (`tests/all/deleted_pricing_rule_period_reads.rs`).
    pub(crate) fn delete_rule_in(&self, rule_arn: &str, period: &str) {
        let at = period_start_millis(period);
        let state = self.engine.state::<BillingConductorState>();
        let mut state = state.lock();
        let deleted = state
            .deleted_pricing_rules
            .iter_mut()
            .find(|deleted| deleted.record.arn == rule_arn)
            .expect("setup delete_rule_in: the rule was deleted above");
        deleted.deletion_time_millis = at;
    }

    /// Backdate the rule's CURRENT generation to `period`, as if the write that left the
    /// rule as it stands had been made in that billing period: how a world whose
    /// generations started in DIFFERENT periods is set up, which no request can ask for
    /// (a write happens now).
    pub(crate) fn restate_generation_in(&self, rule_arn: &str, period: &str) {
        let at = period_start_millis(period);
        let state = self.engine.state::<BillingConductorState>();
        let mut state = state.lock();
        let record = state
            .pricing_rules
            .get_mut(rule_arn)
            .expect("setup restate_generation_in: the rule was created above");
        record.last_modified_time_millis = at;
    }

    /// Backdate the plan's association with ONE rule to `period`, as if that
    /// associating call had been made in that billing period: the way a world where a
    /// plan's associations started in DIFFERENT periods is set up
    /// (`tests/all/pricing_rule_association_billing_periods.rs`), which no request can
    /// ask for.
    pub(crate) fn start_association_in(&self, plan: &str, rule_arn: &str, period: &str) {
        let plan_created_later = {
            let state = self.engine.state::<BillingConductorState>();
            let mut state = state.lock();
            let record = state
                .pricing_plans
                .get_mut(plan)
                .expect("setup start_association_in: the plan was created above");
            let association = record
                .pricing_rule_associations
                .iter_mut()
                .find(|association| association.arn == rule_arn)
                .expect("setup start_association_in: the association was made above");
            association.inclusive_start_billing_period = period.to_string();
            record.creation_time_millis > period_start_millis(period)
        };
        // The plan and the rule both have to exist in the period their association is in
        // force in, so the plan's creation moves back to the EARLIEST of those periods.
        if plan_created_later {
            self.create_plan_in(plan, period);
        }
        self.create_rule_in(rule_arn, period);
    }

    /// The billing period a stored pricing plan was CREATED in, derived from the stored
    /// instant the way the service derives it, or `None` when the store holds no such
    /// plan — which exists in no period at all.
    ///
    /// An AWS-managed plan is held by no account and created by no call, so it has no
    /// creation period; [`Self::plan_existed_in`] answers for it separately.
    pub(crate) fn plan_creation_period(&self, plan_arn: &str) -> Option<String> {
        let arn = normalize_plan(plan_arn);
        let state = self.engine.state::<BillingConductorState>();
        let state = state.lock();
        state
            .pricing_plans
            .get(&arn)
            .map(|plan| period_of_instant(plan.creation_time_millis))
    }

    /// The billing period a stored pricing rule was CREATED in, or `None` when the store
    /// holds no such rule.
    pub(crate) fn rule_creation_period(&self, rule_arn: &str) -> Option<String> {
        let arn = normalize_rule(rule_arn);
        let state = self.engine.state::<BillingConductorState>();
        let state = state.lock();
        state
            .pricing_rules
            .get(&arn)
            .map(|rule| period_of_instant(rule.creation_time_millis))
    }

    /// Did the pricing plan the request addresses exist in the billing period the read
    /// asks about — is it one of the plans that period reports?
    ///
    /// A resource exists in no period before the one it was created in, and a read that
    /// names no period asks about the current one. An AWS-managed plan is there without
    /// any call creating it, so it exists in every period.
    pub(crate) fn plan_existed_in(&self, plan_arn: &str, period: Option<&str>) -> bool {
        if is_aws_managed_plan(&normalize_plan(plan_arn)) {
            return true;
        }
        self.plan_creation_period(plan_arn)
            .is_some_and(|created| asks_about(period) >= period_months(&created))
    }

    /// Did the pricing rule the request addresses exist in the billing period the read
    /// asks about? There are no AWS-managed pricing rules, so what is stored is the whole
    /// universe of rules ([`Self::plan_existed_in`] for the plan side).
    pub(crate) fn rule_existed_in(&self, rule_arn: &str, period: Option<&str>) -> bool {
        self.rule_creation_period(rule_arn)
            .is_some_and(|created| asks_about(period) >= period_months(&created))
    }

    /// The rule ARNs `ListPricingRulesAssociatedToPricingPlan` reports for `plan` in
    /// `period` (`None` names no period at all).
    ///
    /// The plan is the resource that read REFERENCES, so a period this account held no
    /// such plan in is answered with the modelled 404 rather than with an empty set of
    /// associations ([`assert_no_such_resource_in_period`]), and the associations such a
    /// period reports are none.
    pub(crate) async fn rules_for_plan(
        &self,
        case: &str,
        plan: &str,
        period: Option<&str>,
    ) -> Vec<String> {
        let (status, answer) = self.rules_for_plan_response(plan, period).await;
        if !self.plan_existed_in(plan, period) {
            assert_no_such_resource_in_period(
                case,
                "ListPricingRulesAssociatedToPricingPlan",
                status,
                &answer,
                "pricingplan",
                period,
            );
            return Vec::new();
        }
        assert_eq!(
            status, 200,
            "case {case}: ListPricingRulesAssociatedToPricingPlan expected 200, got {status} \
             with {answer:?}"
        );
        arns(&answer, "PricingRuleArns")
    }

    /// The raw answer of `ListPricingRulesAssociatedToPricingPlan`: the status and the
    /// parsed body, for a caller that asserts the refusal itself.
    pub(crate) async fn rules_for_plan_response(
        &self,
        plan: &str,
        period: Option<&str>,
    ) -> (u16, Value) {
        let mut body = json!({"PricingPlanArn": plan});
        if let Some(period) = period {
            body["BillingPeriod"] = json!(period);
        }
        self.post("/list-pricing-rules-associated-to-pricing-plan", &body)
            .await
    }

    /// The plan ARNs `ListPricingPlansAssociatedWithPricingRule` reports for `rule`
    /// in `period`.
    ///
    /// The rule is the resource that read REFERENCES, so a period this account held no
    /// such rule in is answered with the modelled 404 — the same rule the plan side
    /// answers by ([`Self::rules_for_plan`]).
    pub(crate) async fn plans_for_rule(
        &self,
        case: &str,
        rule: &str,
        period: Option<&str>,
    ) -> Vec<String> {
        let (status, answer) = self.plans_for_rule_response(rule, period).await;
        if !self.rule_existed_in(rule, period) {
            assert_no_such_resource_in_period(
                case,
                "ListPricingPlansAssociatedWithPricingRule",
                status,
                &answer,
                "pricingrule",
                period,
            );
            return Vec::new();
        }
        assert_eq!(
            status, 200,
            "case {case}: ListPricingPlansAssociatedWithPricingRule expected 200, got {status} \
             with {answer:?}"
        );
        arns(&answer, "PricingPlanArns")
    }

    /// The STATUS `ListPricingPlansAssociatedWithPricingRule` answers for `rule` in
    /// `period`, beside the plan ARNs it reports — the rule-side reader as an existence
    /// probe too, since it answers the modelled 404 for a rule the period it is read for
    /// held none of (`tests/all/pricing_rule_delete_reader_agreement.rs`).
    pub(crate) async fn plans_for_rule_answer(
        &self,
        rule: &str,
        period: Option<&str>,
    ) -> (u16, Vec<String>) {
        let (status, answer) = self.plans_for_rule_response(rule, period).await;
        (status, arns(&answer, "PricingPlanArns"))
    }

    /// The raw answer of `ListPricingPlansAssociatedWithPricingRule`: the status and the
    /// parsed body.
    pub(crate) async fn plans_for_rule_response(
        &self,
        rule: &str,
        period: Option<&str>,
    ) -> (u16, Value) {
        let mut body = json!({"PricingRuleArn": rule});
        if let Some(period) = period {
            body["BillingPeriod"] = json!(period);
        }
        self.post("/list-pricing-plans-associated-with-pricing-rule", &body)
            .await
    }

    /// The `Size` `ListPricingPlans` reports for `plan` in `period`.
    pub(crate) async fn plan_size(&self, case: &str, plan: &str, period: Option<&str>) -> i64 {
        let mut body = json!({"Filters": {"Arns": [plan]}});
        if let Some(period) = period {
            body["BillingPeriod"] = json!(period);
        }
        let (status, answer) = self.post("/list-pricing-plans", &body).await;
        assert_eq!(
            status, 200,
            "case {case}: ListPricingPlans expected 200, got {status} with {answer:?}"
        );
        // A plan is reported only for the billing periods from its creation onward, so a
        // period in which the plan did not exist yet reports neither the plan nor any
        // association of it: it associated no rule then.
        if answer["PricingPlans"].as_array().is_none_or(Vec::is_empty) {
            return 0;
        }
        answer["PricingPlans"][0]["Size"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case}: ListPricingPlans reported no Size: {answer:?}"))
    }

    /// The `AssociatedPricingPlanCount` `ListPricingRules` reports for `rule` in
    /// `period`.
    pub(crate) async fn rule_plan_count(
        &self,
        case: &str,
        rule: &str,
        period: Option<&str>,
    ) -> i64 {
        let mut body = json!({"Filters": {"Arns": [rule]}});
        if let Some(period) = period {
            body["BillingPeriod"] = json!(period);
        }
        let (status, answer) = self.post("/list-pricing-rules", &body).await;
        assert_eq!(
            status, 200,
            "case {case}: ListPricingRules expected 200, got {status} with {answer:?}"
        );
        // A rule is reported only for the billing periods from its creation onward, so a
        // period in which the rule did not exist yet reports neither the rule nor any
        // association of it: no plan associates it then.
        if answer["PricingRules"]
            .as_array()
            .is_none_or(Vec::is_empty)
        {
            return 0;
        }
        answer["PricingRules"][0]["AssociatedPricingPlanCount"]
            .as_i64()
            .unwrap_or_else(|| {
                panic!("case {case}: ListPricingRules reported no count: {answer:?}")
            })
    }

    /// Does `ListPricingRules` report `rule` for `period`? The period-scoped existence
    /// probe for a pricing rule: a rule is reported exactly while the store holds it and
    /// the period is one from its creation onward, so this is what decides whether a rule
    /// ARN another reader reports resolves at all
    /// (`tests/all/pricing_rule_delete_reader_agreement.rs`,
    /// `tests/all/association_delete_gate_spans_billing_periods.rs`).
    pub(crate) async fn rule_is_reported(
        &self,
        case: &str,
        rule: &str,
        period: Option<&str>,
    ) -> bool {
        let mut body = json!({"Filters": {"Arns": [rule]}});
        if let Some(period) = period {
            body["BillingPeriod"] = json!(period);
        }
        let (status, answer) = self.post("/list-pricing-rules", &body).await;
        assert_eq!(
            status, 200,
            "case {case}: ListPricingRules expected 200, got {status} with {answer:?}"
        );
        answer["PricingRules"]
            .as_array()
            .is_some_and(|rules| !rules.is_empty())
    }

    /// The INVARIANT every reader of one pricing rule association is held to: over each
    /// period of `periods`, the readers of `plan`'s associations agree with each other,
    /// and every rule ARN they report names a rule the store still reports for that
    /// period — so no reader hands a caller an ARN that resolves to nothing.
    ///
    /// The three readers of one association are
    /// `ListPricingRulesAssociatedToPricingPlan` ([`Self::rules_for_plan`]),
    /// `PricingPlanListElement.Size` ([`Self::plan_size`]) and the inverse index
    /// `ListPricingPlansAssociatedWithPricingRule` ([`Self::plans_for_rule_answer`],
    /// which answers the modelled 404 for a rule the store does not hold);
    /// `ListPricingRules` ([`Self::rule_is_reported`]) is what decides whether the ARN
    /// they report resolves.
    pub(crate) async fn assert_readers_agree(&self, case: &str, plan: &str, periods: &[String]) {
        for period in periods {
            let reported = self.rules_for_plan(case, plan, Some(period)).await;
            let size = self.plan_size(case, plan, Some(period)).await;
            let expected_size =
                i64::try_from(reported.len()).expect("a small count fits in an i64");
            assert_eq!(
                size, expected_size,
                "case {case}: in {period} ListPricingPlans counts Size {size} for {plan} while \
                 the plan-side listing reports {reported:?}"
            );
            for rule in &reported {
                assert!(
                    self.rule_is_reported(case, rule, Some(period)).await,
                    "case {case}: the plan {plan} reports the rule {rule} for {period}, but \
                     ListPricingRules does not report that rule then"
                );
                let (status, plans) = self.plans_for_rule_answer(rule, Some(period)).await;
                assert_eq!(
                    status, 200,
                    "case {case}: the plan {plan} reports the rule {rule} for {period}, but the \
                     rule-side reader answered {status} with {plans:?}"
                );
                assert!(
                    plans.iter().any(|named| named == plan),
                    "case {case}: the rule-side reader of {rule} in {period} reports {plans:?}, \
                     which omits {plan}"
                );
            }
        }
    }
}

/// The ARN list a response reports under `member`, as owned strings (an absent
/// member is an empty set, which is how both listings report "none": the member is
/// modelled `@length(min: 1)`).
pub(crate) fn arns(body: &Value, member: &str) -> Vec<String> {
    body.get(member)
        .and_then(Value::as_array)
        .map(|arns| {
            arns.iter()
                .map(|arn| arn.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The billing period the service calls current, read from the same clock it answers
/// from so an expectation holds whatever month the suite runs in.
fn current_period() -> String {
    service_com_amazonaws_billingconductor_awsbillingconductor::state::current_billing_period()
}

/// The billing period an instant falls in, derived the way the service derives the
/// creation period of a stored resource from its `CreationTime`.
fn period_of_instant(millis: i64) -> String {
    service_com_amazonaws_billingconductor_awsbillingconductor::state::billing_period_at(millis)
}

/// A `PricingPlanArn` in either spelling the modelled pattern allows, as the full ARN the
/// store keys plans by.
fn normalize_plan(plan_arn: &str) -> String {
    service_com_amazonaws_billingconductor_awsbillingconductor::state::normalize_pricing_plan_arn(
        plan_arn,
    )
}

/// A `PricingRuleArn` in either spelling the modelled pattern allows, as the full ARN the
/// store keys rules by.
fn normalize_rule(rule_arn: &str) -> String {
    service_com_amazonaws_billingconductor_awsbillingconductor::state::normalize_pricing_rule_arn(
        rule_arn,
    )
}

/// Is this the ARN of a pricing plan Amazon Web Services owns — one that exists without
/// any call creating it, and so in every billing period?
fn is_aws_managed_plan(plan_arn: &str) -> bool {
    service_com_amazonaws_billingconductor_awsbillingconductor::state::is_aws_managed_pricing_plan(
        plan_arn,
    )
}

/// The billing period a read asks about, as a month count: the one it names, or the
/// current one when it names none.
fn asks_about(period: Option<&str>) -> i64 {
    period
        .map(period_months)
        .unwrap_or_else(|| period_months(&current_period()))
}

/// RULE: a period-scoped association read resolves the resource it REFERENCES in the
/// period being read, so a period in which this account held no such resource is answered
/// with the modelled `ResourceNotFoundException` (HTTP 404) rather than with a 200
/// reporting no association — the same answer the period-scoped LISTING of that resource
/// gives by leaving it out of that period.
pub(crate) fn assert_no_such_resource_in_period(
    case: &str,
    operation: &str,
    status: u16,
    body: &Value,
    resource_type: &str,
    period: Option<&str>,
) {
    assert_eq!(
        status, 404,
        "case {case}: {operation} read for {period:?} names a {resource_type} that period held \
         none of, so the modelled 404 is expected, got {status} with {body:?}"
    );
    let code = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case}: {operation} read for {period:?} must be refused with the modelled \
         ResourceNotFoundException, got {code:?} in {body:?}"
    );
    assert_eq!(
        body["ResourceType"].as_str(),
        Some(resource_type),
        "case {case}: the refusal must name the resource type it could not resolve, got {body:?}"
    );
}

/// The periods an association starting in `start` must and must not be reported for:
/// the start itself and later periods, against earlier ones.
fn read_periods(start: &str) -> Vec<(String, bool)> {
    vec![
        (shift_period(start, -13), false),
        (shift_period(start, -1), false),
        (start.to_string(), true),
        (shift_period(start, 1), true),
        (shift_period(start, 13), true),
    ]
}

/// The periods a row starts its association in. Rows differ along the month form the
/// modelled `BillingPeriod` pattern allows (`0?[1-9]`, so `2026-7` and `2026-07` are
/// ONE period), the end-of-year and start-of-year months whose neighbours cross a
/// year, the epoch month, and a far-future one.
const START_PERIODS: &[&str] = &["1970-01", "1999-12", "2026-1", "2026-07", "2030-3", "9980-12"];

#[tokio::test]
async fn an_association_is_reported_from_the_period_it_started_in_onward_and_not_before() {
    let em = Emulator::new();
    for (index, start) in START_PERIODS.iter().enumerate() {
        let rule = em.create_rule(&format!("Period-Rule-{index}")).await;
        let plan = em
            .create_plan(&format!("period-plan-{index}"), std::slice::from_ref(&rule))
            .await;
        em.backdate(&plan, start);

        for (period, in_force) in read_periods(start) {
            let case = format!("start {start:?} read at {period:?}");
            let expected: Vec<String> = if in_force {
                vec![rule.clone()]
            } else {
                vec![]
            };
            let reported = em.rules_for_plan(&case, &plan, Some(&period)).await;
            assert_eq!(
                reported, expected,
                "case {case}: expected {expected:?}, got {reported:?}"
            );

            // The inverse index is derived from the same association, so it agrees
            // period for period.
            let expected_plans: Vec<String> = if in_force {
                vec![plan.clone()]
            } else {
                vec![]
            };
            let inverse = em.plans_for_rule(&case, &rule, Some(&period)).await;
            assert_eq!(
                inverse, expected_plans,
                "case {case}: the inverse index must agree, expected {expected_plans:?}, \
                 got {inverse:?}"
            );
        }

        // Both spellings of ONE month name one period, so they answer the same.
        let (year, month) = start
            .split_once('-')
            .expect("a start period is spelled YYYY-MM");
        let month: u32 = month.parse().expect("a start period has a numeric month");
        for spelling in [format!("{year}-{month}"), format!("{year}-{month:02}")] {
            let case = format!("start {start:?} read at {spelling:?}");
            let reported = em.rules_for_plan(&case, &plan, Some(&spelling)).await;
            assert_eq!(
                reported,
                vec![rule.clone()],
                "case {case}: either spelling of the start period reports the association, \
                 got {reported:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_derived_association_counts_are_taken_for_the_period_the_read_names() {
    let em = Emulator::new();
    for (index, start) in START_PERIODS.iter().enumerate() {
        let rule = em.create_rule(&format!("Count-Rule-{index}")).await;
        let plan = em
            .create_plan(&format!("count-plan-{index}"), std::slice::from_ref(&rule))
            .await;
        em.backdate(&plan, start);

        for (period, in_force) in read_periods(start) {
            let case = format!("start {start:?} counted at {period:?}");
            let expected = i64::from(in_force);
            let size = em.plan_size(&case, &plan, Some(&period)).await;
            assert_eq!(
                size, expected,
                "case {case}: PricingPlanListElement.Size counts the associations in force in \
                 the period, expected {expected}, got {size}"
            );
            let count = em.rule_plan_count(&case, &rule, Some(&period)).await;
            assert_eq!(
                count, expected,
                "case {case}: AssociatedPricingPlanCount counts the plans associating the rule \
                 in the period, expected {expected}, got {count}"
            );
        }
    }
}

#[tokio::test]
async fn an_association_made_now_is_in_force_now_and_in_no_earlier_period() {
    let em = Emulator::new();
    let created_with = em.create_rule("Now-Created").await;
    let associated_later = em.create_rule("Now-Associated").await;
    let plan = em
        .create_plan("now-plan", std::slice::from_ref(&created_with))
        .await;
    em.associate(&plan, std::slice::from_ref(&associated_later))
        .await;

    // Both ways of making an association — the create that names the rule and the
    // associating call that adds one — start it in the period the call ran in.
    let mut both = vec![created_with.clone(), associated_later.clone()];
    both.sort();
    let now = current_period();

    // Rows differ along the period asked about: none at all (the current one), the
    // current period named explicitly, a far-future period, and periods before the
    // association was made.
    let rows: Vec<(String, Option<String>, Vec<String>)> = vec![
        ("no period at all".to_string(), None, both.clone()),
        (
            "the current period".to_string(),
            Some(now.clone()),
            both.clone(),
        ),
        (
            "a far-future period".to_string(),
            Some("9999-12".to_string()),
            both.clone(),
        ),
        (
            "the period before".to_string(),
            Some(shift_period(&now, -1)),
            vec![],
        ),
        (
            "the epoch month".to_string(),
            Some("1970-01".to_string()),
            vec![],
        ),
    ];

    for (case, period, expected) in rows {
        let reported = em.rules_for_plan(&case, &plan, period.as_deref()).await;
        assert_eq!(
            reported, expected,
            "case {case}: expected {expected:?}, got {reported:?}"
        );
        let size = em.plan_size(&case, &plan, period.as_deref()).await;
        let expected_size = i64::try_from(expected.len()).expect("a small count fits in i64");
        assert_eq!(
            size, expected_size,
            "case {case}: Size must count the same associations the listing reports, \
             expected {expected_size}, got {size}"
        );
    }

    // The reads changed nothing: the associations still stand for the current period.
    let reported = em.rules_for_plan("after the reads", &plan, None).await;
    assert_eq!(
        reported, both,
        "case \"after the reads\": a read must not change what a later caller sees, got \
         {reported:?}"
    );
}

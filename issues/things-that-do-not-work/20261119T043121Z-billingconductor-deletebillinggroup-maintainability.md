Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 94d2aaccc78d8a13524b41a95baab02d39e9af8f

# The idempotency-token reclamation of a removed resource is written out four times in state.rs, once per remove_* method

_Filed: 2026-11-19T04:31:21Z_
_Operation: DeleteBillingGroup | Class: maintainability_

> _Note: this issue was identified against the code base at commit `94d2aaccc78d8a13524b41a95baab02d39e9af8f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Reclaiming the `ClientToken` -> ARN entries that named a resource a delete removed is ONE rule of this crate: drop every entry in `idempotency_tokens` whose value is the removed ARN, then prune `idempotency_order` back to the keys that are still in the map, so the order deque keeps holding exactly "the keys of `idempotency_tokens` in insertion order" as its own doc states. That rule is implemented four separate times in `src/state.rs` — once in each of `remove_billing_group`, `remove_custom_line_item`, `remove_pricing_rule` and `remove_pricing_plan` — as four byte-identical copies of the same two statements. The insert half of the same invariant is NOT duplicated: `record_idempotent_arn` is the one writer that pushes a key onto `idempotency_order` and evicts at the cap, so the single-place home for this pair already exists and only the removal half was copied. Because the copies are independent, they can drift: a fifth removal added later (or a correction to one of the four) can keep the map pruning and forget the `idempotency_order` pruning, leaving an order key whose map entry is gone, which then makes the cap evict a token that is still live. The fix is to route all four through one private helper on the state (`fn release_idempotent_arns(&mut self, arn: &str)`) and call it where the four copies stand; no response and no existing test changes, since the behaviour of each copy is identical today.

## Evidence

The invariant the duplicated pair maintains, stated once on the field — services/service-com-amazonaws-billingconductor-awsbillingconductor/src/state.rs:2990-2996:

    /// `<operation>:<ClientToken>` -> the ARN that token's first call minted, so a
    /// retry "completes successfully without performing any further actions".
    /// Private for the same reason as `unassociated_accounts`.
    idempotency_tokens: BTreeMap<String, String>,
    /// The keys of `idempotency_tokens` in insertion order, so the cap evicts the
    /// oldest token rather than an arbitrary one.
    idempotency_order: VecDeque<String>,

Copy 1 — `remove_billing_group`, src/state.rs:3530-3533:

        self.idempotency_tokens.retain(|_, arn| arn != group_arn);
        let remaining = &self.idempotency_tokens;
        self.idempotency_order
            .retain(|key| remaining.contains_key(key));

Copy 2 — `remove_custom_line_item`, src/state.rs:3601-3604:

        self.idempotency_tokens.retain(|_, arn| arn != item_arn);
        let remaining = &self.idempotency_tokens;
        self.idempotency_order
            .retain(|key| remaining.contains_key(key));

Copy 3 — `remove_pricing_rule`, src/state.rs:3794-3797:

        self.idempotency_tokens.retain(|_, arn| arn != rule_arn);
        let remaining = &self.idempotency_tokens;
        self.idempotency_order
            .retain(|key| remaining.contains_key(key));

Copy 4 — `remove_pricing_plan`, src/state.rs:3813-3816:

        self.idempotency_tokens.retain(|_, arn| arn != plan_arn);
        let remaining = &self.idempotency_tokens;
        self.idempotency_order
            .retain(|key| remaining.contains_key(key));

The insert half of the same pair is already written ONCE — src/state.rs:3106-3117 (`record_idempotent_arn`):

        let key = idempotency_key(operation, token);
        if self.idempotency_tokens.insert(key.clone(), arn.to_string()).is_none() {
            self.idempotency_order.push_back(key);
        }
        while self.idempotency_order.len() > MAX_IDEMPOTENCY_TOKENS {
            if let Some(oldest) = self.idempotency_order.pop_front() {
                self.idempotency_tokens.remove(&oldest);
            }
        }

Each of the four copies is reached from a handler: src/ops/delete_billing_group.rs:206 (`let removed = state.remove_billing_group(&group_arn);`), src/ops/delete_custom_line_item.rs:192 (`state.remove_custom_line_item(&item_arn);`), src/ops/delete_pricing_rule.rs:210 (`state.remove_pricing_rule(&rule_arn);`) and src/ops/delete_pricing_plan.rs:259 (`state.remove_pricing_plan(&plan_arn);`).

Test coverage that pins the behaviour the consolidation must leave unchanged: tests/all/billing_group_core_loop.rs:1398-1424 — "RULE: a `ClientToken` stops naming a resource that no longer exists — deleting a ..." (`a_deleted_group_releases_its_client_token`), and tests/all/pricing_rule_lifecycle.rs:627-668 — `a_deleted_rule_frees_its_name_and_its_client_token`, whose rows are "the same ClientToken as the deleted rule" and "no ClientToken at all". tests/all/delete_custom_line_item.rs and tests/all/pricing_plan_delete.rs drive the other two removals.

## Steps to Reproduce

1. Open services/service-com-amazonaws-billingconductor-awsbillingconductor/src/state.rs and read lines 3530-3533, 3601-3604, 3794-3797 and 3813-3816: the same two statements (prune `idempotency_tokens` by removed ARN, then prune `idempotency_order` to the keys still in the map) appear four times, differing only in the name of the ARN variable.
2. Read src/state.rs:2990-2996 for the invariant the pair maintains and src/state.rs:3106-3117 for the insert half, which is written once in `record_idempotent_arn` — so the single-place home for this pair already exists.
3. Confirm each copy is reachable from a handler: src/ops/delete_billing_group.rs:206, src/ops/delete_custom_line_item.rs:192, src/ops/delete_pricing_rule.rs:210, src/ops/delete_pricing_plan.rs:259.
4. Collapse the four copies into one private `release_idempotent_arns(&mut self, arn: &str)` on `ComAmazonawsBillingconductorAwsbillingconductorState` and call it at each of the four sites.
5. Run tests/all/billing_group_core_loop.rs (`a_deleted_group_releases_its_client_token`), tests/all/pricing_rule_lifecycle.rs (`a_deleted_rule_frees_its_name_and_its_client_token`), tests/all/delete_custom_line_item.rs and tests/all/pricing_plan_delete.rs: every response and every assertion is unchanged.

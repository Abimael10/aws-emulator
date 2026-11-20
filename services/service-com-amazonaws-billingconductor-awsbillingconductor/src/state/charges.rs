// SPDX-License-Identifier: CC0-1.0

//! What a billing group's custom line items put on its bill in one billing period: the
//! charge each item carries there, worked out over the resource associations a percentage
//! item is a share of, and the per-product pro forma amounts a cost report answers with.
//!
//! One concern, so the settlement of the association graph lives beside the only reads
//! that need it and each item's charge is worked out exactly once per billing period.

use super::*;

impl ComAmazonawsBillingconductorAwsbillingconductorState {
    /// What the custom line items of one billing group put on its PRO FORMA bill in
    /// `billing_period`, summed per product and SIGNED by each item's charge type.
    ///
    /// A custom line item is an adjustment to what the billing group is charged — "a
    /// one-time fixed charge that can be applied to a single billing group", either "a
    /// fee or discount" — so it is part of the pro forma cost and of nothing else. Every
    /// item of the group whose own billing period range covers the period contributes its
    /// charge with the sign of its type ([`charge_sign`]), under the product it is
    /// presented as ([`custom_line_item_product_name`]). An empty map is a billing group
    /// with no item in effect in that period, so its pro forma cost there is its actual
    /// cost.
    ///
    /// The charges are the ones [`Self::custom_line_item_charges`] settles for the whole
    /// group at once, so each item's charge is worked out exactly ONCE per billing period
    /// and the amount reported is decided by the items and associations the store holds
    /// rather than by the order they are read in.
    pub fn proforma_custom_line_item_charges(
        &self,
        group_arn: &str,
        billing_period: &str,
    ) -> BTreeMap<String, f64> {
        let settled = self.custom_line_item_charges(group_arn, billing_period);
        let mut charges: BTreeMap<String, f64> = BTreeMap::new();
        for item in self.custom_line_items.values() {
            if item.billing_group_arn != group_arn || !item.covers(billing_period) {
                continue;
            }
            let charge = charge_sign(&item.charge_type)
                * settled.get(item.arn.as_str()).copied().unwrap_or(0.0);
            *charges
                .entry(custom_line_item_product_name(item).to_string())
                .or_insert(0.0) += charge;
        }
        charges
    }

    /// The UNSIGNED charge each custom line item of one billing group carries in
    /// `billing_period`, by item ARN — the amount its charge details describe, the item's
    /// type being what decides which way it moves the bill.
    ///
    /// "It should contain only one of `Flat` or `Percentage`": a flat item's charge is its
    /// `ChargeValue`, and a percentage item's is its `PercentageValue` "multiplied against
    /// the combined value of its associated resources", counting only the resources whose
    /// association is in effect in that period. An associated resource is another custom
    /// line item, whose own charge it contributes, or a billing group, which contributes
    /// the metered usage of its accounts — of which this emulator records none, as the
    /// `AWSCost` its cost reports answer with says; an ARN naming neither contributes
    /// nothing.
    ///
    /// Which item lends its charge to which is a graph a CALLER arranges, and nothing
    /// stops two percentage items from being associated to each other: an item the
    /// associations lead back round to is a percentage of its OWN charge, which no charge
    /// of the period settles, so it has no charge there and lends none — the answer that
    /// is a property of the items and associations the store holds rather than of the
    /// order they are walked in. Every other item's charge is settled from the items it
    /// is a percentage of, which the components [`settle_charges`] walks in put first.
    ///
    /// Each item's charge is therefore worked out once per billing period and each
    /// association followed once, whatever shape the caller gave the graph, and the walk
    /// is kept on the HEAP rather than on the native call stack, whose depth a long chain
    /// of associations would otherwise decide.
    ///
    /// The charge values are the ones of the GENERATION of the item that applies in that
    /// billing period ([`CustomLineItemRecord::effective_in`]), so a bill of a period a
    /// later write did not claim is computed from what the item charged then — and an
    /// item NO generation of which applies in the period charges nothing there, however
    /// it is reached, so a percentage item associated to it is lent nothing by it.
    pub(super) fn custom_line_item_charges<'a>(
        &'a self,
        group_arn: &str,
        billing_period: &str,
    ) -> BTreeMap<&'a str, f64> {
        // The items this group's charges are worked out over: its own items, and every
        // stored item those reach through the associations in effect in the period. Each
        // is addressed by its index here, and the resources a percentage item's charge is
        // taken of wait in `associated_arns` until they have been resolved to the items
        // among them.
        let mut index_of: BTreeMap<&'a str, usize> = BTreeMap::new();
        let mut items: Vec<ChargeInput<'a>> = Vec::new();
        let mut associated_arns: Vec<Vec<&'a str>> = Vec::new();
        for item in self.custom_line_items.values() {
            if item.billing_group_arn != group_arn {
                continue;
            }
            let (input, associated) = charge_input(item, billing_period);
            index_of.insert(input.arn, items.len());
            items.push(input);
            associated_arns.push(associated);
        }
        let mut cursor = 0;
        while cursor < items.len() {
            for arn in std::mem::take(&mut associated_arns[cursor]) {
                // A resource that is no stored custom line item of this store lends
                // nothing, so it is no part of the graph at all.
                let Some(associated) = self.custom_line_items.get(arn) else {
                    continue;
                };
                let lender = match index_of.get(associated.arn.as_str()) {
                    Some(lender) => *lender,
                    None => {
                        let (input, reached) = charge_input(associated, billing_period);
                        let lender = items.len();
                        index_of.insert(input.arn, lender);
                        items.push(input);
                        associated_arns.push(reached);
                        lender
                    }
                };
                items[cursor].lenders.push(lender);
            }
            cursor += 1;
        }
        let charges = settle_charges(&items);
        items
            .iter()
            .zip(charges)
            .map(|(item, charge)| (item.arn, charge))
            .collect()
    }
}

/// One custom line item in the graph a billing group's charges are worked out over: how
/// its own charge in that billing period is settled, and which items of the graph lend it
/// theirs.
#[derive(Debug)]
pub(super) struct ChargeInput<'a> {
    /// The item's full ARN, the identity this service's maps key it by.
    pub(super) arn: &'a str,
    /// The amount the item carries ITSELF, for an item whose charge needs no other item:
    /// a flat item's `ChargeValue`, and nothing at all for one no generation of which
    /// applies in the period.
    pub(super) amount: f64,
    /// `PercentageValue`, for a percentage item: its charge is that percentage of the
    /// combined charge of its [`lenders`](Self::lenders).
    pub(super) percentage: Option<f64>,
    /// The items this one's charge is a percentage of, by index into the graph — one per
    /// association in effect in the period that names a stored custom line item.
    pub(super) lenders: Vec<usize>,
}

/// How ONE custom line item's charge in `billing_period` is settled, beside the resources
/// a percentage item's charge is taken of, as the associations in effect in that period
/// name them.
pub(super) fn charge_input<'a>(
    item: &'a CustomLineItemRecord,
    billing_period: &str,
) -> (ChargeInput<'a>, Vec<&'a str>) {
    let settled = |amount: f64, percentage: Option<f64>| ChargeInput {
        arn: item.arn.as_str(),
        amount,
        percentage,
        lenders: Vec::new(),
    };
    let Some(version) = item.effective_in(billing_period) else {
        // No generation of this item applies in the billing period, so the item charges
        // nothing there — the same answer a read scoped to that period gives, which
        // reports no version of it at all. It is reachable here for an ASSOCIATED item
        // only: the group's own items enter the graph whatever period they apply in, and
        // a report counts only the ones in effect. The live charge values are the ones of
        // a generation the period does not hold, so they are no charge of this period and
        // lend a percentage item associated to this one nothing.
        return (settled(0.0, None), Vec::new());
    };
    let details = version.charge_details;
    let Some(percentage) = details.percentage.filter(|_| details.flat.is_none()) else {
        // A flat item's charge is its own `ChargeValue`, and it lends that whatever else
        // the store holds. An item carrying neither charge member is one no create of
        // this service can leave behind, since exactly one of them is required.
        return (
            settled(details.flat.map_or(0.0, |flat| flat.charge_value), None),
            Vec::new(),
        );
    };
    let associated = item
        .associated_resources
        .iter()
        .filter(|association| association.covers(billing_period))
        .map(|association| association.arn.as_str())
        .collect();
    (settled(0.0, Some(percentage.percentage_value)), associated)
}

/// The charge every item of one billing group's graph carries, settled in an order where
/// the items a percentage is taken of are settled before it.
///
/// That order is the one Tarjan's strongly connected components algorithm closes the
/// components of a directed graph in — when a component closes, every component it draws
/// from has closed already — walked ITERATIVELY on the heap, since how deep the graph runs
/// is the caller's to choose. A component holding more than one item, or an item
/// associated to itself, is a set of items the associations lead back round to: each would
/// be a percentage of its own charge, which no charge of the period settles, so each
/// charges nothing. Every other component is one item, settled from the amount it carries
/// itself or as its percentage of what its lenders carry.
///
/// Each item is closed once and each association followed once, so one billing period's
/// charges cost the graph the store holds — at most [`MAX_CUSTOM_LINE_ITEMS`] items of at
/// most [`MAX_RESOURCES_PER_CUSTOM_LINE_ITEM`] associations each — however the caller
/// arranged it.
pub(super) fn settle_charges(items: &[ChargeInput<'_>]) -> Vec<f64> {
    let count = items.len();
    let mut charges: Vec<f64> = vec![0.0; count];
    // Tarjan's bookkeeping: when each item was reached, the oldest item its own walk can
    // reach, and whether it is waiting on the stack of a component that has not closed.
    let mut reached_at: Vec<Option<usize>> = vec![None; count];
    let mut oldest_reached: Vec<usize> = vec![0; count];
    let mut awaiting_component: Vec<bool> = vec![false; count];
    let mut component_stack: Vec<usize> = Vec::new();
    // The items whose lenders are being reached, each with how many of them have been
    // reached so far — the walk itself, on the heap.
    let mut walk: Vec<(usize, usize)> = Vec::new();
    let mut reached = 0;
    for root in 0..count {
        if reached_at[root].is_some() {
            continue;
        }
        reached_at[root] = Some(reached);
        oldest_reached[root] = reached;
        reached += 1;
        component_stack.push(root);
        awaiting_component[root] = true;
        walk.push((root, 0));
        while let Some((item, cursor)) = walk.pop() {
            if let Some(&lender) = items[item].lenders.get(cursor) {
                walk.push((item, cursor + 1));
                match reached_at[lender] {
                    None => {
                        reached_at[lender] = Some(reached);
                        oldest_reached[lender] = reached;
                        reached += 1;
                        component_stack.push(lender);
                        awaiting_component[lender] = true;
                        walk.push((lender, 0));
                    }
                    // A lender already reached and still awaiting a component is one this
                    // walk came through, so the two are in one component: the item can
                    // reach back as far as that lender was reached.
                    Some(lender_reached_at) => {
                        if awaiting_component[lender] {
                            oldest_reached[item] = oldest_reached[item].min(lender_reached_at);
                        }
                    }
                }
                continue;
            }
            // Every lender of this item has been reached. The item heads a component when
            // nothing its walk reached is older than the item itself.
            if Some(oldest_reached[item]) == reached_at[item] {
                let mut component: Vec<usize> = Vec::new();
                while let Some(member) = component_stack.pop() {
                    awaiting_component[member] = false;
                    component.push(member);
                    if member == item {
                        break;
                    }
                }
                // A component of SEVERAL items, or one item associated to itself, is a
                // set the associations lead back round to: each of its items would be a
                // percentage of its own charge, so none of them has a charge in this
                // period — the nothing every charge here starts as. A component of one
                // item the associations do not lead back to is settled from the amount the
                // item carries itself, or as its percentage of what its lenders carry,
                // which the components they head settled before this one.
                if component.len() == 1 && !items[item].lenders.contains(&item) {
                    charges[item] = match items[item].percentage {
                        None => items[item].amount,
                        Some(percentage) => {
                            let combined: f64 = items[item]
                                .lenders
                                .iter()
                                .map(|lender| charges[*lender])
                                .sum();
                            percentage / 100.0 * combined
                        }
                    };
                }
            }
            // The item the walk came from reaches whatever this one reaches.
            if let Some(&(came_from, _)) = walk.last() {
                oldest_reached[came_from] = oldest_reached[came_from].min(oldest_reached[item]);
            }
        }
    }
    charges
}

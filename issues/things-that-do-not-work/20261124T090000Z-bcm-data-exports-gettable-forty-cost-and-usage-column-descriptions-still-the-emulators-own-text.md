Issue-Source: trace-sweep-divergence
Issue-Trace: bcm-data-exports-catalog-get-table
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bcm-data-exports-catalog-get-table
Issue-Shape-Id: com.amazonaws.bcmdataexports#AWSBillingAndCostManagementDataExports
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="cost_category"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="discount"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="product"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="resource_tags"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="tags"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="pricing_public_on_demand_cost"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="pricing_public_on_demand_rate"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="pricing_rate_code"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="product_instance_family"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="product_instancesku"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="product_to_region_code"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_amortized_upfront_cost_for_usage"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_amortized_upfront_fee_for_billing_period"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_effective_cost"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_net_amortized_upfront_cost_for_usage"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_net_amortized_upfront_fee_for_billing_period"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_net_effective_cost"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_net_unused_amortized_upfront_fee_for_billing_period"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_net_unused_recurring_fee"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_net_upfront_value"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_recurring_fee_for_usage"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_reservation_a_r_n"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_subscription_id"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_total_reserved_normalized_units"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_total_reserved_units"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_units_per_reservation"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_unused_amortized_upfront_fee_for_billing_period"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_unused_normalized_unit_quantity"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_unused_recurring_fee"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="reservation_upfront_value"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_amortized_upfront_commitment_for_billing_period"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_instance_type_family"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_net_amortized_upfront_commitment_for_billing_period"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_net_recurring_commitment_for_billing_period"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_net_savings_plan_effective_cost"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_payment_option"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_recurring_commitment_for_billing_period"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_region"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_total_commitment_to_date"].Description
Issue-Key: bcm-data-exports|GetTable|2xx->2xx|$.Schema[Name="savings_plan_used_commitment"].Description
Issue-Related: 20261118T095342Z-bcm-data-exports-gettable-serves-paraphrased-column-descriptions.md

# bcm-data-exports GetTable: forty COST_AND_USAGE_REPORT column descriptions are still the emulator's own text, and no report quotes the dictionary's

> _Split out of `issues/things-that-now-work/20261118T095342Z-bcm-data-exports-gettable-serves-paraphrased-column-descriptions.md`, whose causes 1, 2 and 4 (the carbon emissions methodology version, the cost-and-usage dashboard view, the FOCUS 1.2 schema) now replay clean, and whose cause 3 is closed for every column the parent report quotes AWS's text for. This report carries the columns of cause 3 that are NOT closed, re-filed so they are not recorded as working. Everything below is what the parent fix established about them; a later fixer starts from here rather than from scratch._

## Operation
GetTable

## Description
The parent fix landed the rule and every column it had evidence for: a column's `Description` is the Data Exports table dictionary's entry for THAT table's schema, carried over as published — quirks, typos, trailing clauses and the presence or absence of an example included — and where two schemas publish an entry for the same fact (two unrelated tables, a curated view and the table it views, two versions of one specification), each schema reports its OWN entry. After it, the fixture's four diverging steps are down to ONE (task#3): task#1, task#2 and task#7 replay clean, and 20 of the parent's 60 replay keys are fixed.

What remains is CONTENT this emulator has no source for: the published entry for 40 columns of the `COST_AND_USAGE_REPORT` schema. The parent report lists those 40 as diverging but quotes AWS's expected value for none of them (its "Reported examples" cover only the seven `product_*` entries, all now served as recorded), `replay_issue_units` answers `recorded values withheld` for the step, and the strings are in no file of the checkout. No rule over the model, the request or stored state yields them: the Smithy model carries no per-column text at all (`smithy_query` shape `Column`, member `Description` — "The description for a column." — `GenericString`, length 0..1024, pattern `^[\S\s]*$`), so each entry is fixed per-column content of AWS's dictionary.

=== Cause 1 of 1: forty cost-and-usage column entries are the emulator's own wording ===
AWS: recorded behavior only. `GetTable(TableName=COST_AND_USAGE_REPORT)` answers 200 with `Schema: ColumnList`, each `Column{Name, Type, Description}` carrying the dictionary's published entry for that column. The entry is fixed text per column — nothing in it comes from the request or from state — so it is content, not a per-run value.
Emulator: on `bcm-data-exports-catalog-get-table#task#3` the status, the table name, the table properties, the column SET and every `Type` match, and these 40 `Description` values are the catalog's own wording instead of the published entry:
- the five key-value columns `cost_category`, `discount`, `product`, `resource_tags`, `tags`;
- the three pricing columns `pricing_public_on_demand_cost`, `pricing_public_on_demand_rate`, `pricing_rate_code`;
- the three product attributes `product_instance_family`, `product_instancesku`, `product_to_region_code`;
- the 19 `reservation_*` columns listed in this report's keys (every one but `reservation_availability_zone`, `reservation_end_time`, `reservation_modification_status`, `reservation_net_recurring_fee_for_usage`, `reservation_normalized_units_per_reservation`, `reservation_number_of_reservations`, `reservation_start_time`, `reservation_unused_quantity`, which replay clean);
- the 10 `savings_plan_*` columns listed in this report's keys (every one but `savings_plan_end_time`, `savings_plan_offering_type`, `savings_plan_purchase_term`, `savings_plan_savings_plan_a_r_n`, `savings_plan_savings_plan_effective_cost`, `savings_plan_savings_plan_rate`, `savings_plan_start_time`, which replay clean).
Why: this is the largest and most-consumed table of the catalog, so any client that generates a data dictionary, a column mapping, a schema hash or a drift report out of `GetTable` produces output that does not match AWS for these columns.
Fix: this service's state DATA — the `COST_AND_USAGE_REPORT_COLUMNS` group in `services/service-com-amazonaws-bcmdataexports-awsbillingandcostmanagementdataexports/src/state/catalog.rs`, the `description` of those 40 entries. Nothing in the mechanism needs changing: `src/ops/get_table.rs` only looks the table up and returns `table.schema(&properties)`, the parent fix split the per-dictionary entries that were being shared across schemas, and `ColumnGroup::View`/`ColumnGroup::Revised` already let a schema publish its own entry for a column it carries over.
What a fixer needs: the recorded `Schema` of that step — each of the 40 columns' `Name` with its `Description`. Writing a plausible sentence instead is what this report exists to prevent, and the parent fix measured that: it tried exactly one reconstruction (restoring an `A map column containing key-value pairs of …` lead-in to the five key-value columns, inferred from the FOCUS 1.2 `x_Discounts` entry the parent report DOES quote and which that fix landed), replay showed all five keys still failing, and the guess was reverted. Those five entries, and the other 35, are left as they were under a comment in `catalog.rs` recording that their published text is still owed.
Traces: bcm-data-exports-catalog-get-table#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from the parent fix: verdict failed, 1 diverged step across 1 graded fixture (down from 4), 20 of the parent's 60 keys fixed, `bound_step` `bcm-data-exports-catalog-get-table#task#1` reported `not_among_divergences`, `other_causes_count: 0`. The reported divergence is

```
GetTable (bcm-data-exports-catalog-get-table#task#3): HTTP 200 -> HTTP 200; differs at
  $.Schema[Name="cost_category"].Description,
  $.Schema[Name="discount"].Description,
  $.Schema[Name="pricing_public_on_demand_cost"].Description,
  $.Schema[Name="pricing_public_on_demand_rate"].Description,
  $.Schema[Name="pricing_rate_code"].Description,
  $.Schema[Name="product"].Descript…
```

and the key list adds the remaining columns named above. Every path of task#1, task#2 and task#7 now matches, and the recorded values themselves are withheld by the replay.

## Steps to Reproduce
1. git clone the project repo and build/boot the emulator from this checkout.
2. Replay trace set simparity-wire-aws-v1 against it.
3. Replay fixture bcm-data-exports-catalog-get-table step task#3 and read the recorded response: for each of the 40 columns named in this report's keys, take the `Description` the recording carries for that `Name`.
4. Write those values into the matching `col(...)` entries of `COST_AND_USAGE_REPORT_COLUMNS` in `src/state/catalog.rs`, and remove the comment that records the text as owed. Add a row per column to the published-text table in `tests/all/catalog_published_text.rs`, where the columns whose text is already in hand are pinned.
5. Re-replay task#3: it must carry no diverging path. task#1, task#2, task#4, task#5, task#6 and task#7 must stay clean.

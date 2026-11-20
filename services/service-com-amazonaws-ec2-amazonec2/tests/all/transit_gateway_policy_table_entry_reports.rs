// SPDX-License-Identifier: CC0-1.0

//! The REPORT half of the transit gateway policy table entry surface:
//! `GetTransitGatewayPolicyTableEntries`, driven over the wire the service actually speaks —
//! `ec2Query`, an `application/x-www-form-urlencoded` POST whose body carries `Action=…`,
//! answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: the report answers the entries the named policy table HOLDS, in the
//! order their rule NUMBERS are evaluated in, cut into pages of the size the request asks for
//! within the range the model publishes, narrowed by the filters it carries — each filter's
//! values joined with an OR and matched with EC2's own `*`/`?` wildcards, the filters joined
//! with an AND — and it writes nothing, so every later caller sees exactly what it saw.
//!
//! The suite beside this one (`transit_gateway_policy_table_entries`) pins what one entry
//! round trips as, which refusals the write half answers and that one table's entries are not
//! another's. What is pinned HERE is the reporting rule itself over a SET of inputs: every
//! published page-size bound and one past each of them, a paged walk at four page sizes, and
//! ten filter forms — OR over values, both wildcards, an escaped wildcard, a filter with no
//! value at all, case sensitivity, and two filters at once. Nothing is seeded through the
//! store: the transit gateway, its route table, the policy table and every entry are created
//! through the operations themselves.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, emulator, filters, owned};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refused request with.
const REFUSED_STATUS: u16 = 400;

/// One call of `action` signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// The transit gateway, route table and policy table the entries of one test live in, each
/// minted by the operation that mints one rather than by reaching into the store.
struct Table {
    router: MultiServiceRouter,
    policy_table_id: String,
    route_table_id: String,
}

/// A fresh emulator holding one transit gateway, one of its route tables and one policy table
/// of it.
async fn policy_table() -> Table {
    let (router, _engine) = emulator();

    let (status, body) = invoke(&router, "CreateTransitGateway", &[]).await;
    assert_eq!(
        status, 200,
        "the transit gateway must be created, got {status} with {body}"
    );
    let transit_gateway_id =
        element(&body, "transitGatewayId").expect("the created transit gateway's id");

    let (status, body) = invoke(
        &router,
        "CreateTransitGatewayRouteTable",
        &[param("TransitGatewayId", &transit_gateway_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the route table must be created, got {status} with {body}"
    );
    let route_table_id =
        element(&body, "transitGatewayRouteTableId").expect("the created route table's id");

    let (status, body) = invoke(
        &router,
        "CreateTransitGatewayPolicyTable",
        &[param("TransitGatewayId", &transit_gateway_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the policy table must be created, got {status} with {body}"
    );
    let policy_table_id =
        element(&body, "transitGatewayPolicyTableId").expect("the created policy table's id");

    Table {
        router,
        policy_table_id,
        route_table_id,
    }
}

/// Create one entry of `table` at `rule_number`, describing `rule` through the
/// `PolicyRule.<member>` fields ec2Query flattens a nested structure into.
async fn create_entry(table: &Table, rule_number: &str, rule: &[(&str, &str)]) {
    let mut params = vec![
        param("TransitGatewayPolicyTableId", &table.policy_table_id),
        param("PolicyRuleNumber", rule_number),
        param("TargetRouteTableId", &table.route_table_id),
    ];
    params.extend(
        rule.iter()
            .map(|(member, value)| param(&format!("PolicyRule.{member}"), value)),
    );
    let (status, body) = invoke(
        &table.router,
        "CreateTransitGatewayPolicyTableEntry",
        &params,
    )
    .await;
    assert_eq!(
        status, 200,
        "entry {rule_number}: the create must succeed, got {status} with {body}"
    );
}

/// One read of the table, with `extra` beside the table the request names.
async fn read(table: &Table, extra: &[Param]) -> (u16, String) {
    let mut params = vec![param(
        "TransitGatewayPolicyTableId",
        &table.policy_table_id,
    )];
    params.extend_from_slice(extra);
    invoke(
        &table.router,
        "GetTransitGatewayPolicyTableEntries",
        &params,
    )
    .await
}

/// One reported entry as the assertions below compare it: its rule number, the state it is
/// in, the protocol its rule matches and the route table it targets.
type ReportedEntry = (String, String, String, String);

/// The entries a response reports, in the order it listed them — empty for a body that
/// carries no entry set at all, which is what a refused request answers.
fn reported(case: &str, body: &str) -> Vec<ReportedEntry> {
    let parsed = document(case, body);
    let root = parsed.root_element();
    let Some(set) = child(root, "transitGatewayPolicyTableEntries") else {
        return Vec::new();
    };
    set.children()
        .filter(roxmltree::Node::is_element)
        .map(|item| {
            let rule = child(item, "policyRule");
            (
                child_text(item, "policyRuleNumber").unwrap_or_default(),
                child_text(item, "state").unwrap_or_default(),
                rule.and_then(|rule| child_text(rule, "protocol"))
                    .unwrap_or_default(),
                child_text(item, "targetRouteTableId").unwrap_or_default(),
            )
        })
        .collect()
}

/// The rule numbers a response reported, in the order it listed them.
fn reported_rule_numbers(case: &str, body: &str) -> Vec<String> {
    reported(case, body)
        .into_iter()
        .map(|(rule_number, _, _, _)| rule_number)
        .collect()
}

/// The rule numbers the paging tests create their entries at, written out of order so the
/// report's own ordering is what puts them in order, and spanning a decade boundary so an
/// order read off the TEXT of a number would differ from the one read off its value.
const UNORDERED_RULE_NUMBERS: [&str; 13] = [
    "64", "1", "7", "100", "65535", "2", "9", "30", "11", "300", "3", "8", "5000",
];

/// Those rule numbers as the report must list them: by the NUMBER each names, so 11 follows 9
/// and 100 follows 64 — which comparing them as text would not give.
const ORDERED_RULE_NUMBERS: [&str; 13] = [
    "1", "2", "3", "7", "8", "9", "11", "30", "64", "100", "300", "5000", "65535",
];

/// Create [`UNORDERED_RULE_NUMBERS`] as the entries of one fresh policy table.
async fn thirteen_entries() -> Table {
    let table = policy_table().await;
    for rule_number in UNORDERED_RULE_NUMBERS {
        create_entry(&table, rule_number, &[("Protocol", "6")]).await;
    }
    table
}

/// The page size every request that names none is answered with: the largest the model
/// publishes (`TransitGatewayMaxResults`, `range 5..1000`), so the whole table comes back.
const WHOLE_TABLE: usize = 1000;

/// The report holds `MaxResults` to the range the model publishes for it —
/// `TransitGatewayMaxResults`, `range 5..1000`
/// (`smithy_query(query="shape",shape="TransitGatewayMaxResults",model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")`)
/// — serving each published bound and a value inside it, refusing one past each bound and two
/// values no page size can take, and reporting nothing for the ones it refuses.
#[tokio::test]
async fn the_report_holds_max_results_to_its_published_range() {
    // Each row is a page size and whether the operation admits it: at each published bound,
    // inside the range, one past each bound, and two values no page size can take.
    let rows: [(&str, &str, bool); 7] = [
        ("one below the published minimum", "4", false),
        ("the published minimum", "5", true),
        ("inside the published range", "640", true),
        ("the published maximum", "1000", true),
        ("one above the published maximum", "1001", false),
        ("a page of none", "0", false),
        ("a negative page size", "-1", false),
    ];
    let table = thirteen_entries().await;
    let ordered = owned(&ORDERED_RULE_NUMBERS);

    for (case, max_results, admitted) in rows {
        let (status, body) = read(&table, &[param("MaxResults", max_results)]).await;
        if admitted {
            assert_eq!(
                status, 200,
                "case {case:?}: MaxResults={max_results} is inside the published range, got \
                 {status} with {body}"
            );
            let size = max_results
                .parse::<usize>()
                .unwrap_or_else(|_| panic!("case {case:?}: an admitted page size is a number"));
            let page = size.min(ordered.len());
            assert_eq!(
                reported_rule_numbers(case, &body),
                ordered[..page].to_vec(),
                "case {case:?}: expected the first {page} entries in rule-number order, got \
                 {body}"
            );
            assert_eq!(
                element(&body, "nextToken").is_some(),
                page < ordered.len(),
                "case {case:?}: a token follows exactly when an entry does, got {body}"
            );
        } else {
            assert_eq!(
                status, REFUSED_STATUS,
                "case {case:?}: MaxResults={max_results} is outside the published range, got \
                 {status} with {body}"
            );
            assert_eq!(
                reported(case, &body),
                Vec::new(),
                "case {case:?}: a refused request reports no entry, got {body}"
            );
        }
    }

    // The refusals changed nothing a later caller sees.
    let (status, body) = read(&table, &[]).await;
    assert_eq!(
        status, 200,
        "the report must succeed after the page sizes, got {status} with {body}"
    );
    assert_eq!(
        reported_rule_numbers("after the page sizes", &body),
        ordered,
        "expected the table exactly as it was, got {body}"
    );
}

/// Walk every page of `table` at `page_size`, following the token each page names, and answer
/// the rule numbers seen in order beside how many pages it took.
async fn walk(table: &Table, case: &str, page_size: usize) -> (Vec<String>, usize) {
    let mut seen: Vec<String> = Vec::new();
    let mut pages = 0usize;
    let mut token: Option<String> = None;
    loop {
        let mut extra = vec![param("MaxResults", &page_size.to_string())];
        if let Some(value) = &token {
            extra.push(param("NextToken", value));
        }
        let (status, body) = read(table, &extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: page {pages} must succeed, got {status} with {body}"
        );
        let page = reported_rule_numbers(case, &body);
        assert!(
            !page.is_empty(),
            "case {case:?}: page {pages} must carry at least one entry, got {body}"
        );
        assert!(
            page.len() <= page_size,
            "case {case:?}: page {pages} carries {} entries, above the {page_size} asked for, \
             got {body}",
            page.len()
        );
        seen.extend(page);
        pages += 1;
        token = element(&body, "nextToken");
        match &token {
            None => break,
            Some(value) => {
                // "The token to use to retrieve the next page of results" is opaque, so it is
                // not the position it names written out for a caller to assemble one from.
                assert!(
                    value.parse::<i64>().is_err(),
                    "case {case:?}: page {pages}: a token must not be the rule number it \
                     names, got {value:?}"
                );
                // A token that never clears would page forever: one page per entry is more
                // than any page size of this table can take.
                assert!(
                    pages <= UNORDERED_RULE_NUMBERS.len(),
                    "case {case:?}: the walk took {pages} pages over {} entries, so the token \
                     is not advancing",
                    UNORDERED_RULE_NUMBERS.len()
                );
            }
        }
    }
    (seen, pages)
}

/// A walk at any admitted page size follows the tokens to the end and reports every entry of
/// the table exactly once, in rule-number order — whether the page is smaller than the table,
/// exactly its size, or larger.
#[tokio::test]
async fn a_paged_walk_reports_every_entry_exactly_once_in_rule_number_order() {
    let count = UNORDERED_RULE_NUMBERS.len();
    // The page sizes span what the rule generalises over: the smallest the model admits, one
    // above it, a size that divides the table unevenly, exactly the table's size, and the
    // largest page published.
    let rows: [(&str, usize); 5] = [
        ("the smallest published page", 5),
        ("one above the smallest", 6),
        ("a page that does not divide the table", 7),
        ("a page exactly the size of the table", count),
        ("the largest published page", WHOLE_TABLE),
    ];
    let table = thirteen_entries().await;
    let ordered = owned(&ORDERED_RULE_NUMBERS);

    for (case, page_size) in rows {
        let (seen, pages) = walk(&table, case, page_size).await;
        assert_eq!(
            seen, ordered,
            "case {case:?}: expected every entry exactly once in rule-number order, got {seen:?}"
        );
        let expected_pages = count.div_ceil(page_size);
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} pages of {page_size} over {count} \
             entries, got {pages}"
        );
    }
}

/// One filtering case: what distinguishes it, the `Filters` entries the request carries, and
/// the rule numbers the report must then list.
type FilterCase<'a> = (&'a str, &'a [(&'a str, &'a [&'a str])], &'a [&'a str]);

/// The entries the filtering test runs over: five protocols of the published catalog and four
/// metadata keys, with two entries carrying no metadata at all — so a filter can select by
/// one attribute, by several values of it, or by a wildcard over it.
async fn entries_to_filter() -> Table {
    let table = policy_table().await;
    let rows: [(&str, &[(&str, &str)]); 6] = [
        ("1", &[("Protocol", "6"), ("MetaData.MetaDataKey", "tenant-blue")]),
        (
            "2",
            &[("Protocol", "17"), ("MetaData.MetaDataKey", "tenant-green")],
        ),
        ("3", &[("Protocol", "47"), ("MetaData.MetaDataKey", "owner")]),
        ("4", &[("Protocol", "*")]),
        ("5", &[("Protocol", "1"), ("MetaData.MetaDataKey", "tenant-red")]),
        // A request that narrows nothing: every criterion of the stored rule then matches
        // anything, which the rule's own members spell `*`.
        ("6", &[]),
    ];
    for (rule_number, rule) in rows {
        create_entry(&table, rule_number, rule).await;
    }
    table
}

/// Within one filter the values are joined with an OR and each is matched case-sensitively
/// with EC2's `*`/`?` wildcards — a backslash escaping either, so a value can select on a
/// literal one — and separate filters are joined with an AND.
#[tokio::test]
async fn one_filters_values_are_joined_with_an_or_and_carry_ec2s_wildcards() {
    let rows: [FilterCase<'_>; 10] = [
        (
            "two values of one filter, joined with an OR",
            &[("policy-rule.protocol", &["6", "17"])],
            &["1", "2"],
        ),
        (
            "three values, one of which no entry carries",
            &[("policy-rule.protocol", &["47", "1", "99"])],
            &["3", "5"],
        ),
        (
            "a trailing `*`, matching zero or more characters",
            &[("policy-rule.meta-data.meta-data-key", &["tenant-*"])],
            &["1", "2", "5"],
        ),
        (
            "a `?`, matching exactly one character",
            &[("policy-rule.meta-data.meta-data-key", &["owne?"])],
            &["3"],
        ),
        (
            "a `?` matching one character and no more",
            &[("policy-rule.meta-data.meta-data-key", &["tenant-?"])],
            &[],
        ),
        (
            "a `*` on its own, selecting every entry that carries the attribute",
            &[("policy-rule.meta-data.meta-data-key", &["*"])],
            &["1", "2", "3", "5"],
        ),
        (
            "an escaped wildcard, matching the literal character",
            &[("policy-rule.protocol", &["\\*"])],
            &["4", "6"],
        ),
        (
            "a value matched case-sensitively",
            &[("policy-rule.meta-data.meta-data-key", &["TENANT-*"])],
            &[],
        ),
        (
            "a filter carrying no value at all",
            &[("policy-rule.protocol", &[])],
            &[],
        ),
        (
            "two filters joined with an AND, each matching by a wildcard",
            &[
                ("policy-rule.protocol", &["?"]),
                ("policy-rule.meta-data.meta-data-key", &["tenant-*"]),
            ],
            &["1", "5"],
        ),
    ];
    let table = entries_to_filter().await;

    for (case, entries, expected) in rows {
        let (status, body) = read(&table, &filters(entries)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        assert_eq!(
            reported_rule_numbers(case, &body),
            owned(expected),
            "case {case:?}: expected exactly {expected:?} in rule-number order, got {body}"
        );
    }
}

/// The report is a READ: neither a plain one, a filtered one, a paged one nor a refused one
/// changes what a later caller sees.
#[tokio::test]
async fn the_report_changes_nothing_a_later_caller_sees() {
    let table = thirteen_entries().await;
    let (status, body) = read(&table, &[]).await;
    assert_eq!(
        status, 200,
        "the first report must succeed, got {status} with {body}"
    );
    let before = reported("the first report", &body);
    assert_eq!(
        before.len(),
        UNORDERED_RULE_NUMBERS.len(),
        "the first report must carry every created entry, got {body}"
    );

    // Each row is a read of a different form: the whole table again, a narrowed one, a page
    // of one, a walk's second page, and three a request is refused for.
    let token = {
        let (status, body) = read(&table, &[param("MaxResults", "5")]).await;
        assert_eq!(
            status, 200,
            "the first page must succeed, got {status} with {body}"
        );
        element(&body, "nextToken").expect("a token naming the second page")
    };
    let rows: [(&str, Vec<Param>); 7] = [
        ("the whole table again", Vec::new()),
        (
            "a narrowed report",
            filters(&[("policy-rule.protocol", &["6"])]),
        ),
        ("a page of the table", vec![param("MaxResults", "5")]),
        (
            "the page that token names",
            vec![param("MaxResults", "5"), param("NextToken", &token)],
        ),
        (
            "a token the service did not mint",
            vec![param("NextToken", "not-a-token")],
        ),
        (
            "a page size outside the published range",
            vec![param("MaxResults", "4")],
        ),
        (
            "a filter name the entry has no attribute for",
            filters(&[("policy-rule.source-port", &["443"])]),
        ),
    ];
    for (case, extra) in rows {
        let (status, body) = read(&table, &extra).await;
        assert!(
            status == 200 || status == REFUSED_STATUS,
            "case {case:?}: expected HTTP 200 or {REFUSED_STATUS}, got {status} with {body}"
        );
    }

    let (status, body) = read(&table, &[]).await;
    assert_eq!(
        status, 200,
        "the last report must succeed, got {status} with {body}"
    );
    assert_eq!(
        reported("the last report", &body),
        before,
        "every read must leave the table exactly as it was, got {body}"
    );
}

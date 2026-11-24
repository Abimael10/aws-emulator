// SPDX-License-Identifier: CC0-1.0

//! `ListDomains`, end to end over the service's own wire protocol (`awsJson1_0`,
//! `POST /` with `X-Amz-Target: SimpleWorkflowService.<Operation>`), through the same
//! `MultiServiceRouter` the emulator mounts in production. The harness is the one
//! `workflow_execution_lifecycle` already builds.
//!
//! The rule every case here asserts: the listing answers from the `Domain` records
//! STORED for the account — exactly those whose own `RegistrationStatus` is the
//! requested one — each reported as the same `DomainInfo` the per-domain reader reports,
//! ordered ascending by name, reversed under `reverseOrder`, and cut into pages of
//! `maximumPageSize` with a `nextPageToken` that resumes where the page ended.
//!
//! It is a child of `domain_deprecation` because the status it selects on is the one
//! that suite's `DeprecateDomain`/`UndeprecateDomain` calls move: the listing is how
//! that move becomes observable account-wide. The rows vary what the rule generalises
//! over: the shape and length of the domain name (plain, digits, embedded space,
//! non-ASCII, upper case, the modelled maximum length), the retention period and
//! description each record was registered with, which records are deprecated, which
//! status the request selects on, the page size and the ordering. No row is privileged.

use serde_json::{json, Value};

use crate::workflow_execution_lifecycle::{register_domain_body, string_at, Emulator};

/// The modelled maximum length of `RegisterDomainInput$name`
/// (`smithy_query(query="operation",operation="RegisterDomain")`: `name` targets
/// `DomainName`, len 1..256).
const MAX_DOMAIN_NAME_LEN: usize = 256;

/// The two modelled `RegistrationStatus` values
/// (`smithy_query(query="shape",shape="com.amazonaws.swf#RegistrationStatus")`), which
/// are also the two values `registrationStatus` selects on.
const REGISTERED: &str = "REGISTERED";
const DEPRECATED: &str = "DEPRECATED";

/// One domain the suite registers. `deprecated` rows are deprecated after registration,
/// so both statuses are populated from stored records rather than from the request.
struct SeedDomain {
    name: String,
    /// `workflowExecutionRetentionPeriodInDays`, varied per row so no case depends on
    /// one registered value.
    retention: &'static str,
    /// The optional `description`, so a listed `DomainInfo` is checked over records that
    /// do and do not carry one.
    description: Option<&'static str>,
    deprecated: bool,
}

fn seed_domains() -> Vec<SeedDomain> {
    let row = |name: String, retention, description, deprecated| SeedDomain {
        name,
        retention,
        description,
        deprecated,
    };
    vec![
        row("checkout".into(), "7", Some("the plain case"), false),
        row("checkout-archive".into(), "0", None, true),
        row("7".into(), "1", Some("digits only"), false),
        row("pack items".into(), "90", None, true),
        row("ドメイン".into(), "NONE", Some("unicode"), false),
        row("ZZ-upper".into(), "3", None, false),
        row("zz-lower".into(), "3", None, true),
        row(
            "d".repeat(MAX_DOMAIN_NAME_LEN),
            "30",
            Some("the modelled maximum name length"),
            false,
        ),
    ]
}

/// The names of `domains` with `status`, in the order the listing must report them
/// without `reverseOrder`: ascending by name.
fn expected_names(domains: &[SeedDomain], status: &str) -> Vec<String> {
    let mut selected: Vec<String> = domains
        .iter()
        .filter(|seed| seed_status(seed) == status)
        .map(|seed| seed.name.clone())
        .collect();
    selected.sort();
    selected
}

fn seed_status(seed: &SeedDomain) -> &'static str {
    if seed.deprecated {
        DEPRECATED
    } else {
        REGISTERED
    }
}

/// A `ListDomains` request body. `maximumPageSize` and `nextPageToken` are folded in
/// only when given, so a row can exercise the modelled defaults.
fn list_body(
    status: &str,
    reverse_order: bool,
    page_size: Option<i64>,
    token: Option<&str>,
) -> Value {
    let mut body = json!({ "registrationStatus": status, "reverseOrder": reverse_order });
    let object = body.as_object_mut().expect("a JSON object");
    if let Some(page_size) = page_size {
        object.insert("maximumPageSize".to_string(), json!(page_size));
    }
    if let Some(token) = token {
        object.insert("nextPageToken".to_string(), json!(token));
    }
    body
}

/// The `domainInfos` a listing reported, in the order it reported them.
fn domain_infos(case: &str, body: &Value) -> Vec<Value> {
    body.pointer("/domainInfos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected domainInfos in {body:?}"))
}

/// The domain names a listing reported, in order.
fn names(case: &str, body: &Value) -> Vec<String> {
    domain_infos(case, body)
        .iter()
        .map(|info| string_at(info, "/name"))
        .collect()
}

/// The `nextPageToken` of a listing response, if it issued one.
fn next_token(body: &Value) -> Option<String> {
    body.get("nextPageToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Register one domain, and deprecate it when the row asks for it.
async fn register(emulator: &Emulator, case: &str, seed: &SeedDomain) {
    let mut body = register_domain_body(&seed.name, seed.retention);
    if let Some(description) = seed.description {
        body.as_object_mut()
            .expect("a JSON object")
            .insert("description".to_string(), json!(description));
    }
    emulator.ok(case, "RegisterDomain", &body).await;
    if seed.deprecated {
        emulator
            .ok(case, "DeprecateDomain", &json!({ "name": seed.name }))
            .await;
    }
}

/// Seed one account holding every row of the table.
async fn seed(emulator: &Emulator, domains: &[SeedDomain]) {
    for domain in domains {
        register(emulator, "seed", domain).await;
    }
}

/// The `domainInfo` the per-domain reader reports for one name.
async fn described_info(emulator: &Emulator, case: &str, domain: &str) -> Value {
    let described = emulator
        .ok(case, "DescribeDomain", &json!({ "name": domain }))
        .await;
    described
        .pointer("/domainInfo")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a domainInfo in {described:?}"))
}

#[tokio::test]
async fn a_listing_reports_the_stored_domains_of_the_requested_status() {
    let emulator = Emulator::new();
    let domains = seed_domains();
    seed(&emulator, &domains).await;

    for status in [REGISTERED, DEPRECATED] {
        let case = format!("status {status}");
        let expected = expected_names(&domains, status);
        let response = emulator
            .ok(&case, "ListDomains", &list_body(status, false, None, None))
            .await;
        let reported = names(&case, &response);
        assert_eq!(
            reported, expected,
            "case {case:?}: the listing reports the stored records of that status, \
             ascending by name, got {reported:?}"
        );
        assert_eq!(
            next_token(&response),
            None,
            "case {case:?}: a listing that reported every match issues no nextPageToken, \
             got {response:?}"
        );

        // Each reported info is the record the per-domain reader reports, so nothing in
        // the listing is synthesised for it.
        for (index, name) in expected.iter().enumerate() {
            let described = described_info(&emulator, &case, name).await;
            let listed = &domain_infos(&case, &response)[index];
            assert_eq!(
                listed, &described,
                "case {case:?}: the listed info of {name:?} is the stored record the \
                 reader reports, got {listed:?}"
            );
        }
    }
}

#[tokio::test]
async fn reverse_order_reports_the_same_matches_in_descending_order() {
    let emulator = Emulator::new();
    let domains = seed_domains();
    seed(&emulator, &domains).await;

    for status in [REGISTERED, DEPRECATED] {
        let case = format!("status {status} / reverseOrder");
        let mut expected = expected_names(&domains, status);
        expected.reverse();
        let response = emulator
            .ok(&case, "ListDomains", &list_body(status, true, None, None))
            .await;
        let reported = names(&case, &response);
        assert_eq!(
            reported, expected,
            "case {case:?}: reverseOrder reports the same matches descending, \
             got {reported:?}"
        );
    }
}

#[tokio::test]
async fn paging_a_listing_visits_every_match_exactly_once_in_either_order() {
    let emulator = Emulator::new();
    let domains = seed_domains();
    seed(&emulator, &domains).await;

    let total = domains.len();
    let total_i64 = i64::try_from(total).expect("the table fits in an i64");
    for status in [REGISTERED, DEPRECATED] {
        for reverse_order in [false, true] {
            // Page sizes spanning the class: one at a time, a size that does not divide
            // the match count, a size equal to the whole table and one above it. 0 is
            // the modelled default ("not specified"), which means every match.
            for page_size in [1_i64, 2, 3, total_i64, total_i64 + 1, 0] {
                let case =
                    format!("status {status} / reverseOrder {reverse_order} / page {page_size}");
                let mut expected = expected_names(&domains, status);
                if reverse_order {
                    expected.reverse();
                }

                let mut walked: Vec<String> = Vec::new();
                let mut token: Option<String> = None;
                let mut pages = 0_usize;
                loop {
                    pages += 1;
                    assert!(
                        pages <= total + 2,
                        "case {case:?}: paging must terminate, got {pages} pages after \
                         {walked:?}"
                    );
                    let response = emulator
                        .ok(
                            &case,
                            "ListDomains",
                            &list_body(
                                status,
                                reverse_order,
                                Some(page_size),
                                token.as_deref(),
                            ),
                        )
                        .await;
                    let page = names(&case, &response);
                    if page_size > 0 {
                        let limit = usize::try_from(page_size).expect("a positive page size");
                        assert!(
                            page.len() <= limit,
                            "case {case:?}: a page holds at most maximumPageSize matches, \
                             got {page:?}"
                        );
                    }
                    walked.extend(page);
                    token = next_token(&response);
                    if token.is_none() {
                        break;
                    }
                }

                assert_eq!(
                    walked, expected,
                    "case {case:?}: paging visits every match exactly once, in the \
                     listing's own order, got {walked:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_page_token_is_refused_under_the_other_ordering_or_when_forged() {
    let emulator = Emulator::new();
    let domains = seed_domains();
    seed(&emulator, &domains).await;

    for status in [REGISTERED, DEPRECATED] {
        for reverse_order in [false, true] {
            let case = format!("status {status} / reverseOrder {reverse_order}");
            let first = emulator
                .ok(
                    &case,
                    "ListDomains",
                    &list_body(status, reverse_order, Some(1), None),
                )
                .await;
            let token = next_token(&first)
                .unwrap_or_else(|| panic!("case {case:?}: expected a token in {first:?}"));

            // "Keep all other arguments unchanged": the token carries the ordering it
            // was issued for, so replaying it under the other one is refused rather
            // than silently skipping or repeating domains.
            emulator
                .refused(
                    &case,
                    "ListDomains",
                    &list_body(status, !reverse_order, Some(1), Some(&token)),
                    "ValidationException",
                )
                .await;

            // So is a token this listing never issued: a tag no domain listing stamps,
            // and a payload that is not the hex one it writes.
            for forged in ["not-a-token", "6d660000", "mfzz", "mf7a7"] {
                let forged_case = format!("{case} / forged {forged:?}");
                emulator
                    .refused(
                        &forged_case,
                        "ListDomains",
                        &list_body(status, reverse_order, Some(1), Some(forged)),
                        "ValidationException",
                    )
                    .await;
            }
        }
    }
}

#[tokio::test]
async fn a_status_no_stored_domain_carries_reports_an_empty_list() {
    // An account holding no domain at all, then one holding only REGISTERED records:
    // the listing of a status nothing carries is an empty list, not a fault.
    let emulator = Emulator::new();
    for status in [REGISTERED, DEPRECATED] {
        let case = format!("empty account / status {status}");
        let response = emulator
            .ok(&case, "ListDomains", &list_body(status, false, None, None))
            .await;
        assert_eq!(
            names(&case, &response),
            Vec::<String>::new(),
            "case {case:?}: an account holding no domain reports an empty list, \
             got {response:?}"
        );
        assert_eq!(
            next_token(&response),
            None,
            "case {case:?}: an empty listing issues no nextPageToken, got {response:?}"
        );
    }

    let only_registered: Vec<SeedDomain> = seed_domains()
        .into_iter()
        .filter(|seed| !seed.deprecated)
        .collect();
    seed(&emulator, &only_registered).await;
    let case = "no deprecated record";
    let response = emulator
        .ok(case, "ListDomains", &list_body(DEPRECATED, false, None, None))
        .await;
    assert_eq!(
        names(case, &response),
        Vec::<String>::new(),
        "case {case:?}: a status no stored record carries reports an empty list, \
         got {response:?}"
    );
    // And the records that ARE stored are reported by the other listing, so the empty
    // answer above is a filter over state and not an empty account.
    let registered = emulator
        .ok(case, "ListDomains", &list_body(REGISTERED, false, None, None))
        .await;
    assert_eq!(
        names(case, &registered),
        expected_names(&only_registered, REGISTERED),
        "case {case:?}: the stored records are reported by their own status, \
         got {registered:?}"
    );
}

#[tokio::test]
async fn a_domain_moves_between_the_two_listings_as_its_stored_status_moves() {
    let emulator = Emulator::new();
    let domains = seed_domains();
    seed(&emulator, &domains).await;

    // Each row, whichever status it starts in, is reported by the listing of its stored
    // status and by no other — before and after each move of that one record.
    for seed_domain in &domains {
        let case = format!("moving {:?}", seed_domain.name);
        for (operation, moved_to) in [
            ("DeprecateDomain", DEPRECATED),
            ("UndeprecateDomain", REGISTERED),
        ] {
            let step = format!("{case} / {operation}");
            let already = listing_holds(&emulator, &step, moved_to, &seed_domain.name).await;
            if already {
                continue;
            }
            emulator
                .ok(&step, operation, &json!({ "name": seed_domain.name }))
                .await;
            assert!(
                listing_holds(&emulator, &step, moved_to, &seed_domain.name).await,
                "case {step:?}: the record is reported by the listing of its new status"
            );
            let other = if moved_to == DEPRECATED {
                REGISTERED
            } else {
                DEPRECATED
            };
            assert!(
                !listing_holds(&emulator, &step, other, &seed_domain.name).await,
                "case {step:?}: the record is reported by the {other} listing no longer"
            );
        }
    }
}

/// Whether the `status` listing reports `domain`, read over every page so the answer
/// does not depend on where a page boundary falls.
async fn listing_holds(emulator: &Emulator, case: &str, status: &str, domain: &str) -> bool {
    let mut token: Option<String> = None;
    loop {
        let response = emulator
            .ok(
                case,
                "ListDomains",
                &list_body(status, false, Some(2), token.as_deref()),
            )
            .await;
        if names(case, &response).iter().any(|name| name == domain) {
            return true;
        }
        token = next_token(&response);
        if token.is_none() {
            return false;
        }
    }
}

#[tokio::test]
async fn listing_repeatedly_does_not_change_what_a_later_caller_sees() {
    let emulator = Emulator::new();
    let domains = seed_domains();
    seed(&emulator, &domains).await;

    for status in [REGISTERED, DEPRECATED] {
        let case = format!("status {status}");
        let body = list_body(status, false, None, None);
        let first = emulator.ok(&case, "ListDomains", &body).await;
        for round in 2..=3 {
            let round_case = format!("{case} / read #{round}");
            // A page taken in between must not move the unpaged listing either.
            emulator
                .ok(
                    &round_case,
                    "ListDomains",
                    &list_body(status, false, Some(1), None),
                )
                .await;
            let again = emulator.ok(&round_case, "ListDomains", &body).await;
            assert_eq!(
                again, first,
                "case {round_case:?}: a read reports the same records a later caller \
                 sees, got {again:?}"
            );
        }

        // And the status the listing selected on is still each record's own: a listing
        // moves nothing.
        for name in expected_names(&domains, status) {
            let described = described_info(&emulator, &case, &name).await;
            assert_eq!(
                string_at(&described, "/status"),
                status,
                "case {case:?}: {name:?} keeps its stored status, got {described:?}"
            );
        }
    }
}

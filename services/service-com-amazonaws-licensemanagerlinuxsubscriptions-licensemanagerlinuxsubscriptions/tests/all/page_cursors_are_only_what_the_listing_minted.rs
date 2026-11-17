// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for ONE rule about every paged list of this service, driven over the
//! real wire path:
//!
//! * a `NextToken` is honoured only when THIS store minted it for THIS list: the position a
//!   cursor names travels inside the token, so a value a caller assembled — however exactly
//!   it has the shape, the size and the framing of a minted one — names no position and is
//!   refused with a `ValidationException`, while the cursor a page handed out resumes the
//!   walk after the element that page reported last.
//! * the list a cursor belongs to is the SELECTION the request means, not the spelling it
//!   arrived in: two requests that select the same elements by the same comparisons — an
//!   absent `Filter$Operator` and the equality it stands for, a set of values or of
//!   filters or of sources in another order or with a repeat, an absent and an empty
//!   selection — continue ONE walk, while a request meaning another selection is refused
//!   the cursor.
//!
//! All three paged lists of this service share ONE cursor path
//! (`crate::state::paginate`), so each row below drives it through a different one of them:
//! `ListLinuxSubscriptionInstances` and `ListLinuxSubscriptions`, which page a `Filters`
//! selection of the discovered inventory, and `ListRegisteredSubscriptionProviders`, which
//! pages a `SubscriptionProviderSources` selection of the registrations.
//!
//! The forged values span the forms a cursor a listing never issued takes: one that is not
//! this service's spelling at all, one too short to carry an authenticator, one built with
//! exactly a minted cursor's shape, one minted under another store's secret, one that is a
//! minted cursor with a single byte changed, one that is a minted cursor with the position
//! inside it replaced by another, and one minted by a SIBLING list of the same store.

use serde_json::{json, Value};

use crate::harness::{
    call, enable_discovery, harness, seed_discovered_instance, seed_provider, Harness, Reply,
};
use service_com_amazonaws_licensemanagerlinuxsubscriptions_licensemanagerlinuxsubscriptions::state::{
    AUTHENTICATOR_BYTES, DiscoveredInstanceRecord, PageTokenKey,
};

/// `POST /subscription/ListLinuxSubscriptionInstances` (`OPERATIONS.md`).
const INSTANCES: &str = "/subscription/ListLinuxSubscriptionInstances";
/// `POST /subscription/ListLinuxSubscriptions` (`OPERATIONS.md`).
const SUBSCRIPTIONS: &str = "/subscription/ListLinuxSubscriptions";
/// `POST /subscription/ListRegisteredSubscriptionProviders` (`OPERATIONS.md`).
const PROVIDERS: &str = "/subscription/ListRegisteredSubscriptionProviders";

/// The one Region the fixture records were found in, and the Region the account's settings
/// discover, so the seeded records are the inventory the two discovery lists project.
const REGION: &str = "us-west-2";

/// The discovered instances the walks below page over: three instances carrying three
/// subscription names, so both discovery lists hold three elements and a page size of one
/// truncates twice.
const DISCOVERED: &[(&str, &str)] = &[
    ("i-0a1", "RHEL"),
    ("i-0b2", "SUSE Linux Enterprise Server"),
    ("i-0c3", "Ubuntu Pro"),
];

/// One paged list, as a case drives it: its path, the member its page reports and the member
/// that identifies an element of it.
struct Listing {
    /// What the case prints for this list.
    label: &'static str,
    path: &'static str,
    /// The output member carrying the page.
    items: &'static str,
    /// The member of an entry that identifies it — which is also what a cursor of this list
    /// names.
    identity: &'static str,
}

/// The three paged lists, each the operation of one of the former call sites of the cursor
/// path.
const LISTINGS: &[Listing] = &[
    Listing {
        label: "ListLinuxSubscriptionInstances",
        path: INSTANCES,
        items: "Instances",
        identity: "InstanceID",
    },
    Listing {
        label: "ListLinuxSubscriptions",
        path: SUBSCRIPTIONS,
        items: "Subscriptions",
        identity: "Name",
    },
    Listing {
        label: "ListRegisteredSubscriptionProviders",
        path: PROVIDERS,
        items: "RegisteredSubscriptionProviders",
        identity: "SubscriptionProviderArn",
    },
];

/// An emulator holding what all three lists page over: the discovered inventory the two
/// discovery lists project, and three registrations the provider list reports.
async fn seeded() -> Harness {
    let h = harness();
    enable_discovery(&h, &[REGION]).await;
    for (instance_id, subscription_name) in DISCOVERED {
        seed_discovered_instance(
            &h,
            DiscoveredInstanceRecord {
                instance_id: (*instance_id).to_string(),
                account_id: "000000000000".to_string(),
                ami_id: "ami-0001".to_string(),
                instance_type: "t3.micro".to_string(),
                status: "running".to_string(),
                region: REGION.to_string(),
                usage_operation: "RunInstances:0010".to_string(),
                product_codes: vec!["pc-linux".to_string()],
                subscription_name: (*subscription_name).to_string(),
                subscription_type: "Subscription-included (Amazon EC2)".to_string(),
                ..DiscoveredInstanceRecord::default()
            },
        );
    }
    for index in 0..DISCOVERED.len() {
        seed_provider(
            &h,
            &format!("00000000-0000-4000-8000-{:012x}", 0x3000 + index),
            &format!("arn:aws:secretsmanager:{REGION}:000000000000:secret:rhel-{index}"),
            &[],
        );
    }
    h
}

/// One page of `listing`, with `max_results` as the page size and `token` as the
/// continuation, if any.
async fn page(h: &Harness, listing: &Listing, max_results: i32, token: Option<&str>) -> Reply {
    let mut body = json!({ "MaxResults": max_results });
    if let Some(token) = token {
        body["NextToken"] = json!(token);
    }
    call(h, listing.path, &body).await
}

/// The identities a 200 page of `listing` reports.
fn reported(reply: &Reply, listing: &Listing, case: &str) -> Vec<String> {
    let body = reply.ok(case);
    body.get(listing.items)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no {} in {body:?}", listing.items))
        .iter()
        .map(|entry| {
            entry
                .get(listing.identity)
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: no {} in {entry:?}", listing.identity)
                })
                .to_string()
        })
        .collect()
}

/// The `NextToken` a 200 page carries, if it is truncated.
fn next_token(reply: &Reply, case: &str) -> Option<String> {
    reply
        .ok(case)
        .get("NextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The cursor the FIRST page of `listing` hands out at a page size of one, with the identity
/// that page reported.
async fn first_cursor(h: &Harness, listing: &Listing) -> (String, String) {
    let case = &format!("{}: the first page of a truncated walk", listing.label);
    let reply = page(h, listing, 1, None).await;
    let reported = reported(&reply, listing, case);
    let token = next_token(&reply, case).unwrap_or_else(|| {
        panic!("case {case:?}: expected a NextToken on a truncated page, got {:?}", reply.body)
    });
    (
        token,
        reported
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected one element, got {reported:?}"))
            .clone(),
    )
}

/// `token`'s bytes with the byte at `index` changed by one bit, spelled back the way this
/// service spells a cursor.
fn with_byte_flipped(token: &str, index: usize) -> String {
    let mut bytes = ::hex::decode(token).expect("a minted cursor is this service's hex spelling");
    bytes[index] ^= 0x01;
    ::hex::encode(bytes)
}

/// A value with exactly the SHAPE of a minted cursor of this service: an authenticator of
/// the modelled width ahead of the payload a cursor carries (a list identity and the key of
/// the element a page reported last), spelled in hex.
///
/// This is what a caller can build for itself — the shape is not a secret — and what only
/// the authenticator can refuse.
fn shaped_like_a_cursor(authenticator: u8, identity: &str, element: &str) -> String {
    let payload = ::serde_json::to_string(&(identity, [element]))
        .expect("the payload of a cursor serializes");
    let mut bytes = vec![authenticator; AUTHENTICATOR_BYTES];
    bytes.extend_from_slice(payload.as_bytes());
    ::hex::encode(bytes)
}

/// The same payload, behind the authenticator of a key that is NOT this store's: the cursor
/// another emulator process — or a restarted one — would have minted.
fn minted_under_another_key(identity: &str, element: &str) -> String {
    let payload = ::serde_json::to_string(&(identity, [element]))
        .expect("the payload of a cursor serializes");
    ::hex::encode(PageTokenKey::new().mint(&payload))
}

/// The cursor a page handed out, with the position it names replaced by another of the same
/// length.
///
/// This is the forgery that needs nothing but the value the caller was GIVEN: a cursor is
/// opaque, not confidential, so the position inside it is readable and a caller can put
/// another one there. What has to refuse it is therefore the authenticator over that
/// position rather than the framing around it.
fn with_the_position_changed(token: &str, element: &str) -> String {
    let mut bytes = ::hex::decode(token).expect("a minted cursor is this service's hex spelling");
    let needle = element.as_bytes();
    let at = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap_or_else(|| panic!("a cursor carries the position it names, {element:?}"));
    // Another position of the same length, so everything around it is untouched: only the
    // element the cursor names differs.
    let mut replacement = needle.to_vec();
    let last = replacement.len() - 1;
    replacement[last] = if replacement[last] == b'z' { b'y' } else { b'z' };
    bytes.splice(at..at + needle.len(), replacement);
    ::hex::encode(bytes)
}

#[tokio::test]
async fn a_cursor_a_page_minted_resumes_the_walk_of_every_paged_list() {
    let h = seeded().await;

    for listing in LISTINGS {
        let case = &format!("{}: a walk at a page size of one", listing.label);
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let reply = page(&h, listing, 1, token.as_deref()).await;
            walked.extend(reported(&reply, listing, case));
            token = next_token(&reply, case);
            assert!(
                walked.len() <= DISCOVERED.len() + 1,
                "case {case:?}: the walk did not terminate, reported {walked:?}"
            );
            if token.is_none() {
                break;
            }
        }

        let mut sorted = walked.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            walked, sorted,
            "case {case:?}: expected the walk to report every element once, in the list's \
             order, got {walked:?}"
        );
        assert_eq!(
            walked.len(),
            DISCOVERED.len(),
            "case {case:?}: expected {} elements, got {walked:?}",
            DISCOVERED.len()
        );
    }
}

#[tokio::test]
async fn a_cursor_the_listing_did_not_mint_is_refused_by_every_paged_list() {
    let h = seeded().await;

    // Each list's own cursor, so a row can forge a value of that list's shape and so the
    // cross-list row has a sibling's cursor to send.
    let mut minted: Vec<(String, String)> = Vec::new();
    for listing in LISTINGS {
        minted.push(first_cursor(&h, listing).await);
    }

    for (index, listing) in LISTINGS.iter().enumerate() {
        let (cursor, element) = &minted[index];
        let bytes = ::hex::decode(cursor).expect("a minted cursor is hex");
        // A list identity of the size this service binds into a cursor, which a caller can
        // render for itself but cannot authenticate.
        let identity = "0".repeat(AUTHENTICATOR_BYTES * 2);
        // The sibling list's cursor: a value THIS store minted, for another list.
        let (sibling_cursor, _) = &minted[(index + 1) % LISTINGS.len()];

        // (what the row varies, the value sent as NextToken)
        let forged: Vec<(String, String)> = vec![
            (
                "not this service's spelling of a cursor".to_string(),
                "not-a-cursor".to_string(),
            ),
            (
                "hex too short to carry an authenticator".to_string(),
                ::hex::encode(vec![0_u8; AUTHENTICATOR_BYTES - 1]),
            ),
            (
                "a cursor's shape behind an all-zero authenticator".to_string(),
                shaped_like_a_cursor(0x00, &identity, element),
            ),
            (
                "a cursor's shape behind an all-ones authenticator".to_string(),
                shaped_like_a_cursor(0xff, &identity, element),
            ),
            (
                "minted under another store's secret".to_string(),
                minted_under_another_key(&identity, element),
            ),
            (
                "a minted cursor with its first authenticator byte changed".to_string(),
                with_byte_flipped(cursor, 0),
            ),
            (
                "a minted cursor with its last authenticator byte changed".to_string(),
                with_byte_flipped(cursor, AUTHENTICATOR_BYTES - 1),
            ),
            (
                "a minted cursor with its first payload byte changed".to_string(),
                with_byte_flipped(cursor, AUTHENTICATOR_BYTES),
            ),
            (
                "a minted cursor with its last payload byte changed".to_string(),
                with_byte_flipped(cursor, bytes.len() - 1),
            ),
            (
                "a minted cursor with the position inside it replaced".to_string(),
                with_the_position_changed(cursor, element),
            ),
            (
                "a cursor another list of this store minted".to_string(),
                sibling_cursor.clone(),
            ),
        ];

        for (variation, token) in forged {
            let case = &format!("{} sent {variation}", listing.label);
            page(&h, listing, 1, Some(&token))
                .await
                .error(case, 400, "ValidationException");
        }

        // The genuine cursor is still honoured, after every refusal above: a refused value
        // changes nothing about the walk the listing offered.
        let case = &format!("{}: the cursor its own page minted", listing.label);
        let reply = page(&h, listing, 1, Some(cursor)).await;
        let reported = reported(&reply, listing, case);
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected the next page to report one element, got {reported:?}"
        );
        assert!(
            reported[0] > *element,
            "case {case:?}: expected the page to resume after {element:?}, got {:?}",
            reported[0]
        );
    }
}

/// The paged list labelled `label`, from [`LISTINGS`].
fn listing(label: &str) -> &'static Listing {
    LISTINGS
        .iter()
        .find(|listing| listing.label == label)
        .unwrap_or_else(|| panic!("{label} is one of this service's paged lists"))
}

/// One page of `listing` under `selection` — the selection members (`Filters`, or
/// `SubscriptionProviderSources`) exactly as a request spells them — with `max_results` as
/// the page size and `token` as the continuation.
async fn select(
    h: &Harness,
    listing: &Listing,
    selection: &Value,
    max_results: Option<i32>,
    token: Option<&str>,
) -> Reply {
    let mut body = selection.clone();
    if let Some(max_results) = max_results {
        body["MaxResults"] = json!(max_results);
    }
    if let Some(token) = token {
        body["NextToken"] = json!(token);
    }
    call(h, listing.path, &body).await
}

/// Pairs of requests spelling ONE selection two ways: what the row varies, the list it
/// drives, and the two spellings' selection members.
///
/// The rows span what the equivalence generalises over: the default comparison spelled out
/// or left absent, the order of a set of values, the order of a set of filters, a repeat of
/// a component of a set, and an absent selection against an empty one — each on a list that
/// selects on `Filters` and, where it applies, on the one that selects on
/// `SubscriptionProviderSources`.
fn one_selection_two_spellings() -> Vec<(&'static str, &'static Listing, Value, Value)> {
    let instances = listing("ListLinuxSubscriptionInstances");
    let subscriptions = listing("ListLinuxSubscriptions");
    let providers = listing("ListRegisteredSubscriptionProviders");
    vec![
        (
            "an absent Operator and the Equal it means",
            instances,
            json!({ "Filters": [{ "Name": "Status", "Values": ["running"] }] }),
            json!({ "Filters": [{ "Name": "Status", "Values": ["running"], "Operator": "Equal" }] }),
        ),
        (
            "the Values of an Equal filter in another order",
            instances,
            json!({ "Filters": [{ "Name": "InstanceID", "Values": ["i-0a1", "i-0b2", "i-0c3"], "Operator": "Equal" }] }),
            json!({ "Filters": [{ "Name": "InstanceID", "Values": ["i-0c3", "i-0a1", "i-0b2"], "Operator": "Equal" }] }),
        ),
        (
            "the Values of a Contains filter in another order",
            instances,
            json!({ "Filters": [{ "Name": "Region", "Values": ["us-west", "eu-"], "Operator": "Contains" }] }),
            json!({ "Filters": [{ "Name": "Region", "Values": ["eu-", "us-west"], "Operator": "Contains" }] }),
        ),
        (
            "two filters in another order",
            instances,
            json!({ "Filters": [
                { "Name": "Status", "Values": ["running"] },
                { "Name": "AccountID", "Values": ["000000000000"] },
            ] }),
            json!({ "Filters": [
                { "Name": "AccountID", "Values": ["000000000000"] },
                { "Name": "Status", "Values": ["running"] },
            ] }),
        ),
        (
            "one filter named once and twice",
            instances,
            json!({ "Filters": [{ "Name": "Status", "Values": ["running"] }] }),
            json!({ "Filters": [
                { "Name": "Status", "Values": ["running"] },
                { "Name": "Status", "Values": ["running"] },
            ] }),
        ),
        (
            "an absent Filters and an empty one",
            instances,
            json!({}),
            json!({ "Filters": [] }),
        ),
        (
            "an absent Operator and the Equal it means",
            subscriptions,
            json!({ "Filters": [{ "Name": "Subscription", "Values": ["RHEL", "Ubuntu Pro"] }] }),
            json!({ "Filters": [{ "Name": "Subscription", "Values": ["RHEL", "Ubuntu Pro"], "Operator": "Equal" }] }),
        ),
        (
            "the Values of an Equal filter in another order",
            subscriptions,
            json!({ "Filters": [{ "Name": "Subscription", "Values": ["RHEL", "Ubuntu Pro"], "Operator": "Equal" }] }),
            json!({ "Filters": [{ "Name": "Subscription", "Values": ["Ubuntu Pro", "RHEL"], "Operator": "Equal" }] }),
        ),
        (
            "one source named once and twice",
            providers,
            json!({ "SubscriptionProviderSources": ["RedHat"] }),
            json!({ "SubscriptionProviderSources": ["RedHat", "RedHat"] }),
        ),
        (
            "an absent source list and an empty one",
            providers,
            json!({}),
            json!({ "SubscriptionProviderSources": [] }),
        ),
    ]
}

#[tokio::test]
async fn one_walk_continues_whichever_way_its_selection_is_spelled() {
    let h = seeded().await;

    for (variation, listing, one, other) in one_selection_two_spellings() {
        let case = &format!("{}: {variation}", listing.label);

        // What makes the two spellings one list: each selects the same elements, in the
        // same order, so a cursor naming a position in one names a position in the other.
        let whole = reported(&select(&h, listing, &one, None, None).await, listing, case);
        let whole_other = reported(
            &select(&h, listing, &other, None, None).await,
            listing,
            case,
        );
        assert_eq!(
            whole, whole_other,
            "case {case:?}: the two spellings must select the same list, got {whole:?} and \
             {whole_other:?}"
        );
        assert!(
            whole.len() >= 2,
            "case {case:?}: a row has to select at least two elements for a page size of \
             one to truncate, got {whole:?}"
        );

        // The walk, with the spelling ALTERNATING from page to page: every cursor is
        // minted under one spelling and sent back under the other, in both directions, and
        // the walk still reports the selected list exactly once, in order.
        let mut spellings = [&one, &other].into_iter().cycle();
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let selection = spellings.next().expect("the spellings cycle is endless");
            let reply = select(&h, listing, selection, Some(1), token.as_deref()).await;
            walked.extend(reported(&reply, listing, case));
            token = next_token(&reply, case);
            assert!(
                walked.len() <= whole.len(),
                "case {case:?}: the walk did not terminate, reported {walked:?} of {whole:?}"
            );
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, whole,
            "case {case:?}: the alternating walk must report the selected list exactly \
             once, in order, expected {whole:?}, got {walked:?}"
        );
    }
}

/// Pairs of requests meaning two DIFFERENT selections: what the row varies, the list it
/// drives, and the two selections — the second selecting other elements than the first, so
/// a cursor naming a position in one names no position in the other.
fn two_selections() -> Vec<(&'static str, &'static Listing, Value, Value)> {
    let instances = listing("ListLinuxSubscriptionInstances");
    let subscriptions = listing("ListLinuxSubscriptions");
    vec![
        (
            "the same values compared with another Operator",
            instances,
            json!({ "Filters": [{ "Name": "Status", "Values": ["running"] }] }),
            json!({ "Filters": [{ "Name": "Status", "Values": ["running"], "Operator": "NotEqual" }] }),
        ),
        (
            "Contains against the Equal of the same value",
            instances,
            json!({ "Filters": [{ "Name": "Region", "Values": ["us-west"], "Operator": "Contains" }] }),
            json!({ "Filters": [{ "Name": "Region", "Values": ["us-west"], "Operator": "Equal" }] }),
        ),
        (
            "another member addressed",
            instances,
            json!({ "Filters": [{ "Name": "Status", "Values": ["running"] }] }),
            json!({ "Filters": [{ "Name": "InstanceID", "Values": ["i-0a1"] }] }),
        ),
        (
            "another set of values",
            subscriptions,
            json!({ "Filters": [{ "Name": "Subscription", "Values": ["RHEL", "Ubuntu Pro"] }] }),
            json!({ "Filters": [{ "Name": "Subscription", "Values": ["SUSE Linux Enterprise Server"] }] }),
        ),
    ]
}

#[tokio::test]
async fn a_cursor_is_refused_by_a_request_meaning_another_selection() {
    let h = seeded().await;

    for (variation, listing, one, other) in two_selections() {
        let case = &format!("{}: {variation}", listing.label);

        // What makes them two lists: they report different elements.
        let whole = reported(&select(&h, listing, &one, None, None).await, listing, case);
        let whole_other = reported(
            &select(&h, listing, &other, None, None).await,
            listing,
            case,
        );
        assert_ne!(
            whole, whole_other,
            "case {case:?}: the two selections must report different lists, both reported \
             {whole:?}"
        );

        let first = select(&h, listing, &one, Some(1), None).await;
        let token = next_token(&first, case).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a NextToken on a truncated page, got {:?}",
                first.body
            )
        });
        select(&h, listing, &other, Some(1), Some(&token))
            .await
            .error(case, 400, "ValidationException");
    }
}

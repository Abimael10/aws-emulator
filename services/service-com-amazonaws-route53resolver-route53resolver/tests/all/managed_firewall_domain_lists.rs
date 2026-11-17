// SPDX-License-Identifier: CC0-1.0

//! A DNS Firewall domain list the service publishes itself is not the caller's
//! to write, and its domains are not the caller's to read.
//!
//! The rule, stated once and then exercised over a set of inputs: a domain list
//! whose `ManagedOwnerName` is set belongs to Route 53 Resolver DNS Firewall and
//! not to the account that can see it, so
//!
//! * every operation that would WRITE it — `UpdateFirewallDomains`,
//!   `DeleteFirewallDomainList` — answers exactly as it answers for a list that
//!   does not exist: HTTP 400 `ResourceNotFoundException` carrying the service's
//!   "can't find the resource with ID" body for the id the request named, so a
//!   list the caller may not write is indistinguishable from one it does not
//!   have;
//! * `ListFirewallDomains` answers HTTP 400 `AccessDeniedException` — the
//!   domains inside a published list are the service's own threat intelligence,
//!   so the list is visible while its contents are not;
//! * the readers of the list itself keep serving it: `GetFirewallDomainList`
//!   reports the same list, with the same `Status` and `DomainCount`, after
//!   every one of those refusals;
//! * and the caller's OWN list takes the very same write, so the refusals are
//!   about who owns the list and not about rejecting the request.
//!
//! The same ownership decides what a published list is IDENTIFIED by, which is
//! the second rule pinned here: a resource a service publishes itself carries
//! the service's OWN published identifiers — one `Id` per published resource,
//! the same for every caller and every process, and an `Arn` whose account field
//! is the publishing service's account and never the account this emulator
//! serves — so the only part of that identity a request decides is the Region
//! the ARN is rooted in. A list the CALLER creates is the other side of it: its
//! identifiers are minted in the caller's own account.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! shared [`super::harness`].

use serde_json::{json, Value};
use service_com_amazonaws_route53resolver_route53resolver::state::MANAGED_FIREWALL_DOMAIN_LIST_PUBLISHER_ACCOUNT;
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    call_in_region, call_tracing, emulator, error_type, ok, string_member, tracing_chain, ACCOUNT,
    REGION,
};

/// The tracing chain every case of this suite sends, so a refusal that closes
/// with the trace of the request it refused closes with a known `Root` and the
/// whole message can be compared byte for byte.
fn tracing() -> (String, String) {
    tracing_chain(37)
}

/// The message Route 53 Resolver answers for a resource the caller named but
/// cannot address: the bracketed service error code, the fixed sentence (with
/// the typographic apostrophe AWS writes it with), the id from the request, and
/// the trace clause of the request it closes with.
fn cannot_find(resource_id: &str) -> String {
    let (_, root) = tracing();
    format!(r#"[RSLVR-02025] Can’t find the resource with ID "{resource_id}". Trace Id: "{root}""#)
}

/// The fixed message `ListFirewallDomains` refuses a published list with.
const MANAGED_LIST_DENIED: &str = "You don't have a permission to list the domains inside a \
                                   Route 53 Resolver DNS Firewall managed list.";

/// Every domain list the service publishes, as `(name, id)` — the lists
/// `ListFirewallDomainLists` reports a `ManagedOwnerName` for, which is the
/// member used "only for lists that are not managed by you"
/// (`smithy_query(query="doc",shape="FirewallDomainList",member="ManagedOwnerName")`).
async fn published_lists(router: &MultiServiceRouter) -> Vec<(String, String)> {
    let listing = ok(router, "ListFirewallDomainLists", json!({ "MaxResults": 100 })).await;
    let published: Vec<(String, String)> = listing
        .get("FirewallDomainLists")
        .and_then(Value::as_array)
        .expect("the listing reports the domain lists")
        .iter()
        .filter(|item| item.get("ManagedOwnerName").is_some())
        .map(|item| (string_member(item, "Name"), string_member(item, "Id")))
        .collect();
    assert!(
        !published.is_empty(),
        "the listing must report the lists DNS Firewall publishes, got {listing}"
    );
    published
}

/// The `FirewallDomainList` `GetFirewallDomainList` reports for `id`.
async fn read_list(router: &MultiServiceRouter, id: &str) -> Value {
    let answered = ok(
        router,
        "GetFirewallDomainList",
        json!({ "FirewallDomainListId": id }),
    )
    .await;
    answered
        .get("FirewallDomainList")
        .unwrap_or_else(|| panic!("the read must answer a FirewallDomainList, got {answered}"))
        .clone()
}

/// Assert `operation` refuses `request` with HTTP 400, the error shape
/// `expected_error` and exactly `expected_message`.
async fn expect_refusal(
    router: &MultiServiceRouter,
    operation: &str,
    request: Value,
    case: &str,
    expected_error: &str,
    expected_message: &str,
) {
    let (chain, _) = tracing();
    let (status, answered) = call_tracing(router, operation, request, Some(&chain)).await;
    assert_eq!(
        status, 400,
        "case {case:?}: {operation} must answer HTTP 400, got {status}: {answered}"
    );
    assert!(
        error_type(&answered).contains(expected_error),
        "case {case:?}: {operation} must answer {expected_error}, got {answered}"
    );
    assert_eq!(
        string_member(&answered, "Message"),
        expected_message,
        "case {case:?}: {operation} must state the fault as AWS words it, got {answered}"
    );
}

/// The write path hides every published list, the domain listing refuses it,
/// and the list itself is unchanged and still readable afterwards.
///
/// The table spans what the rule generalises over: every published list rather
/// than one, and each `FirewallDomainUpdateOperation` the model declares, so the
/// answer is about the list and not about the kind of write.
#[tokio::test]
async fn a_published_domain_list_is_hidden_from_the_write_path() {
    let router = emulator();

    for (name, id) in published_lists(&router).await {
        let before = read_list(&router, &id).await;

        for operation in ["ADD", "REMOVE", "REPLACE"] {
            let case = format!("{name} / {operation}");
            expect_refusal(
                &router,
                "UpdateFirewallDomains",
                json!({
                    "FirewallDomainListId": id.clone(),
                    "Operation": operation,
                    "Domains": ["sim2real-should-not-land.example."],
                }),
                &case,
                "ResourceNotFoundException",
                &cannot_find(&id),
            )
            .await;
        }

        expect_refusal(
            &router,
            "DeleteFirewallDomainList",
            json!({ "FirewallDomainListId": id.clone() }),
            &name,
            "ResourceNotFoundException",
            &cannot_find(&id),
        )
        .await;

        expect_refusal(
            &router,
            "ListFirewallDomains",
            json!({ "FirewallDomainListId": id.clone() }),
            &name,
            "AccessDeniedException",
            MANAGED_LIST_DENIED,
        )
        .await;

        // The list itself is still served, and nothing about it moved.
        let after = read_list(&router, &id).await;
        assert_eq!(
            after, before,
            "case {name:?}: the published list must be reported exactly as before the refusals"
        );
        assert_eq!(
            string_member(&after, "Name"),
            name,
            "case {name:?}: the published list must still be readable under its own id, got {after}"
        );
        assert!(
            after.get("ManagedOwnerName").is_some(),
            "case {name:?}: the list the refusals are about is the service's own, got {after}"
        );
        assert_eq!(
            after.get("DomainCount").and_then(Value::as_i64),
            Some(0),
            "case {name:?}: the read must report the DomainCount this store holds for a published \
             list, whose domains are the service's own and are held nowhere here, got {after}"
        );
        assert_eq!(
            string_member(&after, "Status"),
            string_member(&before, "Status"),
            "case {name:?}: the refusals must not move the list's Status, got {after}"
        );
    }
}

/// The same write the published lists hide is served against the caller's own
/// list, and an id nothing minted is refused with the same body a published list
/// is — so the refusal is about ownership, not about the request, and a list the
/// caller may not write cannot be told apart from one that is not there.
#[tokio::test]
async fn the_callers_own_list_takes_the_write_a_published_list_hides() {
    let router = emulator();
    let published = published_lists(&router).await;
    let (_, published_id) = published
        .first()
        .cloned()
        .expect("at least one published list");

    let own = ok(
        &router,
        "CreateFirewallDomainList",
        json!({ "CreatorRequestId": "req-own", "Name": "my-own-list" }),
    )
    .await;
    let own_id = string_member(
        own.get("FirewallDomainList")
            .unwrap_or_else(|| panic!("the create must answer a FirewallDomainList, got {own}")),
        "Id",
    );

    let updated = ok(
        &router,
        "UpdateFirewallDomains",
        json!({
            "FirewallDomainListId": own_id.clone(),
            "Operation": "ADD",
            "Domains": ["sim2real-should-not-land.example."],
        }),
    )
    .await;
    assert_eq!(
        string_member(&updated, "Id"),
        own_id,
        "the caller's own list must take the write, got {updated}"
    );
    let listed = ok(
        &router,
        "ListFirewallDomains",
        json!({ "FirewallDomainListId": own_id.clone() }),
    )
    .await;
    assert_eq!(
        listed.get("Domains"),
        Some(&json!(["sim2real-should-not-land.example."])),
        "the caller's own list must report the domain the write added, got {listed}"
    );

    // An id nothing minted, and the published list, answer the same body.
    let absent = format!("{own_id}x");
    for (case, id) in [
        ("an id nothing minted", absent),
        ("a published list", published_id),
    ] {
        expect_refusal(
            &router,
            "UpdateFirewallDomains",
            json!({
                "FirewallDomainListId": id.clone(),
                "Operation": "ADD",
                "Domains": ["sim2real-should-not-land.example."],
            }),
            case,
            "ResourceNotFoundException",
            &cannot_find(&id),
        )
        .await;
    }
}

/// The Regions the identity cases sign for. The rule generalises over the
/// Region: the signed Region is the ARN's `region` field and decides nothing
/// else about a published list's identity, so the table spans three Regions in
/// three geographies rather than the one a single call was made in.
const REGIONS: &[&str] = &[REGION, "eu-west-1", "ap-southeast-2"];

/// The identity DNS Firewall publishes each of its own domain lists under, as
/// `(Name, Id)`, in [`REGION`].
///
/// Source: the catalog AWS publishes for DNS Firewall's managed domain lists —
/// the same published data the names and categories asserted above come from,
/// whose ids are per-Region service constants a client may pin. They are listed
/// here so a published id that went back to being derived from something of the
/// caller's (the list's name, the caller's account) fails this suite.
const PUBLISHED_IDENTITIES: &[(&str, &str)] = &[
    (
        "AWSManagedDomainsMalwareDomainList",
        "rslvr-fdl-fe17696f22a445f6",
    ),
    (
        "AWSManagedDomainsBotnetCommandandControl",
        "rslvr-fdl-c80983a1f0284c99",
    ),
    (
        "AWSManagedDomainsAggregateThreatList",
        "rslvr-fdl-d252ee1944404e15",
    ),
    (
        "AWSManagedDomainsAmazonGuardDutyThreatList",
        "rslvr-fdl-23584f0fb4c24ebb",
    ),
];

/// Call one operation signed for `region` and require HTTP 200.
async fn ok_in_region(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, parsed) = call_in_region(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {region:?}: {operation} must succeed, got {status}: {parsed}"
    );
    parsed
}

/// Every entry `ListFirewallDomainLists` reports to a caller signing for
/// `region`, walked to the end of the pages.
async fn listed_in_region(router: &MultiServiceRouter, region: &str) -> Vec<Value> {
    let mut walked: Vec<Value> = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut body = json!({});
        if let Some(token) = &next_token {
            body["NextToken"] = json!(token);
        }
        let page = ok_in_region(router, region, "ListFirewallDomainLists", body).await;
        let items = page
            .get("FirewallDomainLists")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {region:?}: the response carries a FirewallDomainLists array, got {page}")
            });
        walked.extend(items);
        next_token = page
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if next_token.is_none() {
            return walked;
        }
    }
}

/// The published entries of a listing: the ones that report a `ManagedOwnerName`,
/// the member used "only for lists that are not managed by you".
fn published_entries(items: &[Value]) -> Vec<Value> {
    items
        .iter()
        .filter(|item| item.get("ManagedOwnerName").is_some())
        .cloned()
        .collect()
}

/// The field at `index` of an ARN (`arn:aws:<service>:<region>:<account>:<rest>`).
fn arn_field(arn: &str, index: usize) -> String {
    arn.split(':').nth(index).unwrap_or_default().to_string()
}

/// Every published list carries the publishing service's identity: the id DNS
/// Firewall publishes it under, which no store and no caller moves, under an ARN
/// rooted in the signed Region and in the publisher's account.
///
/// The rows span what the rule generalises over: every entry of the published
/// catalog rather than one, each of [`REGIONS`], a second store, and — as the
/// other side of the rule — a list this account created, whose identifiers ARE
/// its own. The published identities themselves ([`PUBLISHED_IDENTITIES`]) are
/// pinned in the Region they are published for.
#[tokio::test]
async fn a_published_list_is_identified_by_the_publishing_service_not_the_caller() {
    let router = emulator();

    // The other side of the rule: a list the caller creates is a resource of
    // the caller's account, so its ARN carries the account this emulator
    // serves.
    let created = ok(
        &router,
        "CreateFirewallDomainList",
        json!({ "CreatorRequestId": "req-own-identity", "Name": "own-identity" }),
    )
    .await;
    let own = created
        .get("FirewallDomainList")
        .unwrap_or_else(|| panic!("the create must answer a FirewallDomainList, got {created}"));
    let own_id = string_member(own, "Id");
    let own_arn = string_member(own, "Arn");
    assert_eq!(
        arn_field(&own_arn, 4),
        ACCOUNT,
        "case \"a list this account created\": its ARN is rooted in this account, got {own_arn:?}"
    );

    let mut per_region: Vec<(&str, Vec<(String, String)>)> = Vec::new();

    for region in REGIONS {
        let items = listed_in_region(&router, region).await;
        let published = published_entries(&items);
        assert!(
            !published.is_empty(),
            "case {region:?}: the catalog the service publishes must be reported, got {items:?}"
        );

        let mut identities: Vec<(String, String)> = Vec::new();
        for item in &published {
            let name = string_member(item, "Name");
            let case = format!("{name} in {region}");
            let id = string_member(item, "Id");
            let arn = string_member(item, "Arn");

            assert!(
                id.starts_with("rslvr-fdl-") && !id.is_empty() && id.len() <= 64,
                "case {case:?}: a domain list Id is rslvr-fdl-<hex> inside ResourceId's length \
                 1..64, got {id:?}"
            );
            assert_eq!(
                arn,
                format!(
                    "arn:aws:route53resolver:{region}:\
                     {MANAGED_FIREWALL_DOMAIN_LIST_PUBLISHER_ACCOUNT}:firewall-domain-list/{id}"
                ),
                "case {case:?}: the Arn must carry the publishing service's account and this id, \
                 got {item}"
            );
            assert_ne!(
                arn_field(&arn, 4),
                ACCOUNT,
                "case {case:?}: a list this account does not manage must not be published inside \
                 this account's ARN space, got {arn:?}"
            );
            assert_eq!(
                arn_field(&arn, 3),
                *region,
                "case {case:?}: the Arn must be rooted in the signed Region, got {arn:?}"
            );
            assert_ne!(
                id, own_id,
                "case {case:?}: a published id must not be an id minted for this account, got \
                 {id:?}"
            );

            // The id the catalog advertised is the id the rest of the service
            // resolves, in the very same Region.
            let fetched = ok_in_region(
                &router,
                region,
                "GetFirewallDomainList",
                json!({ "FirewallDomainListId": id.clone() }),
            )
            .await;
            let read = fetched.get("FirewallDomainList").unwrap_or_else(|| {
                panic!("case {case:?}: the read must answer a FirewallDomainList, got {fetched}")
            });
            for member in ["Id", "Arn", "Name", "ManagedOwnerName"] {
                assert_eq!(
                    read.get(member),
                    item.get(member),
                    "case {case:?}: the read must agree with the listing on {member}, got {fetched}"
                );
            }

            identities.push((name, id));
        }

        let mut ids: Vec<&String> = identities.iter().map(|(_, id)| id).collect();
        ids.sort();
        let count = ids.len();
        ids.dedup();
        assert_eq!(
            ids.len(),
            count,
            "case {region:?}: each published list must carry its own id, got {identities:?}"
        );

        if *region == REGION {
            // In the Region the catalog above is published for, the identities
            // are those published values and not ones this emulator derived.
            let published_identities: Vec<(String, String)> = PUBLISHED_IDENTITIES
                .iter()
                .map(|(name, id)| ((*name).to_string(), (*id).to_string()))
                .collect();
            assert_eq!(
                identities, published_identities,
                "case {region:?}: each list must be reported under the id DNS Firewall publishes \
                 it under, not one derived from its name or from this account"
            );
        }

        per_region.push((region, identities));
    }

    // The identity does not move with the store either: a second emulator,
    // whose store nothing ever wrote, publishes each Region's catalog under the
    // same identifiers — what a client that pinned a published id relies on.
    let other = emulator();
    for (region, identities) in &per_region {
        let elsewhere: Vec<(String, String)> =
            published_entries(&listed_in_region(&other, region).await)
                .iter()
                .map(|item| (string_member(item, "Name"), string_member(item, "Id")))
                .collect();
        assert_eq!(
            &elsewhere, identities,
            "case {region:?}: a process holding another store must publish the same identities"
        );
    }
}

/// The account the catalog is published under is a well-formed AWS account id,
/// and is not the account this emulator serves — the two cannot be the same
/// without every published entry contradicting its own `ManagedOwnerName`.
#[tokio::test]
async fn the_publishing_account_is_not_the_account_this_emulator_serves() {
    let publisher = MANAGED_FIREWALL_DOMAIN_LIST_PUBLISHER_ACCOUNT;
    assert!(
        publisher.len() == 12 && publisher.chars().all(|c| c.is_ascii_digit()),
        "the publishing account must be a 12-digit AWS account id, got {publisher:?}"
    );
    assert_ne!(
        publisher, ACCOUNT,
        "the service's publishing account must not be the account this emulator serves"
    );
}

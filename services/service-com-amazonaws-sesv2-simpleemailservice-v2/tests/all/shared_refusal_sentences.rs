// SPDX-License-Identifier: CC0-1.0

//! The three refusal SENTENCES this service shares across the paths that reach them, end
//! to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered, through the crate's shared JSON wire harness
//! [`crate::json_wire`] — so routing, deserialization, the handler and serialization are
//! all exercised.
//!
//! The rules these cases pin, one per suite below:
//!
//! * a `NextToken` that is not a position in the listing being read is refused with the
//!   listing's modelled input error and ONE sentence, `"Invalid next token."` — wholly
//!   fixed text that never echoes the rejected cursor — whichever listing refuses it and
//!   whatever form the cursor takes, because it is one condition rather than one
//!   operation's own complaint. (`ListEmailIdentities` is the one listing AWS words
//!   differently, echoing the cursor it rejected; its sentence is pinned beside its own
//!   operation in `tests/all/email_identity_cursor_refusal_wording.rs` and nothing here
//!   drives it.)
//! * every TENANT-keyed view of a name the account does not hold reports it with one
//!   sentence naming the requested tenant in angle brackets, whether the name was never
//!   created or was deleted while it still held resource associations — the lookup, not
//!   the operation, is what words it;
//! * a contact create refused because the list already holds the address states the
//!   address and the KIND of resource it is held in, never the list's own name — the same
//!   sentence for the losers of a race as for a duplicate sent on its own.
//!
//! The tables span what the rules generalise over: the LISTING and the FORM of a bad
//! cursor; how a tenant name came to be unknown and the form of that name; and the width
//! of a concurrent contact race together with the form of the address raced for.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::json_wire::{query_component, Reply, Ses};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state;

/// AWS's fixed sentence for a cursor the listings below never issued.
const INVALID_NEXT_TOKEN: &str = "Invalid next token.";

/// The contact list the listing table reads, and the identity whose certificates it
/// reads: values the rules do not generalise over, named once.
const SEEDED_LIST: &str = "announcements";
const SEEDED_IDENTITY: &str = "example.com";
const SEEDED_TENANT: &str = "tenant-a";
const SEEDED_CONFIGURATION_SET: &str = "cs-a";

/// Where a listing carries its `NextToken`: the two wire forms this service's listings
/// take.
#[derive(Debug, Clone, Copy)]
enum Cursor {
    /// `NextToken` is bound to `@httpQuery` on a `GET`.
    Query,
    /// `NextToken` is a member of the `POST` body.
    Body,
}

/// One listing under test: how it is addressed, where its cursor goes, the members it
/// must carry beside the cursor, and whether its model constrains the cursor's own form.
struct Listing {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    cursor: Cursor,
    /// The required members of a `POST` body listing; `Value::Null` for a `GET`.
    body: fn() -> Value,
    /// Whether the model constrains `NextToken` to base64 text, in which case a cursor
    /// outside that form is refused by the constraint walk before any handler sees it
    /// (`NextTokenV2`'s `@pattern`, from
    /// `smithy_query(query="operation",operation="ListMultiRegionEndpoints")`).
    modelled_base64: bool,
}

/// The listings the table spans: the account-wide `GET`s, the ones addressed through a
/// `POST` body, and the ones whose cursor sits beside a required parent member.
fn listings() -> Vec<Listing> {
    vec![
        Listing {
            case: "ListDedicatedIpPools",
            method: "GET",
            path: "/v2/email/dedicated-ip-pools",
            cursor: Cursor::Query,
            body: || Value::Null,
            modelled_base64: false,
        },
        Listing {
            case: "ListContactLists",
            method: "GET",
            path: "/v2/email/contact-lists",
            cursor: Cursor::Query,
            body: || Value::Null,
            modelled_base64: false,
        },
        Listing {
            case: "ListMultiRegionEndpoints",
            method: "GET",
            path: "/v2/email/multi-region-endpoints",
            cursor: Cursor::Query,
            body: || Value::Null,
            modelled_base64: true,
        },
        Listing {
            case: "ListContacts",
            method: "POST",
            path: "/v2/email/contact-lists/announcements/contacts/list",
            cursor: Cursor::Body,
            body: || json!({}),
            modelled_base64: false,
        },
        Listing {
            case: "ListEmailIdentityCertificates",
            method: "POST",
            path: "/v2/email/identity/certificates/list",
            cursor: Cursor::Body,
            body: || json!({"EmailIdentity": SEEDED_IDENTITY}),
            modelled_base64: false,
        },
        Listing {
            case: "ListResourceTenants",
            method: "POST",
            path: "/v2/email/resources/tenants/list",
            cursor: Cursor::Body,
            body: || json!({"ResourceArn": seeded_resource_arn()}),
            modelled_base64: false,
        },
        Listing {
            case: "ListTenantResources",
            method: "POST",
            path: "/v2/email/tenants/resources/list",
            cursor: Cursor::Body,
            body: || json!({"TenantName": SEEDED_TENANT}),
            modelled_base64: false,
        },
    ]
}

/// The ARN of the configuration set the seeded tenant holds, built by the crate's own
/// ARN construction so no suite spells the form itself.
fn seeded_resource_arn() -> String {
    state::configuration_set_arn(crate::json_wire::HOME, SEEDED_CONFIGURATION_SET)
}

/// The forms a cursor this service never issued can arrive in: base64 of text no listing
/// mints, a cursor ANOTHER listing of this service did mint, text that is not base64 at
/// all, a bare resource name, and nothing at all. The flag says whether the form is
/// base64-shaped, so a listing whose model admits only base64 is driven with the forms
/// that reach its handler.
fn forged_cursors() -> Vec<(&'static str, String, bool)> {
    vec![
        (
            "base64 of text no listing ever minted",
            "c2ltMnJlYWwtZm9yZ2VkLW5leHQtdG9rZW4=".to_string(),
            true,
        ),
        (
            "a cursor another listing of this service minted",
            state::encode_tagged_page_token(
                state::SUPPRESSED_DESTINATIONS_PAGE_TOKEN_TAG,
                "someone@example.com",
            ),
            true,
        ),
        (
            "text that is not base64 at all",
            "!!not-a-token!!".to_string(),
            false,
        ),
        (
            "a bare resource name",
            ::aws_smithy_types::base64::encode(SEEDED_LIST),
            true,
        ),
        ("the empty string", String::new(), false),
    ]
}

/// Read `listing`, carrying `cursor` when there is one and `page_size` when the case
/// names one.
async fn read_page(
    ses: &Ses,
    listing: &Listing,
    cursor: Option<&str>,
    page_size: Option<i64>,
) -> Reply {
    match listing.cursor {
        Cursor::Query => {
            let mut query = Vec::new();
            if let Some(cursor) = cursor {
                query.push(format!("NextToken={}", query_component(cursor)));
            }
            if let Some(page_size) = page_size {
                query.push(format!("PageSize={page_size}"));
            }
            let path = if query.is_empty() {
                listing.path.to_string()
            } else {
                format!("{}?{}", listing.path, query.join("&"))
            };
            ses.call(listing.method, &path, None).await
        }
        Cursor::Body => {
            let mut body = (listing.body)();
            let members = body
                .as_object_mut()
                .expect("a POST body listing sends an object");
            if let Some(cursor) = cursor {
                members.insert("NextToken".to_string(), json!(cursor));
            }
            if let Some(page_size) = page_size {
                members.insert("PageSize".to_string(), json!(page_size));
            }
            ses.call(listing.method, listing.path, Some(body)).await
        }
    }
}

/// Everything the listing table reads: a pool, a contact list holding a contact, an
/// identity, a multi-region endpoint, and a tenant holding a configuration set.
async fn seed_listings(ses: &Ses) {
    for (case, method, path, body) in [
        (
            "the pool",
            "POST",
            "/v2/email/dedicated-ip-pools",
            json!({"PoolName": "pool-a", "ScalingMode": "STANDARD"}),
        ),
        (
            "the contact list",
            "POST",
            "/v2/email/contact-lists",
            json!({"ContactListName": SEEDED_LIST}),
        ),
        (
            "the contact",
            "POST",
            "/v2/email/contact-lists/announcements/contacts",
            json!({"EmailAddress": "reader@example.com"}),
        ),
        (
            "the identity",
            "POST",
            "/v2/email/identities",
            json!({"EmailIdentity": SEEDED_IDENTITY}),
        ),
        (
            "the multi-region endpoint",
            "POST",
            "/v2/email/multi-region-endpoints",
            json!({
                "EndpointName": "endpoint-a",
                "Details": {"RoutesDetails": [{"Region": "us-west-2"}]},
            }),
        ),
        (
            "the configuration set",
            "POST",
            "/v2/email/configuration-sets",
            json!({"ConfigurationSetName": SEEDED_CONFIGURATION_SET}),
        ),
        (
            "the tenant",
            "POST",
            "/v2/email/tenants",
            json!({"TenantName": SEEDED_TENANT}),
        ),
        (
            "the association",
            "POST",
            "/v2/email/tenants/resources",
            json!({"TenantName": SEEDED_TENANT, "ResourceArn": seeded_resource_arn()}),
        ),
    ] {
        let reply = ses.call(method, path, Some(body)).await;
        assert_eq!(
            reply.status, 200,
            "seeding {case}: the set-up call must succeed, got {} {:?}",
            reply.status, reply.body
        );
    }
}

#[tokio::test]
async fn every_listing_words_a_cursor_it_never_issued_the_same_way() {
    let ses = Ses::new();
    seed_listings(&ses).await;

    for listing in listings() {
        // The positive control: the same listing with no cursor at all is served, and
        // that answer is what every refusal below must leave untouched.
        let served = read_page(&ses, &listing, None, None).await;
        assert_eq!(
            served.status,
            200,
            "case {:?} with no cursor: expected the listing to be served, got {} {:?}",
            listing.case,
            served.status,
            served.body
        );
        let page = served.body.clone();

        for (form, token, base64_shaped) in forged_cursors() {
            if listing.modelled_base64 && !base64_shaped {
                continue;
            }
            // Both ways a caller pages: the cursor on its own, and the cursor beside the
            // page size the issue's caller sent.
            for page_size in [None, Some(1)] {
                let case = format!("{} with {form} and PageSize {page_size:?}", listing.case);
                let refused = read_page(&ses, &listing, Some(&token), page_size).await;
                assert_eq!(
                    refused.status,
                    400,
                    "case {case:?}: expected the forged cursor to be refused 400, got {} {:?}",
                    refused.status,
                    refused.body
                );
                assert_eq!(
                    refused.error_code(),
                    "BadRequestException",
                    "case {case:?}: expected BadRequestException, got {:?} {:?}",
                    refused.error_code(),
                    refused.message()
                );
                assert_eq!(
                    refused.message(),
                    INVALID_NEXT_TOKEN,
                    "case {case:?}: expected the refusal to read {INVALID_NEXT_TOKEN:?}, got {:?}",
                    refused.message()
                );
                assert!(
                    token.is_empty() || !refused.message().contains(&token),
                    "case {case:?}: the refusal must not echo the rejected cursor, got {:?}",
                    refused.message()
                );
                assert!(
                    !refused.message().contains(listing.case),
                    "case {case:?}: the refusal must not name the listing, got {:?}",
                    refused.message()
                );
            }
        }

        // The refusals read, so the listing still answers exactly what it answered
        // before them.
        let again = read_page(&ses, &listing, None, None).await;
        assert_eq!(
            again.body,
            page,
            "case {:?}: expected the refusals to leave the listing unchanged, got {:?}",
            listing.case,
            again.body
        );
    }
}

/// How a tenant name came to be one the account does not hold.
#[derive(Debug, Clone, Copy)]
enum Unknown {
    /// The name was never created.
    NeverCreated,
    /// The tenant was deleted while it held nothing.
    DeletedEmpty,
    /// The tenant was deleted while it still held a resource association — the path the
    /// tenant-keyed index is reached through.
    DeletedHoldingResources,
}

/// The `TenantName` forms the table varies over: the shortest the model admits, one
/// carrying both documented separators and mixed case, and the longest documented name
/// ("can contain up to 64 alphanumeric characters, including letters, numbers, hyphens (-)
/// and underscores (_) only", `smithy_query(query="shape",shape="TenantName")`), taken
/// from the crate's own bound so it is stated once.
fn tenant_names() -> Vec<String> {
    vec![
        "t".to_string(),
        "Tenant_2-b".to_string(),
        "t".repeat(state::MAX_TENANT_NAME_CHARS),
    ]
}

#[tokio::test]
async fn every_tenant_keyed_view_of_an_unknown_tenant_words_the_missing_tenant_the_same_way() {
    for how in [
        Unknown::NeverCreated,
        Unknown::DeletedEmpty,
        Unknown::DeletedHoldingResources,
    ] {
        for name in tenant_names() {
            let case = format!("{how:?} tenant of {} characters", name.chars().count());
            let ses = Ses::new();

            if !matches!(how, Unknown::NeverCreated) {
                let created = ses
                    .call("POST", "/v2/email/tenants", Some(json!({"TenantName": name})))
                    .await;
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: seeding the tenant must succeed, got {} {:?}",
                    created.status, created.body
                );
            }
            if matches!(how, Unknown::DeletedHoldingResources) {
                let set = ses
                    .call(
                        "POST",
                        "/v2/email/configuration-sets",
                        Some(json!({"ConfigurationSetName": SEEDED_CONFIGURATION_SET})),
                    )
                    .await;
                assert_eq!(
                    set.status, 200,
                    "case {case:?}: seeding the resource must succeed, got {} {:?}",
                    set.status, set.body
                );
                let associated = ses
                    .call(
                        "POST",
                        "/v2/email/tenants/resources",
                        Some(json!({
                            "TenantName": name,
                            "ResourceArn": seeded_resource_arn(),
                        })),
                    )
                    .await;
                assert_eq!(
                    associated.status, 200,
                    "case {case:?}: seeding the association must succeed, got {} {:?}",
                    associated.status, associated.body
                );
            }
            if !matches!(how, Unknown::NeverCreated) {
                let deleted = ses
                    .call(
                        "POST",
                        "/v2/email/tenants/delete",
                        Some(json!({"TenantName": name})),
                    )
                    .await;
                assert_eq!(
                    deleted.status, 200,
                    "case {case:?}: the delete must succeed whatever the tenant held, got {} {:?}",
                    deleted.status, deleted.body
                );
            }

            // Every tenant-keyed view of the name — the read of the tenant itself and the
            // read of what it holds — reports it in the one sentence.
            let expected = format!("The requested tenant <{name}> does not exist.");
            for (view, path) in [
                ("GetTenant", "/v2/email/tenants/get"),
                ("ListTenantResources", "/v2/email/tenants/resources/list"),
            ] {
                let refused = ses
                    .call("POST", path, Some(json!({"TenantName": name})))
                    .await;
                assert_eq!(
                    (refused.status, refused.error_code().as_str()),
                    (404, "NotFoundException"),
                    "case {case:?}: expected {view} to answer 404 NotFoundException, got {} {:?}",
                    refused.status,
                    refused.body
                );
                assert_eq!(
                    refused.message(),
                    expected,
                    "case {case:?}: expected {view} to read {expected:?}, got {:?}",
                    refused.message()
                );
            }
        }
    }
}

/// `CreateContact` for `address` on `list`, over the harness.
async fn create_contact(ses: &Ses, list: &str, address: &str) -> Reply {
    ses.call(
        "POST",
        &format!("/v2/email/contact-lists/{list}/contacts"),
        Some(json!({"EmailAddress": address})),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_of_one_contact_leave_one_winner_worded_the_same_as_a_lone_duplicate() {
    // The rows span the width of the race and the form of the address raced for: a plain
    // address, one carrying a plus tag, and one whose local part is mixed case — on lists
    // whose names differ, so neither the width nor the list can be what the sentence
    // reads.
    for (case, list, address, racers) in [
        ("a pair racing for a plain address", "list-1", "reader@example.com", 2usize),
        (
            "six racing for a tagged address",
            "news_letter-2",
            "reader+weekly@example.com",
            6,
        ),
        (
            "three racing for a mixed-case local part",
            "digest",
            "Reader.One@example.com",
            3,
        ),
    ] {
        let ses = Arc::new(Ses::new());
        let created = ses
            .call(
                "POST",
                "/v2/email/contact-lists",
                Some(json!({"ContactListName": list})),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: seeding the list must succeed, got {} {:?}",
            created.status, created.body
        );

        let mut racing = Vec::new();
        for _ in 0..racers {
            let ses = Arc::clone(&ses);
            let address = address.to_string();
            let list = list.to_string();
            racing.push(tokio::spawn(async move {
                create_contact(&ses, &list, &address).await
            }));
        }
        let mut replies = Vec::new();
        for racer in racing {
            replies.push(racer.await.expect("a racer must not panic"));
        }

        let winners = replies.iter().filter(|reply| reply.status == 200).count();
        assert_eq!(
            winners,
            1,
            "case {case:?}: expected exactly one of {racers} racers to land, got {winners} \
             (statuses {:?})",
            replies
                .iter()
                .map(|reply| reply.status)
                .collect::<Vec<u16>>()
        );

        // Every loser, and a duplicate sent on its own once the race has settled, read
        // the one sentence: the address, the kind of resource, and nothing about the list.
        let expected = format!("{address} already exists in List.");
        let settled = create_contact(&ses, list, address).await;
        for (label, reply) in replies
            .iter()
            .enumerate()
            .filter(|(_, reply)| reply.status != 200)
            .map(|(index, reply)| (format!("loser {index}"), reply))
            .chain([("the settled duplicate".to_string(), &settled)])
            .collect::<Vec<(String, &Reply)>>()
        {
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (400, "AlreadyExistsException"),
                "case {case:?}, {label}: expected 400 AlreadyExistsException, got {} {:?}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.message(),
                expected,
                "case {case:?}, {label}: expected {expected:?}, got {:?}",
                reply.message()
            );
            assert!(
                !reply.message().contains(list),
                "case {case:?}, {label}: the refusal must not name the list, got {:?}",
                reply.message()
            );
        }

        // One winner, so the list holds exactly one contact under the raced address.
        let listed = ses
            .call(
                "POST",
                &format!("/v2/email/contact-lists/{list}/contacts/list"),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            listed.listed("Contacts", "EmailAddress"),
            vec![address.to_string()],
            "case {case:?}: expected the race to leave one contact, got {:?}",
            listed.listed("Contacts", "EmailAddress")
        );
    }
}

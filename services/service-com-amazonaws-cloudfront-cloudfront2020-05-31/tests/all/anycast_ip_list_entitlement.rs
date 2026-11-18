// SPDX-License-Identifier: CC0-1.0

//! What an account may ALLOCATE Anycast static IP lists against, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rule under test is the one CloudFront's refusal of a create states: an
//! Anycast static IP list is a resource the ACCOUNT holds a Service Quotas quota for, so
//!
//! * the number of lists the account may hold is the value that quota is IN FORCE at, and its
//!   AWS default is zero — an account that has not had the quota raised is entitled to none,
//!   so every `CreateAnycastIpList` it makes is refused with the modelled
//!   `EntityLimitExceeded` (HTTP 400) under the sentence that tells the caller to have the
//!   quota raised, and the refused create allocates nothing;
//! * raising that quota through Service Quotas' own `RequestServiceQuotaIncrease` — the
//!   operation an account raises one with — admits exactly as many lists as the raised value,
//!   and the create that would take the account past it is refused again under the same
//!   sentence;
//! * a list the account WAS entitled to is a resource every later read answers from, so it
//!   reads back through `GetAnycastIpList`.
//!
//! The rows span what the rule generalises over: every create the lifecycle suite's table
//! holds (both documented `IpCount` values, every `IpAddressType`, names at both published
//! bounds, tags and IPAM CIDR configurations) under no entitlement, and entitlements of
//! several sizes.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, and the create path, the reader
//! and the quota calls are [`crate::anycast_ip_list_lifecycle`]'s — reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::anycast_ip_list_lifecycle::{
    create_table, get_anycast_ip_list, post_anycast_ip_list, quota_in_force,
    raise_anycast_entitlement, Create, ANYCAST_QUOTA_CODE, ANYCAST_QUOTA_SERVICE_CODE,
};
use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};

/// The sentence CloudFront refuses a create the account's entitlement cannot serve with,
/// spelled out here rather than taken from the crate, so a reworded refusal fails this suite.
/// It names the Service Quotas page of the quota that bounds the resource and nothing from the
/// request.
const ENTITLEMENT_EXCEEDED_MESSAGE: &str =
    "To request a limit increase for your Anycast static IP list, contact AWS Support: \
     https://us-east-1.console.aws.amazon.com/servicequotas/home/services/cloudfront/quotas/\
     L-6A19EDFD";

/// The modelled error code and status of that refusal
/// (`smithy_query(query="operation",operation="CreateAnycastIpList")` reports
/// `EntityLimitExceeded` with `httpError` 400).
const ENTITLEMENT_EXCEEDED_CODE: &str = "EntityLimitExceeded";
const ENTITLEMENT_EXCEEDED_STATUS: u16 = 400;

/// `GET /2020-05-31/anycast-ip-list` — read here only to check what the account holds.
async fn list_anycast_ip_lists(router: &MultiServiceRouter) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/anycast-ip-list"),
        None,
        None,
    )
    .await
}

/// How many `AnycastIpListSummary` elements the listing reports. The page is an
/// `AnycastIpListCollection` whose `Items` carries one `AnycastIpListSummary` per list.
fn listed_count(listing: &Answer) -> usize {
    listing
        .root()
        .at("Items")
        .map(|items| items.children_named("AnycastIpListSummary").len())
        .unwrap_or_default()
}

/// Assert one answer is the entitlement refusal, whole.
fn assert_entitlement_refusal(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, ENTITLEMENT_EXCEEDED_STATUS,
        "case {case}: expected HTTP {ENTITLEMENT_EXCEEDED_STATUS}, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        ENTITLEMENT_EXCEEDED_CODE,
        "case {case}: expected Code {ENTITLEMENT_EXCEEDED_CODE}, got {:?}",
        answer.error_code()
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, ENTITLEMENT_EXCEEDED_MESSAGE,
        "case {case}: expected the limit-increase sentence, got {message:?}"
    );
}

/// RULE: the quota's AWS default is zero, so an account that has not had it raised may
/// allocate NO Anycast static IP list — every create it makes, whatever the create asks for,
/// is the entitlement refusal, and nothing is allocated for it.
#[tokio::test]
async fn an_account_at_the_default_quota_may_allocate_no_list() {
    let router = emulator();

    // The quota the refusal's sentence names is the one this account is read at, and the
    // value in force on it is AWS's default: none.
    let in_force = quota_in_force(&router).await;
    assert_eq!(
        in_force, 0.0,
        "case the untouched account: expected the published default of the \
         {ANYCAST_QUOTA_SERVICE_CODE}/{ANYCAST_QUOTA_CODE} quota to be zero, got {in_force}"
    );

    for create in create_table() {
        let answer = post_anycast_ip_list(&router, &create.xml()).await;
        assert_entitlement_refusal(&answer, create.case);
    }

    // A refused create allocates nothing: the account holds no list at all.
    let listing = list_anycast_ip_lists(&router).await;
    assert_eq!(
        listing.status, 200,
        "case the listing after the refusals: expected HTTP 200, got {} with {}",
        listing.status, listing.raw
    );
    assert_eq!(
        listed_count(&listing),
        0,
        "case the listing after the refusals: expected no Anycast static IP list, got {}",
        listing.raw
    );
}

/// RULE: the entitlement in force is how many lists the account may hold — exactly that many
/// creates are served, the next one is refused under the same sentence, and each list the
/// account was entitled to reads back through `GetAnycastIpList`.
///
/// The rows differ in the SIZE of the entitlement, which is what the rule generalises over.
#[tokio::test]
async fn the_quota_in_force_is_how_many_lists_the_account_may_hold() {
    for entitlement in [1_u32, 3] {
        let router = emulator();
        raise_anycast_entitlement(&router, entitlement).await;

        for index in 0..entitlement {
            let create = Create {
                case: "a create under the raised quota",
                name: format!("entitled-list-{index}"),
                ip_count: 3,
                ip_address_type: None,
                ipam_cidr_configs: Vec::new(),
                tags: Vec::new(),
            };
            let answer = post_anycast_ip_list(&router, &create.xml()).await;
            assert_eq!(
                answer.status, 202,
                "case entitlement {entitlement}, list {index}: expected HTTP 202, got {} with {}",
                answer.status, answer.raw
            );
            // A list the account was entitled to is a stored record every later read answers
            // from.
            let id = answer.root().find_text("Id").unwrap_or_default().to_string();
            let read = get_anycast_ip_list(&router, &id).await;
            assert_eq!(
                read.status, 200,
                "case entitlement {entitlement}, list {index}: expected the list to read back, \
                 got {} with {}",
                read.status, read.raw
            );
            assert_eq!(
                read.root().find_text("Name").unwrap_or_default(),
                create.name,
                "case entitlement {entitlement}, list {index}: expected the submitted Name, got \
                 {}",
                read.raw
            );
        }

        // The create that would take the account past the value in force is refused.
        let beyond = Create {
            case: "one list past the raised quota",
            name: "one-list-too-many".to_string(),
            ip_count: 3,
            ip_address_type: None,
            ipam_cidr_configs: Vec::new(),
            tags: Vec::new(),
        };
        let answer = post_anycast_ip_list(&router, &beyond.xml()).await;
        assert_entitlement_refusal(&answer, &format!("entitlement {entitlement}, one past it"));

        // ... and the lists the account was entitled to are still there.
        let listing = list_anycast_ip_lists(&router).await;
        assert_eq!(
            listed_count(&listing),
            usize::try_from(entitlement).expect("the entitlement fits"),
            "case entitlement {entitlement}: expected the entitled lists to survive the refusal, \
             got {}",
            listing.raw
        );
    }
}

// SPDX-License-Identifier: CC0-1.0

//! What the operations of a managed prefix list answer for a list Amazon Web Services
//! publishes rather than the account creating.
//!
//! "The ID of the owner of the prefix list" (`ManagedPrefixList$OwnerId`) is Amazon Web
//! Services itself for such a list: it is visible to the caller — `DescribeManagedPrefixLists`
//! "describes your managed prefix lists and any Amazon Web Services-managed prefix lists" —
//! but no action of the account reaches it.
//!
//! The rules under test:
//!
//! 1. An action asked of an AWS-managed prefix list is refused with HTTP 400
//!    `UnsupportedOperation` and the one fixed sentence, which names neither the list nor the
//!    action: reporting what references it, revising it, restoring an earlier version of it
//!    and deleting it all answer it.
//! 2. An id nothing the caller can see carries is still the not-found refusal, so the two
//!    refusals are not confused with one another.
//! 3. The same operations answer a list the ACCOUNT created, so the refusal is about the
//!    list's owner and not about the operation.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, unescaped};

/// The emulator the tests drive.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// EC2's sentence for an action asked of a list Amazon Web Services manages. Fixed text with
/// no variable part: it names neither the list nor the action that was asked.
const UNSUPPORTED_MESSAGE: &str = "The action is not supported for an AWS-managed prefix list.";

/// The id of one prefix list the Region publishes, read off the report that lists them.
async fn published_prefix_list_id(router: &MultiServiceRouter, label: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeManagedPrefixLists",
        &super::ec2_query::filters(&[("owner-id", &["AWS"])]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the published lists must be reported, got {status} {body}"
    );
    let parsed = super::wire::document("published lists", &body);
    let wanted = format!("com.amazonaws.{REGION}.{label}");
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .find(|item| {
            super::wire::child_text(*item, "prefixListName").as_deref() == Some(wanted.as_str())
        })
        .and_then(|item| super::wire::child_text(item, "prefixListId"))
        .unwrap_or_else(|| panic!("no published list named {wanted} in {body}"))
}

/// One managed prefix list the ACCOUNT creates, answered as its id.
async fn created_prefix_list_id(router: &MultiServiceRouter) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateManagedPrefixList",
        &[
            ("PrefixListName".to_string(), "team-network".to_string()),
            ("AddressFamily".to_string(), "IPv4".to_string()),
            ("MaxEntries".to_string(), "5".to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the account's own list must be created, got {status} {body}"
    );
    element(&body, "prefixListId").unwrap_or_else(|| panic!("no <prefixListId> in {body}"))
}

/// Rule 1 over every action that reaches one list, for two different published lists: a
/// gateway-service list and a list published for another service, so the refusal is not keyed
/// to one row of the catalog.
#[tokio::test]
async fn an_action_on_a_published_prefix_list_is_unsupported() {
    let router = emulator();
    for label in ["s3", "route53-healthchecks"] {
        let prefix_list_id = published_prefix_list_id(&router, label).await;
        let actions: [(&str, Vec<(String, String)>); 4] = [
            (
                "GetManagedPrefixListAssociations",
                vec![("PrefixListId".to_string(), prefix_list_id.clone())],
            ),
            (
                "ModifyManagedPrefixList",
                vec![
                    ("PrefixListId".to_string(), prefix_list_id.clone()),
                    ("PrefixListName".to_string(), "renamed".to_string()),
                ],
            ),
            (
                "RestoreManagedPrefixListVersion",
                vec![
                    ("PrefixListId".to_string(), prefix_list_id.clone()),
                    ("PreviousVersion".to_string(), "1".to_string()),
                    ("CurrentVersion".to_string(), "1".to_string()),
                ],
            ),
            (
                "DeleteManagedPrefixList",
                vec![("PrefixListId".to_string(), prefix_list_id.clone())],
            ),
        ];

        for (action, params) in actions {
            let case = format!("{label}/{action}");
            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, "UnsupportedOperation",
                "case {case:?}: expected UnsupportedOperation, got {body}"
            );
            let message = unescaped(&error_message(&body));
            assert_eq!(
                message, UNSUPPORTED_MESSAGE,
                "case {case:?}: expected {UNSUPPORTED_MESSAGE:?}, got {message:?}"
            );
            assert!(
                !message.contains(&prefix_list_id),
                "case {case:?}: the sentence names no list, got {message:?}"
            );
        }
    }
}

/// Rule 2: an id nothing carries is the not-found refusal, which the owner refusal above must
/// not have taken over.
#[tokio::test]
async fn an_unknown_prefix_list_id_is_still_not_found() {
    let router = emulator();
    let (status, body) = call(
        &router,
        REGION,
        "GetManagedPrefixListAssociations",
        &[("PrefixListId".to_string(), "pl-0123456789abcdef0".to_string())],
    )
    .await;
    assert_eq!(status, 400, "expected HTTP 400, got {status} {body}");
    let code = error_code(&body);
    assert_eq!(
        code, "InvalidPrefixListID.NotFound",
        "expected the not-found code, got {body}"
    );
    let message = unescaped(&error_message(&body));
    assert_eq!(
        message, "The prefix list (pl-0123456789abcdef0) does not exist.",
        "expected the not-found sentence, got {message:?}"
    );
}

/// Rule 3: the account's OWN list is reported by the same operation, so the refusal above is
/// about who owns the list.
#[tokio::test]
async fn the_accounts_own_prefix_list_reports_its_associations() {
    let router = emulator();
    let prefix_list_id = created_prefix_list_id(&router).await;
    let (status, body) = call(
        &router,
        REGION,
        "GetManagedPrefixListAssociations",
        &[("PrefixListId".to_string(), prefix_list_id.clone())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {prefix_list_id:?}: expected HTTP 200, got {status} {body}"
    );
    assert!(
        !body.contains("UnsupportedOperation"),
        "case {prefix_list_id:?}: the account's own list is not refused, got {body}"
    );
}
